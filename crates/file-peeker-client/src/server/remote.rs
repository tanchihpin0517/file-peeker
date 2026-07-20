use std::{
    io,
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::{AsyncBufRead, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::{Child, ChildStdin, ChildStdout, Command},
};

use super::{
    BufTcpStream, RemoteServerStartup, Server,
    common::{
        authenticate_stream, ensure_server_executable, heartbeat_server, initialize_control,
        read_server_startup, shell_quote, shutdown_server_process, stop_child, stop_server_process,
    },
};

#[derive(Debug)]
pub struct RemoteServer {
    ssh_executable: PathBuf,
    destination: Option<String>,
    socks_port: Option<u16>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    startup: Option<RemoteServerStartup>,
    control: Option<BufTcpStream>,
}

impl Default for RemoteServer {
    fn default() -> Self {
        Self {
            ssh_executable: PathBuf::from("ssh"),
            destination: None,
            socks_port: None,
            child: None,
            stdin: None,
            stdout: None,
            startup: None,
            control: None,
        }
    }
}

impl Server for RemoteServer {
    type ConnectArgument = String;

    async fn connect(&mut self, remote_server: Self::ConnectArgument) -> io::Result<()> {
        tracing::debug!(remote_server = %remote_server, "checking connection state");
        if self.child.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "remote server is already connected",
            ));
        }

        let (port, mut child, stdin, stdout, startup) =
            prepare_remote_process(&self.ssh_executable, &remote_server).await?;
        tracing::debug!(server_port = startup.forward_port, "server started");
        tracing::debug!("opening control stream through SSH proxy");
        let control = async {
            let control = open_operation_stream(port, startup.forward_port).await?;
            let mut control = BufTcpStream::new(control);
            tracing::debug!("control stream opened; initializing control protocol");
            initialize_control(&mut control, &startup.token).await?;
            tracing::debug!("control protocol initialized");
            Ok::<_, io::Error>(control)
        }
        .await;
        let control = match control {
            Ok(control) => control,
            Err(error) => {
                tracing::debug!(%error, "control connection failed; stopping SSH connection");
                drop(stdin);
                drop(stdout);
                stop_child(&mut child).await;
                return Err(error);
            }
        };

        tracing::debug!(remote_server = %remote_server, "server connection ready");
        self.destination = Some(remote_server);
        self.socks_port = Some(port);
        self.child = Some(child);
        self.stdin = Some(stdin);
        self.stdout = Some(stdout);
        self.startup = Some(startup);
        self.control = Some(control);
        Ok(())
    }

    async fn operate(&self) -> io::Result<BufTcpStream> {
        let socks_port = self.socks_port.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "SSH proxy is not connected")
        })?;
        let startup = self
            .startup
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "server is not started"))?;

        let stream = open_operation_stream(socks_port, startup.forward_port).await?;
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

async fn prepare_remote_process(
    ssh_executable: &Path,
    remote_server: &str,
) -> io::Result<(
    u16,
    Child,
    ChildStdin,
    BufReader<ChildStdout>,
    RemoteServerStartup,
)> {
    tracing::debug!(remote_server = %remote_server, "creating SSH connection");
    let (port, mut child, mut stdin, mut stdout) =
        RemoteServer::create_ssh_connection(ssh_executable, remote_server).await?;
    tracing::debug!(socks_port = port, "SSH connection created");
    let startup = async {
        tracing::debug!("ensuring server executable");
        let mut command_output = tokio::io::sink();
        let executable = RemoteServer::get_server_executable(
            &mut stdin,
            &mut stdout,
            false,
            None,
            &mut command_output,
        )
        .await?;
        tracing::debug!(server_executable = %executable, "server executable ready");
        tracing::debug!("starting server executable");
        RemoteServer::start_server(&mut stdin, &mut stdout, &executable).await
    }
    .await;
    match startup {
        Ok(startup) => Ok((port, child, stdin, stdout, startup)),
        Err(error) => {
            drop(stdin);
            drop(stdout);
            stop_child(&mut child).await;
            Err(error)
        }
    }
}

