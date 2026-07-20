use std::{
    ffi::OsStr,
    io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::BufReader,
    net::TcpStream,
    process::{Child, ChildStdin, ChildStdout, Command},
};

use super::{
    BufTcpStream, RemoteServerStartup, Server,
    common::{
        authenticate_stream, ensure_server_executable, heartbeat_server, initialize_control,
        read_server_startup, shutdown_server_process, stop_child, stop_server_process,
    },
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalServerConfig {
    pub force_install: bool,
    pub local_source_path: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct LocalServer {
    executable: Option<PathBuf>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    startup: Option<RemoteServerStartup>,
    control: Option<BufTcpStream>,
}

impl Server for LocalServer {
    type ConnectArgument = LocalServerConfig;

    async fn connect(&mut self, config: Self::ConnectArgument) -> io::Result<()> {
        tracing::debug!("checking local connection state");
        if self.child.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "local server is already connected",
            ));
        }
        let (server_executable, mut child, stdin, stdout, startup) =
            prepare_local_process(&config).await?;
        let server_executable_display = server_executable.display();
        tracing::debug!(server_port = startup.forward_port, "local server started");

        tracing::debug!("opening local control stream");
        let control = async {
            let control = open_local_stream(startup.forward_port).await?;
            let mut control = BufTcpStream::new(control);
            tracing::debug!("local control stream opened; initializing control protocol");
            initialize_control(&mut control, &startup.token).await?;
            tracing::debug!("control protocol initialized");
            Ok::<_, io::Error>(control)
        }
        .await;
        let control = match control {
            Ok(control) => control,
            Err(error) => {
                tracing::debug!(%error, "local control connection failed; stopping process");
                drop(stdin);
                drop(stdout);
                stop_child(&mut child).await;
                return Err(error);
            }
        };

        tracing::debug!(server_executable = %server_executable_display, "local server connection ready");
        self.executable = Some(server_executable);
        self.child = Some(child);
        self.stdin = Some(stdin);
        self.stdout = Some(stdout);
        self.startup = Some(startup);
        self.control = Some(control);
        Ok(())
    }

    async fn operate(&self) -> io::Result<BufTcpStream> {
        let startup = self
            .startup
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "server is not started"))?;

        let stream = open_local_stream(startup.forward_port).await?;
        let mut stream = BufTcpStream::new(stream);
        authenticate_stream(&mut stream, &startup.token).await?;
        Ok(stream)
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        shutdown_server_process(
            &mut self.control,
            &mut self.stdin,
            &mut self.stdout,
            &mut self.child,
        )
        .await
    }
}

async fn prepare_local_process(
    config: &LocalServerConfig,
) -> io::Result<(
    PathBuf,
    Child,
    ChildStdin,
    BufReader<ChildStdout>,
    RemoteServerStartup,
)> {
    tracing::debug!("ensuring local server executable");
    let server_executable = LocalServer::get_server_executable(config).await?;
    tracing::debug!(server_executable = %server_executable.display(), "local server executable ready");
    tracing::debug!(server_executable = %server_executable.display(), "starting local server process");
    let mut child = local_server_command(&server_executable).spawn()?;
    let Some(stdin) = child.stdin.take() else {
        stop_child(&mut child).await;
        return Err(io::Error::other(
            "server process did not provide its piped standard input",
        ));
    };
    let Some(stdout) = child.stdout.take() else {
        drop(stdin);
        stop_child(&mut child).await;
        return Err(io::Error::other(
            "server process did not provide its piped standard output",
        ));
    };
    let mut stdout = BufReader::new(stdout);
    tracing::debug!("waiting for local server startup result");
    let startup = match read_server_startup(&mut stdout).await {
        Ok(startup) => startup,
        Err(error) => {
            drop(stdin);
            drop(stdout);
            stop_child(&mut child).await;
            return Err(error);
        }
    };
    Ok((server_executable, child, stdin, stdout, startup))
}

impl LocalServer {
    /// Ensures the configured local server executable is installed.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the installer cannot be started, installation
    /// fails, or the executable path cannot be read.
    pub async fn get_server_executable(config: &LocalServerConfig) -> io::Result<PathBuf> {
        let local_source_path = config
            .local_source_path
            .as_deref()
            .map(|path| {
                path.to_str().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "local server source path must be valid UTF-8",
                    )
                })
            })
            .transpose()?;
        let mut command = Command::new("sh");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let Some(mut stdin) = child.stdin.take() else {
            stop_child(&mut child).await;
            return Err(io::Error::other(
                "server installer standard input is unavailable",
            ));
        };
        let Some(stdout) = child.stdout.take() else {
            drop(stdin);
            stop_child(&mut child).await;
            return Err(io::Error::other(
                "server installer standard output is unavailable",
            ));
        };
        let mut command_output = tokio::io::stdout();
        let executable = ensure_server_executable(
            &mut stdin,
            &mut BufReader::new(stdout),
            config.force_install,
            local_source_path,
            &mut command_output,
        )
        .await;
        drop(stdin);

        let executable = match executable {
            Ok(executable) => executable,
            Err(error) => {
                stop_child(&mut child).await;
                return Err(error);
            }
        };
        let status = child.wait().await?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "server installer failed with {status}"
            )));
        }
        Ok(PathBuf::from(executable))
    }

    #[must_use]
    pub fn server_executable(&self) -> Option<&Path> {
        self.executable.as_deref()
    }

    #[must_use]
    pub fn startup(&self) -> Option<&RemoteServerStartup> {
        self.startup.as_ref()
    }

    /// Checks that the persistent control connection is responsive.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the server is not connected or does not
    /// acknowledge the heartbeat.
    pub async fn heartbeat(&mut self) -> io::Result<()> {
        heartbeat_server(self.control.as_mut()).await
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.control.take();
        stop_server_process(&mut self.stdin, &mut self.stdout, &mut self.child);
    }
}

fn local_server_command(server_executable: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(server_executable);
    command
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn open_local_stream(server_port: u16) -> io::Result<TcpStream> {
    TcpStream::connect((Ipv4Addr::LOCALHOST, server_port)).await
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, path::PathBuf};

    use super::{LocalServer, local_server_command};
    use crate::server::Server;

    #[test]
    fn local_server_command_runs_serve() {
        let command = local_server_command("/tmp/file-peeker-server");
        let command = command.as_std();

        assert_eq!(command.get_program(), "/tmp/file-peeker-server");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("serve")]
        );
    }

    #[test]
    fn local_server_command_reports_spawn_failure() {
        local_server_command(PathBuf::from("/file-peeker-test/missing-server"))
            .spawn()
            .expect_err("missing server executable should fail");
    }

    #[tokio::test]
    async fn operate_requires_connected_server() {
        let error = LocalServer::default()
            .operate()
            .await
            .expect_err("disconnected server should not operate");

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
    }
}
