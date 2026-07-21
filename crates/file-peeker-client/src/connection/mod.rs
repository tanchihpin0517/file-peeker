use std::{io, path::Path};

use tokio::{
    io::{BufReader, BufStream},
    net::TcpStream,
    process::{Child, ChildStdin, ChildStdout},
};

mod common;
pub mod local;
pub mod remote;

use common::{
    authenticate_stream, heartbeat_server, initialize_control, shutdown_server_process, stop_child,
    stop_server_process,
};

pub type BufTcpStream = BufStream<TcpStream>;

/// Information reported by a successfully started server process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionInfo {
    pub server_port: u16,
    pub token: String,
}

/// Configuration used to create a managed connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionConfig {
    Local {
        force_install: bool,
    },
    Remote {
        destination: String,
        force_install: bool,
    },
}

#[derive(Debug)]
pub(super) enum ConnectionRoute {
    Local,
    Ssh { socks_port: u16 },
}

impl ConnectionRoute {
    async fn open_stream(&self, server_port: u16) -> io::Result<TcpStream> {
        match self {
            Self::Local => local::open_local_stream(server_port).await,
            Self::Ssh { socks_port } => {
                remote::open_operation_stream(*socks_port, server_port).await
            }
        }
    }
}

/// An initialized route to one managed File Peeker server process.
#[derive(Debug)]
pub struct Connection {
    route: ConnectionRoute,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    info: ConnectionInfo,
    control: Option<BufTcpStream>,
}

impl Connection {
    /// Creates and initializes a connection for the configured target.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the server cannot be installed, started, or initialized.
    pub async fn from(config: ConnectionConfig) -> io::Result<Self> {
        match config {
            ConnectionConfig::Local { force_install } => {
                let (child, stdin, stdout, info) = local::prepare(force_install).await?;
                tracing::debug!(server_port = info.server_port, "local server started");
                tracing::debug!("opening local control stream");
                let connection =
                    Self::initialize(ConnectionRoute::Local, child, stdin, stdout, info).await;
                if connection.is_err() {
                    tracing::debug!("local control connection failed; process stopped");
                }
                connection
            }
            ConnectionConfig::Remote {
                destination,
                force_install,
            } => {
                let (socks_port, child, stdin, stdout, info) =
                    remote::prepare(Path::new("ssh"), &destination, force_install).await?;
                tracing::debug!(server_port = info.server_port, "remote server started");
                tracing::debug!("opening control stream through SSH proxy");
                let connection = Self::initialize(
                    ConnectionRoute::Ssh { socks_port },
                    child,
                    stdin,
                    stdout,
                    info,
                )
                .await;
                if connection.is_err() {
                    tracing::debug!("remote control connection failed; SSH process stopped");
                } else {
                    tracing::debug!(remote_server = %destination, "remote connection ready");
                }
                connection
            }
        }
    }

    pub(super) async fn initialize(
        route: ConnectionRoute,
        mut child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
        info: ConnectionInfo,
    ) -> io::Result<Self> {
        let control = async {
            let stream = route.open_stream(info.server_port).await?;
            let mut stream = BufTcpStream::new(stream);
            initialize_control(&mut stream, &info.token).await?;
            Ok::<_, io::Error>(stream)
        }
        .await;

        let control = match control {
            Ok(control) => control,
            Err(error) => {
                drop(stdin);
                drop(stdout);
                stop_child(&mut child).await;
                return Err(error);
            }
        };

        Ok(Self {
            route,
            child: Some(child),
            stdin: Some(stdin),
            stdout: Some(stdout),
            info,
            control: Some(control),
        })
    }

    /// Returns the server connection information.
    #[must_use]
    pub fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    /// Opens and authenticates a stream for one file operation.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the stream cannot be opened or authenticated.
    pub async fn open_operation(&self) -> io::Result<BufTcpStream> {
        let stream = self.route.open_stream(self.info.server_port).await?;
        let mut stream = BufTcpStream::new(stream);
        authenticate_stream(&mut stream, &self.info.token).await?;
        Ok(stream)
    }

    /// Checks that the persistent control connection is responsive.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the control stream does not acknowledge the heartbeat.
    pub async fn heartbeat(&mut self) -> io::Result<()> {
        heartbeat_server(self.control.as_mut()).await
    }

    /// Gracefully shuts down the server and releases its owned process.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the shutdown exchange fails.
    pub async fn close(mut self) -> io::Result<()> {
        shutdown_server_process(
            &mut self.control,
            &mut self.stdin,
            &mut self.stdout,
            &mut self.child,
        )
        .await
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.control.take();
        stop_server_process(&mut self.stdin, &mut self.stdout, &mut self.child);
    }
}
