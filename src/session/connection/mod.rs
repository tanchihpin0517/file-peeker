//! One authenticated route to a File Peeker server.
//!
//! The server runs either inside this process or on a remote host over SSH.
//! Only [`RunningServer`] — which process runs it — and [`Route`] — how a stream
//! reaches its loopback port — differ between the two. Channel construction,
//! authentication, the health check, and shutdown ordering are shared.

use std::{io, net::Ipv4Addr, path::Path, time::Duration};

use crate::protocol::{FILE_PEEKER_SERVICE_NAME, SERVER_STARTUP_PREFIX};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    process::Child,
};
use tonic::{
    Request,
    metadata::MetadataValue,
    transport::{Channel, Endpoint, Uri},
};
use tonic_health::pb::{HealthCheckRequest, health_client::HealthClient};
use tower::service_fn;

mod embedded;
pub(crate) mod ssh;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const SERVER_READY_PREFIX: &str = "FILE_PEEKER_SERVER_READY=";
const SERVER_ERROR_PREFIX: &str = "FILE_PEEKER_SERVER_ERROR=";
const ENSURE_SERVER_SCRIPT: &str = include_str!("ensure-server.sh");
const ENSURE_SERVER_HEREDOC: &str = "FILE_PEEKER_ENSURE_SERVER_SCRIPT";

#[derive(Deserialize)]
struct ServerStartupResponse {
    port: u16,
    token: String,
}

/// Information reported by a successfully started server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConnectionInfo {
    pub server_port: u16,
    pub token: String,
}

/// Where a Session's server should run. The request that produces a
/// [`RunningServer`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ServerLocation {
    /// Inside this process, for a local Session.
    Embedded,
    /// On a remote host, launched over SSH.
    Ssh {
        destination: String,
        force_install: bool,
    },
}

/// A server that is up, as the side responsible for stopping it sees it. The
/// result of starting a [`ServerLocation`].
///
/// `SshServer` is boxed because it carries a whole child process's handles and
/// dwarfs the embedded variant.
#[derive(Debug)]
enum RunningServer {
    Embedded(crate::server::ServerHandle),
    Ssh(Box<ssh::SshServer>),
}

/// How one operation stream reaches the server's loopback port.
///
/// Both variants produce a plain `TcpStream`; the remote one just takes a
/// detour through the SSH process's SOCKS5 proxy.
#[derive(Clone, Copy, Debug)]
enum Route {
    Direct { server_port: u16 },
    Socks5 { socks_port: u16, server_port: u16 },
}

impl Route {
    async fn open(self) -> io::Result<TcpStream> {
        match self {
            Self::Direct { server_port } => {
                TcpStream::connect((Ipv4Addr::LOCALHOST, server_port)).await
            }
            Self::Socks5 {
                socks_port,
                server_port,
            } => ssh::open_operation_stream(socks_port, server_port).await,
        }
    }
}

/// An initialized, authenticated route to one managed File Peeker server.
#[derive(Debug)]
pub(crate) struct ServerConnection {
    info: ConnectionInfo,
    channel: Channel,
    /// `None` once the server has been shut down.
    server: Option<RunningServer>,
}

impl ServerConnection {
    /// Starts a server at `location` and connects to it.
    ///
    /// # Errors
    ///
    /// Returns an error when startup, installation, connection, authentication,
    /// or the initial health check fails.
    pub(crate) async fn open(location: ServerLocation) -> io::Result<Self> {
        let started = match location {
            ServerLocation::Embedded => embedded::start().await?,
            ServerLocation::Ssh {
                destination,
                force_install,
            } => ssh::start(Path::new("ssh"), &destination, force_install).await?,
        };
        Self::connected(started).await
    }

    /// Opens a connection to an embedded server that serves `service` instead
    /// of the real filesystem adapter.
    ///
    /// Everything below the service is real — the listener, the bearer token,
    /// the health check, the channel — so a test using this exercises the whole
    /// client stack against scripted protocol responses.
    ///
    /// # Errors
    ///
    /// Returns an error when the server cannot be started or reached.
    #[cfg(test)]
    pub(crate) async fn open_scripted<S>(service: S) -> io::Result<Self>
    where
        S: crate::protocol::v1::file_peeker_server::FilePeeker,
    {
        Self::connected(embedded::start_with(service).await?).await
    }

    /// Connects to a server that has just been started, stopping it again if
    /// the channel or health check fails.
    async fn connected(
        (server, info, route): (RunningServer, ConnectionInfo, Route),
    ) -> io::Result<Self> {
        match Self::connect(&info, route).await {
            Ok(channel) => Ok(Self {
                info,
                channel,
                server: Some(server),
            }),
            Err(error) => {
                abort_server(server);
                Err(error)
            }
        }
    }

    /// Opens an authenticated channel and proves the server is serving.
    ///
    /// The dummy authority keeps every connection — including HTTP/2
    /// reconnects — going through the route's connector.
    async fn connect(info: &ConnectionInfo, route: Route) -> io::Result<Channel> {
        let channel = Endpoint::from_static("http://file-peeker")
            .connect_timeout(CONNECT_TIMEOUT)
            .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
            .keep_alive_timeout(KEEPALIVE_TIMEOUT)
            .keep_alive_while_idle(true)
            .connect_with_connector(service_fn(move |_uri: Uri| async move {
                route.open().await.map(TokioIo::new)
            }))
            .await
            .map_err(io::Error::other)?;

        check_health(channel.clone(), &info.token).await?;
        Ok(channel)
    }

    #[must_use]
    pub(crate) fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    /// Returns a handle to the open channel. Cheap: a `Channel` is refcounted.
    ///
    /// Infallible because `shutdown` consumes the connection, so a caller
    /// cannot hold one that has been stopped. A closed *Session* is rejected
    /// one layer up, where the connection slot is emptied.
    pub(crate) fn channel(&self) -> Channel {
        self.channel.clone()
    }

    pub(crate) fn request<T>(&self, message: T) -> io::Result<Request<T>> {
        authenticated_request(message, &self.info.token)
    }

    /// Gracefully shuts down this managed server.
    ///
    /// Named `shutdown` rather than `close` to keep the server's lifecycle
    /// distinct from `Session::close`, which is what calls it.
    ///
    /// # Errors
    ///
    /// Returns an error when the server exits unsuccessfully or misses the
    /// bounded shutdown deadline.
    pub(crate) async fn shutdown(mut self) -> io::Result<()> {
        let server = self
            .server
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "session is closed"))?;
        match server {
            RunningServer::Embedded(handle) => embedded::shutdown(handle).await,
            RunningServer::Ssh(server) => ssh::shutdown(server).await,
        }
    }
}

impl Drop for ServerConnection {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            abort_server(server);
        }
    }
}

fn abort_server(server: RunningServer) {
    match server {
        RunningServer::Embedded(handle) => handle.abort(),
        RunningServer::Ssh(server) => ssh::abort(server),
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
        // The package spec selects the one merged package; the remote build skips
        // the TUI and CLI dependency trees.
        assert!(ENSURE_SERVER_SCRIPT.contains("\n        file-peeker\n"));
        assert!(ENSURE_SERVER_SCRIPT.contains("--no-default-features --features server"));
        assert!(ENSURE_SERVER_SCRIPT.contains("$HOME/.file-peeker/servers/$server_version"));
        assert!(ENSURE_SERVER_SCRIPT.contains(super::SERVER_READY_PREFIX));
        assert!(ENSURE_SERVER_SCRIPT.contains(super::SERVER_ERROR_PREFIX));
    }
}