impl RemoteServer {
    /// Starts the SSH process and captures its standard input and output.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the remote server is empty or the SSH
    /// connection cannot be created. The returned tuple contains the selected
    /// SOCKS port, child process, standard input, and standard output.
    pub async fn create_ssh_connection(
        ssh_executable: &Path,
        remote_server: &str,
    ) -> io::Result<(u16, Child, ChildStdin, BufReader<ChildStdout>)> {
        if remote_server.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "remote server is required",
            ));
        }

        let port = available_loopback_port()?;
        let mut child = ssh_command(ssh_executable, port, remote_server).spawn()?;
        let Some(stdin) = child.stdin.take() else {
            stop_child(&mut child).await;
            return Err(io::Error::other(
                "SSH process did not provide its piped standard input",
            ));
        };
        let Some(stdout) = child.stdout.take() else {
            drop(stdin);
            stop_child(&mut child).await;
            return Err(io::Error::other(
                "SSH process did not provide its piped standard output",
            ));
        };

        Ok((port, child, stdin, BufReader::new(stdout)))
    }

    /// Ensures the matching server executable exists on the connected host.
    ///
    /// The command checks the versioned application directory first and uses
    /// Cargo to install the server from crates.io when it is missing.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the command cannot be sent, its response
    /// cannot be read, or installation fails.
    pub async fn get_server_executable(
        server_stdin: &mut (impl AsyncWrite + Unpin),
        server_stdout: &mut (impl AsyncBufRead + Unpin),
        force_install: bool,
        local_source_path: Option<&str>,
        command_output: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<String> {
        ensure_server_executable(
            server_stdin,
            server_stdout,
            force_install,
            local_source_path,
            command_output,
        )
        .await
    }

    /// Starts the server executable and reads its connection information.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the start command cannot be sent or the
    /// server reports invalid startup information.
    pub async fn start_server(
        server_stdin: &mut (impl AsyncWrite + Unpin),
        server_stdout: &mut (impl AsyncBufRead + Unpin),
        server_executable: &str,
    ) -> io::Result<RemoteServerStartup> {
        if server_executable.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "server executable is required",
            ));
        }

        server_stdin
            .write_all(format!("exec {} serve\n", shell_quote(server_executable)).as_bytes())
            .await?;
        server_stdin.flush().await?;

        read_server_startup(server_stdout).await
    }

    #[must_use]
    pub fn remote_server(&self) -> Option<&str> {
        self.destination.as_deref()
    }

    #[must_use]
    pub fn socks_port(&self) -> Option<u16> {
        self.socks_port
    }

    pub fn stdin(&mut self) -> Option<&mut ChildStdin> {
        self.stdin.as_mut()
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    pub fn stdout(&mut self) -> Option<&mut BufReader<ChildStdout>> {
        self.stdout.as_mut()
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

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.control.take();
        stop_server_process(&mut self.stdin, &mut self.stdout, &mut self.child);
    }
}

async fn open_operation_stream(socks_port: u16, server_port: u16) -> io::Result<TcpStream> {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, socks_port)).await?;
    connect_socks5(&mut stream, server_port).await?;
    Ok(stream)
}

fn available_loopback_port() -> io::Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}

async fn connect_socks5<S>(stream: &mut S, server_port: u16) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream.write_all(&[5, 1, 0]).await?;
    let mut greeting = [0_u8; 2];
    stream.read_exact(&mut greeting).await?;
    if greeting != [5, 0] {
        return Err(io::Error::other("SSH proxy rejected SOCKS5 negotiation"));
    }

    let port = server_port.to_be_bytes();
    stream
        .write_all(&[5, 1, 0, 1, 127, 0, 0, 1, port[0], port[1]])
        .await?;

    let mut response = [0_u8; 4];
    stream.read_exact(&mut response).await?;
    if response[0] != 5 {
        return Err(io::Error::other(
            "SSH proxy returned an invalid SOCKS version",
        ));
    }
    if response[1] != 0 {
        return Err(io::Error::other(format!(
            "SSH proxy rejected the server connection with status {}",
            response[1]
        )));
    }

    let address_bytes = match response[3] {
        1 => 4,
        3 => {
            let mut length = [0_u8; 1];
            stream.read_exact(&mut length).await?;
            usize::from(length[0])
        }
        4 => 16,
        address_type => {
            return Err(io::Error::other(format!(
                "SSH proxy returned unknown SOCKS5 address type {address_type}"
            )));
        }
    };
    let mut bound_address_and_port = vec![0_u8; address_bytes + 2];
    stream.read_exact(&mut bound_address_and_port).await?;
    Ok(())
}

