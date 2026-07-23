use std::{io, path::Path, time::Duration};

use file_peeker_server::protocol::FILE_PEEKER_SERVICE_NAME;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
};
use tonic::{
    Request,
    metadata::MetadataValue,
    transport::{Channel, Endpoint, Uri},
};
use tonic_health::pb::{HealthCheckRequest, health_client::HealthClient};
use tower::service_fn;

pub mod remote;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(5);
const SERVER_STARTUP_PREFIX: &str = "FILE_PEEKER_SERVER_STARTUP=";
const SERVER_READY_PREFIX: &str = "FILE_PEEKER_SERVER_READY=";
const SERVER_ERROR_PREFIX: &str = "FILE_PEEKER_SERVER_ERROR=";
const ENSURE_SERVER_SCRIPT: &str = include_str!("ensure-server.sh");
const ENSURE_SERVER_HEREDOC: &str = "FILE_PEEKER_ENSURE_SERVER_SCRIPT";

#[derive(Deserialize)]
struct ServerStartupResponse {
    port: u16,
    token: String,
}

/// Information reported by a successfully started server process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionInfo {
    pub server_port: u16,
    pub token: String,
}

/// An initialized route to one managed File Peeker server process.
#[derive(Debug)]
pub struct RemoteConnection {
    info: ConnectionInfo,
    server: Option<RemoteServer>,
}

#[derive(Debug)]
struct RemoteServer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    channel: Channel,
}

impl RemoteConnection {
    /// Creates and initializes a connection for the configured target.
    ///
    /// # Errors
    ///
    /// Returns an error when installation, startup, connection, authentication,
    /// or the initial health check fails.
    pub async fn from(destination: &str, force_install: bool) -> io::Result<Self> {
        let (socks_port, child, stdin, stdout, info) =
            remote::prepare(Path::new("ssh"), destination, force_install).await?;
        Self::initialize(socks_port, child, stdin, stdout, info).await
    }

    pub(super) async fn initialize(
        socks_port: u16,
        mut child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
        info: ConnectionInfo,
    ) -> io::Result<Self> {
        let endpoint = Endpoint::from_static("http://file-peeker")
            .connect_timeout(CONNECT_TIMEOUT)
            .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
            .keep_alive_timeout(KEEPALIVE_TIMEOUT)
            .keep_alive_while_idle(true);
        let server_port = info.server_port;
        let channel = endpoint
            .connect_with_connector(service_fn(move |_uri: Uri| async move {
                remote::open_operation_stream(socks_port, server_port)
                    .await
                    .map(TokioIo::new)
            }))
            .await
            .map_err(io::Error::other);

        let channel = match channel {
            Ok(channel) => channel,
            Err(error) => {
                drop(stdin);
                drop(stdout);
                stop_child(&mut child).await;
                return Err(error);
            }
        };

        if let Err(error) = check_health(channel.clone(), &info.token).await {
            drop(channel);
            drop(stdin);
            drop(stdout);
            stop_child(&mut child).await;
            return Err(error);
        }

        Ok(Self {
            info,
            server: Some(RemoteServer {
                child,
                stdin,
                stdout,
                channel,
            }),
        })
    }

    #[must_use]
    pub fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    pub(crate) fn channel(&self) -> io::Result<Channel> {
        self.server
            .as_ref()
            .map(|server| server.channel.clone())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "session is closed"))
    }

    pub(crate) fn request<T>(&self, message: T) -> io::Result<Request<T>> {
        authenticated_request(message, &self.info.token)
    }

    /// Gracefully shuts down this managed server through its stdin lease.
    ///
    /// # Errors
    ///
    /// Returns an error when the server exits unsuccessfully or misses the
    /// bounded shutdown deadline.
    pub async fn close(mut self) -> io::Result<()> {
        let server = self
            .server
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "session is closed"))?;
        shutdown_server_process(server).await
    }
}

impl Drop for RemoteConnection {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            stop_server_process(server);
        }
    }
}

async fn read_server_startup(
    server_stdout: &mut (impl AsyncBufRead + Unpin),
) -> io::Result<ConnectionInfo> {
    let startup_json = loop {
        let mut line = String::new();
        if server_stdout.read_line(&mut line).await? == 0 || !line.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed stdout before reporting startup",
            ));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(json) = line.strip_prefix(SERVER_STARTUP_PREFIX) {
            break json.to_owned();
        }
    };
    let startup: ServerStartupResponse = serde_json::from_str(&startup_json).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("server reported invalid startup JSON: {error}"),
        )
    })?;
    if startup.token.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "server reported an empty token",
        ));
    }

    Ok(ConnectionInfo {
        server_port: startup.port,
        token: startup.token,
    })
}

async fn ensure_server_executable(
    server_stdin: &mut (impl AsyncWrite + Unpin),
    server_stdout: &mut (impl AsyncBufRead + Unpin),
    force_install: bool,
    command_output: &mut (impl AsyncWrite + Unpin),
) -> io::Result<String> {
    server_stdin
        .write_all(ensure_server_command(force_install).as_bytes())
        .await?;
    server_stdin.flush().await?;

    loop {
        let mut line = String::new();
        if server_stdout.read_line(&mut line).await? == 0 || !line.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server installer closed stdout before reporting a result",
            ));
        }
        let status_line = line.trim_end_matches(['\r', '\n']);
        if let Some(executable) = status_line.strip_prefix(SERVER_READY_PREFIX) {
            return Ok(executable.to_owned());
        }
        if let Some(message) = status_line.strip_prefix(SERVER_ERROR_PREFIX) {
            return Err(io::Error::other(message.to_owned()));
        }
        command_output.write_all(line.as_bytes()).await?;
        command_output.flush().await?;
    }
}