fn ssh_command(ssh_executable: &Path, port: u16, remote_server: &str) -> Command {
    let mut command = Command::new(ssh_executable);
    command
        .arg("-T")
        .arg("-D")
        .arg(format!("127.0.0.1:{port}"))
        .arg(remote_server)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    command
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, io::Cursor, path::Path};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, duplex};

    use super::{RemoteServer, RemoteServerStartup, ssh_command};
    use crate::server::{
        Server,
        common::{SERVER_READY_PREFIX, ensure_server_command},
    };

    #[test]
    fn ssh_command_uses_dynamic_forwarding() {
        let command = ssh_command(Path::new("ssh"), 43827, "example.test");
        let command = command.as_std();
        let arguments: Vec<_> = command.get_args().collect();

        assert_eq!(command.get_program(), "ssh");
        assert_eq!(
            arguments,
            [
                OsStr::new("-T"),
                OsStr::new("-D"),
                OsStr::new("127.0.0.1:43827"),
                OsStr::new("example.test"),
            ]
        );
    }

    #[tokio::test]
    async fn get_server_executable_returns_remote_path() {
        let expected = "/home/test/.file-peeker/servers/0.1.0/bin/file-peeker-server";
        let mut stdin = Vec::new();
        let mut stdout = Cursor::new(format!("login banner\n{SERVER_READY_PREFIX}{expected}\n"));
        let mut output = Vec::new();

        let executable =
            RemoteServer::get_server_executable(&mut stdin, &mut stdout, false, None, &mut output)
                .await
                .expect("server executable should be reported");

        assert_eq!(executable, expected);
        assert_eq!(output, b"login banner\n");
        assert_eq!(
            String::from_utf8(stdin).expect("command should be UTF-8"),
            ensure_server_command(false, None)
        );
    }

    #[tokio::test]
    async fn start_server_runs_executable_and_returns_connection_information() {
        let mut stdin = Vec::new();
        let mut stdout = Cursor::new(
            "server output\nFILE_PEEKER_SERVER_STARTUP={\"port\":43827,\"token\":\"test-token\"}\n",
        );

        let startup = RemoteServer::start_server(
            &mut stdin,
            &mut stdout,
            "/home/test/file peeker/bin/file-peeker-server",
        )
        .await
        .expect("server should start");

        assert_eq!(
            startup,
            RemoteServerStartup {
                forward_port: 43827,
                token: "test-token".into(),
            }
        );
        assert_eq!(
            stdin,
            b"exec '/home/test/file peeker/bin/file-peeker-server' serve\n"
        );
    }

    #[tokio::test]
    async fn buffered_output_is_retained_between_ensure_and_start() {
        let executable = "/home/test/.file-peeker/servers/0.1.0/bin/file-peeker-server";
        let mut stdin = Vec::new();
        let mut stdout = Cursor::new(format!(
            "{SERVER_READY_PREFIX}{executable}\nFILE_PEEKER_SERVER_STARTUP={{\"port\":43827,\"token\":\"test-token\"}}\n"
        ));

        let resolved = RemoteServer::get_server_executable(
            &mut stdin,
            &mut stdout,
            false,
            None,
            &mut Vec::new(),
        )
        .await
        .unwrap();
        let startup = RemoteServer::start_server(&mut stdin, &mut stdout, &resolved)
            .await
            .unwrap();

        assert_eq!(resolved, executable);
        assert_eq!(startup.forward_port, 43827);
        assert_eq!(startup.token, "test-token");
    }

    #[tokio::test]
    async fn socks5_handshake_connects_to_remote_loopback_port() {
        let (mut client, mut proxy) = duplex(64);
        let proxy_task = tokio::spawn(async move {
            let mut greeting = [0_u8; 3];
            proxy.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            proxy.write_all(&[5, 0]).await.unwrap();

            let mut request = [0_u8; 10];
            proxy.read_exact(&mut request).await.unwrap();
            assert_eq!(request, [5, 1, 0, 1, 127, 0, 0, 1, 171, 51]);
            proxy
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();
        });

        super::connect_socks5(&mut client, 43827)
            .await
            .expect("SOCKS5 handshake should succeed");
        proxy_task.await.unwrap();
    }

    #[tokio::test]
    async fn operate_requires_connected_server() {
        let error = RemoteServer::default()
            .operate()
            .await
            .expect_err("disconnected server should not operate");

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
    }
}