fn ensure_server_command(force_install: bool) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let force_install = if force_install { "true" } else { "false" };
    format!(
        "sh -s -- '{version}' '{force_install}' <<'{ENSURE_SERVER_HEREDOC}'\n{ENSURE_SERVER_SCRIPT}{ENSURE_SERVER_HEREDOC}\n"
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

async fn stop_child(child: &mut Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

async fn shutdown_server_process(server: RemoteServer) -> io::Result<()> {
    let RemoteServer {
        mut child,
        stdin,
        stdout,
        channel,
    } = server;
    drop(channel);
    drop(stdin);
    drop(stdout);
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) if status.success() => {}
        Ok(Ok(status)) => {
            return Err(io::Error::other(format!(
                "server process exited unsuccessfully: {status}"
            )));
        }
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            stop_child(&mut child).await;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "server did not stop within 5 seconds",
            ));
        }
    }
    Ok(())
}

fn stop_server_process(server: RemoteServer) {
    let RemoteServer {
        mut child,
        stdin,
        stdout,
        channel,
    } = server;
    drop(channel);
    drop(stdin);
    drop(stdout);
    let _ = child.start_kill();
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let _ = child.wait().await;
        });
    }
}

fn authenticated_request<T>(message: T, token: &str) -> io::Result<Request<T>> {
    let mut authorization = MetadataValue::try_from(format!("Bearer {token}"))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    authorization.set_sensitive(true);
    let mut request = Request::new(message);
    request
        .metadata_mut()
        .insert("authorization", authorization);
    Ok(request)
}

async fn check_health(channel: Channel, token: &str) -> io::Result<()> {
    let request = authenticated_request(
        HealthCheckRequest {
            service: FILE_PEEKER_SERVICE_NAME.into(),
        },
        token,
    )?;
    let response = tokio::time::timeout(CONNECT_TIMEOUT, HealthClient::new(channel).check(request))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "gRPC health check timed out"))?
        .map_err(status_error)?
        .into_inner();
    if response.status == tonic_health::pb::health_check_response::ServingStatus::Serving as i32 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "gRPC health check returned status {}",
            response.status
        )))
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn status_error(status: tonic::Status) -> io::Error {
    use tonic::Code;

    let kind = match status.code() {
        Code::NotFound => io::ErrorKind::NotFound,
        Code::PermissionDenied | Code::Unauthenticated => io::ErrorKind::PermissionDenied,
        Code::InvalidArgument => io::ErrorKind::InvalidInput,
        Code::FailedPrecondition => io::ErrorKind::NotADirectory,
        Code::Cancelled => io::ErrorKind::Interrupted,
        Code::DeadlineExceeded => io::ErrorKind::TimedOut,
        Code::Unavailable => io::ErrorKind::ConnectionAborted,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(
        kind,
        format!("gRPC {:?}: {}", status.code(), status.message()),
    )
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        ConnectionInfo, ENSURE_SERVER_SCRIPT, ensure_server_command, read_server_startup,
        stop_child,
    };

    #[tokio::test]
    async fn startup_ignores_output_before_prefixed_result() {
        let mut output = Cursor::new(
            "diagnostic\nFILE_PEEKER_SERVER_STARTUP={\"port\":43827,\"token\":\"test-token\"}\n",
        );

        assert_eq!(
            read_server_startup(&mut output).await.unwrap(),
            ConnectionInfo {
                server_port: 43827,
                token: "test-token".into(),
            }
        );
    }

    #[tokio::test]
    async fn startup_rejects_empty_token() {
        let mut output =
            Cursor::new("FILE_PEEKER_SERVER_STARTUP={\"port\":43827,\"token\":\"\"}\n");

        assert_eq!(
            read_server_startup(&mut output).await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn stopping_child_reaps_process() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("sleep 30").kill_on_drop(true);
        let mut child = command.spawn().unwrap();

        stop_child(&mut child).await;

        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn ensure_command_checks_then_installs_versioned_server() {
        let command = ensure_server_command(false);
        let forced_command = ensure_server_command(true);
        let version = env!("CARGO_PKG_VERSION");

        assert!(command.starts_with(&format!("sh -s -- '{version}' 'false' <<")));
        assert!(forced_command.starts_with(&format!("sh -s -- '{version}' 'true' <<")));
        assert!(command.contains(ENSURE_SERVER_SCRIPT));
        assert!(ENSURE_SERVER_SCRIPT.contains("[ -x \"$server_executable\" ]"));
        assert!(ENSURE_SERVER_SCRIPT.contains("cargo install"));
        assert!(ENSURE_SERVER_SCRIPT.contains("--force"));
        assert!(ENSURE_SERVER_SCRIPT.contains("--root \"$server_root\""));
        assert!(ENSURE_SERVER_SCRIPT.contains("--version \"$server_version\""));
        assert!(ENSURE_SERVER_SCRIPT.contains("--bin file-peeker-server"));
        assert!(
            ENSURE_SERVER_SCRIPT
                .contains("--git https://github.com/tanchihpin0517/file-peeker.git")
        );
        assert!(!ENSURE_SERVER_SCRIPT.contains("--path"));
        assert!(ENSURE_SERVER_SCRIPT.contains("$HOME/.file-peeker/servers/$server_version"));
        assert!(ENSURE_SERVER_SCRIPT.contains(super::SERVER_READY_PREFIX));
        assert!(ENSURE_SERVER_SCRIPT.contains(super::SERVER_ERROR_PREFIX));
    }
}
