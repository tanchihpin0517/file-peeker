use std::{
    io::ErrorKind,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use file_peeker_protocol::{
    ClientMessage, ConnectionRole, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, ServerMessage,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    process::{Child, ChildStdin, Command},
    sync::{Notify, mpsc},
    task::JoinHandle,
    time::{Instant, sleep, timeout, timeout_at},
};

use crate::install::{RemoteInstallConfig, RemoteInstallPolicy, install_remote_server};
use crate::{FilePeekerError, SessionConfig, SessionTarget};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(20);
const STDERR_LIMIT: usize = 64 * 1024;
const MAX_SOCKET_PATH_BYTES: usize = 100;

#[derive(Debug)]
pub(super) struct LifecycleHandle {
    shutdown: mpsc::UnboundedSender<()>,
    socket_path: PathBuf,
    closed: Arc<AtomicBool>,
    closed_notify: Arc<Notify>,
}

struct RunningProcess {
    child: Child,
    control: UnixStream,
    stderr_task: JoinHandle<Result<BoundedOutput, std::io::Error>>,
    _endpoint: TempDir,
    child_stdin: Option<ChildStdin>,
}

impl LifecycleHandle {
    pub(super) fn shutdown(&self) {
        let _ = self.shutdown.send(());
    }

    pub(super) async fn close(&self) -> Result<(), FilePeekerError> {
        if self.is_closed() {
            return Ok(());
        }
        let notified = self.closed_notify.notified();
        self.shutdown();
        if self.is_closed() {
            return Ok(());
        }
        timeout(SHUTDOWN_TIMEOUT + Duration::from_secs(1), notified)
            .await
            .map_err(|_| FilePeekerError::ConnectionClosed {
                message: "timed out waiting for server shutdown".into(),
            })
    }

    pub(super) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

pub(super) async fn start(config: SessionConfig) -> Result<LifecycleHandle, FilePeekerError> {
    match config.target {
        SessionTarget::Local {
            server_executable_path,
        } => start_local(server_executable_path).await,
        SessionTarget::Ssh { destination } => start_remote(destination).await,
    }
}

async fn start_local(server_executable_path: String) -> Result<LifecycleHandle, FilePeekerError> {
    let executable = validate_local_executable(&server_executable_path)?;
    let endpoint = create_endpoint()?;
    let socket_path = endpoint.path().join("server.sock");
    validate_socket_length(&socket_path)?;

    let mut command = Command::new(&executable);
    command
        .arg("serve")
        .arg("--socket")
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .map_err(|error| FilePeekerError::ServerStart {
            message: format!("cannot launch `{}`: {error}", executable.display()),
        })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| FilePeekerError::ServerStart {
            message: "server stderr was not available".into(),
        })?;
    let stderr_task = tokio::spawn(read_bounded(stderr, STDERR_LIMIT));
    let deadline = Instant::now() + STARTUP_TIMEOUT;

    let mut control = match connect_control(&mut child, &socket_path, deadline).await {
        Ok(stream) => stream,
        Err(error) => {
            return Err(cleanup_startup_failure(child, stderr_task, error).await);
        }
    };

    if let Err(error) = complete_handshake(&mut child, &mut control, deadline).await {
        return Err(cleanup_startup_failure(child, stderr_task, error).await);
    }

    let (shutdown, shutdown_receiver) = mpsc::unbounded_channel();
    let closed = Arc::new(AtomicBool::new(false));
    let closed_notify = Arc::new(Notify::new());
    tokio::spawn(supervise(
        RunningProcess {
            child,
            control,
            stderr_task,
            _endpoint: endpoint,
            child_stdin: None,
        },
        shutdown_receiver,
        Arc::clone(&closed),
        Arc::clone(&closed_notify),
    ));

    Ok(LifecycleHandle {
        shutdown,
        socket_path,
        closed,
        closed_notify,
    })
}

pub(super) async fn start_remote(destination: String) -> Result<LifecycleHandle, FilePeekerError> {
    validate_destination(&destination)?;
    install_remote_server(&RemoteInstallConfig::for_current_build(
        destination.clone(),
        RemoteInstallPolicy::ReuseExisting,
    ))
    .await
    .map_err(|error| FilePeekerError::ServerStart {
        message: error.to_string(),
    })?;

    let endpoint = create_endpoint()?;
    let socket_path = endpoint.path().join("server.sock");
    validate_socket_length(&socket_path)?;
    let token = endpoint
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| FilePeekerError::ServerStart {
            message: "generated endpoint name is not valid UTF-8".into(),
        })?;
    let remote_directory = format!("/tmp/{token}-remote");
    let remote_socket = format!("{remote_directory}/server.sock");
    let remote_script = format!(
        "set -eu; runtime={}; mkdir -m 700 \"$runtime\"; server=\"$HOME/.file-peeker/servers/{}/bin/file-peeker-server\"; \"$server\" serve --socket \"$runtime/server.sock\" --remove-parent-on-exit & server_pid=$!; (cat >/dev/null; kill \"$server_pid\" 2>/dev/null || :) & monitor_pid=$!; cleanup() {{ kill \"$server_pid\" \"$monitor_pid\" 2>/dev/null || :; rm -rf \"$runtime\"; }}; trap cleanup EXIT HUP INT TERM; set +e; wait \"$server_pid\"; status=$?; kill \"$monitor_pid\" 2>/dev/null || :; wait \"$monitor_pid\" 2>/dev/null; exit \"$status\"",
        shell_quote(&remote_directory),
        env!("CARGO_PKG_VERSION")
    );
    let forward = format!("{}:{remote_socket}", socket_path.display());
    let mut command = Command::new("ssh");
    command
        .arg("-T")
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-o")
        .arg("StreamLocalBindUnlink=yes")
        .arg("-L")
        .arg(forward)
        .arg(&destination)
        .arg(remote_script)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    start_child(command, endpoint, socket_path, "SSH remote server").await
}

fn validate_destination(destination: &str) -> Result<(), FilePeekerError> {
    if destination.is_empty() || destination.starts_with('-') {
        return Err(FilePeekerError::ServerStart {
            message: "SSH destination is required and must not begin with `-`".into(),
        });
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

async fn start_child(
    mut command: Command,
    endpoint: TempDir,
    socket_path: PathBuf,
    description: &str,
) -> Result<LifecycleHandle, FilePeekerError> {
    let mut child = command
        .spawn()
        .map_err(|error| FilePeekerError::ServerStart {
            message: format!("cannot launch {description}: {error}"),
        })?;
    let child_stdin = child.stdin.take();
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| FilePeekerError::ServerStart {
            message: format!("{description} stderr was not available"),
        })?;
    let stderr_task = tokio::spawn(read_bounded(stderr, STDERR_LIMIT));
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let control = loop {
        let mut stream = match connect_control(&mut child, &socket_path, deadline).await {
            Ok(stream) => stream,
            Err(error) => return Err(cleanup_startup_failure(child, stderr_task, error).await),
        };
        match complete_handshake(&mut child, &mut stream, deadline).await {
            Ok(()) => break stream,
            Err(FilePeekerError::ConnectionClosed { .. }) if Instant::now() < deadline => {
                sleep(CONNECT_RETRY_DELAY).await;
            }
            Err(error) => return Err(cleanup_startup_failure(child, stderr_task, error).await),
        }
    };
    let (shutdown, shutdown_receiver) = mpsc::unbounded_channel();
    let closed = Arc::new(AtomicBool::new(false));
    let closed_notify = Arc::new(Notify::new());
    tokio::spawn(supervise(
        RunningProcess {
            child,
            control,
            stderr_task,
            _endpoint: endpoint,
            child_stdin,
        },
        shutdown_receiver,
        Arc::clone(&closed),
        Arc::clone(&closed_notify),
    ));
    Ok(LifecycleHandle {
        shutdown,
        socket_path,
        closed,
        closed_notify,
    })
}

fn validate_local_executable(server_executable_path: &str) -> Result<PathBuf, FilePeekerError> {
    if server_executable_path.is_empty() {
        return Err(FilePeekerError::ServerStart {
            message: "server executable path is required".into(),
        });
    }
    Ok(PathBuf::from(server_executable_path))
}

fn create_endpoint() -> Result<TempDir, FilePeekerError> {
    let endpoint = tempfile::Builder::new()
        .prefix("fp-")
        .tempdir_in("/tmp")
        .map_err(|error| FilePeekerError::ServerStart {
            message: format!("cannot create private server directory: {error}"),
        })?;
    let mut permissions = std::fs::metadata(endpoint.path())
        .map_err(|error| FilePeekerError::ServerStart {
            message: format!("cannot inspect private server directory: {error}"),
        })?
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(endpoint.path(), permissions).map_err(|error| {
        FilePeekerError::ServerStart {
            message: format!("cannot secure private server directory: {error}"),
        }
    })?;
    Ok(endpoint)
}

fn validate_socket_length(socket_path: &Path) -> Result<(), FilePeekerError> {
    if socket_path.as_os_str().as_bytes().len() > MAX_SOCKET_PATH_BYTES {
        return Err(FilePeekerError::ServerStart {
            message: format!("generated server socket path exceeds {MAX_SOCKET_PATH_BYTES} bytes"),
        });
    }
    Ok(())
}

async fn connect_control(
    child: &mut Child,
    socket_path: &Path,
    deadline: Instant,
) -> Result<UnixStream, FilePeekerError> {
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| FilePeekerError::ServerStart {
                message: format!("cannot inspect server process: {error}"),
            })?
        {
            return Err(server_exited_error(status, None));
        }
        if Instant::now() >= deadline {
            return Err(FilePeekerError::ServerStart {
                message: format!(
                    "timed out after {} ms waiting for the server socket",
                    STARTUP_TIMEOUT.as_millis()
                ),
            });
        }

        match UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::ConnectionRefused
                ) =>
            {
                sleep(CONNECT_RETRY_DELAY).await;
            }
            Err(error) => {
                return Err(FilePeekerError::ServerStart {
                    message: format!("cannot connect to server socket: {error}"),
                });
            }
        }
    }
}

async fn complete_handshake(
    child: &mut Child,
    stream: &mut UnixStream,
    deadline: Instant,
) -> Result<(), FilePeekerError> {
    let handshake = handshake_control(stream);
    tokio::pin!(handshake);

    tokio::select! {
        result = timeout_at(deadline, &mut handshake) => {
            result.map_err(|_| FilePeekerError::ServerStart {
                message: format!(
                    "timed out after {} ms during the control handshake",
                    STARTUP_TIMEOUT.as_millis()
                ),
            })?
        }
        status = child.wait() => {
            let status = status.map_err(|error| FilePeekerError::ServerStart {
                message: format!("cannot wait for server process: {error}"),
            })?;
            Err(server_exited_error(status, None))
        }
    }
}

async fn handshake_control(stream: &mut UnixStream) -> Result<(), FilePeekerError> {
    let hello = ClientMessage::Hello {
        version: PROTOCOL_VERSION,
        role: ConnectionRole::Control,
    };
    let mut bytes = serde_json::to_vec(&hello).map_err(|error| FilePeekerError::Protocol {
        message: format!("cannot encode control hello: {error}"),
    })?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .await
        .map_err(|error| FilePeekerError::ConnectionClosed {
            message: format!("cannot send control hello: {error}"),
        })?;
    stream
        .flush()
        .await
        .map_err(|error| FilePeekerError::ConnectionClosed {
            message: format!("cannot flush control hello: {error}"),
        })?;

    let response = read_server_message(stream).await?;
    match response {
        ServerMessage::HelloOk { version } if version == PROTOCOL_VERSION => Ok(()),
        ServerMessage::HelloOk { version } => Err(FilePeekerError::Protocol {
            message: format!("server accepted unexpected protocol version {version}"),
        }),
        ServerMessage::Error { code, message } => Err(FilePeekerError::Protocol {
            message: format!("server rejected control handshake ({code:?}): {message}"),
        }),
        response => Err(FilePeekerError::Protocol {
            message: format!("unexpected control handshake response: {response:?}"),
        }),
    }
}

async fn read_server_message(stream: &mut UnixStream) -> Result<ServerMessage, FilePeekerError> {
    let reader = BufReader::new(stream);
    let mut bytes = Vec::new();
    let count = reader
        .take((MAX_MESSAGE_BYTES + 2) as u64)
        .read_until(b'\n', &mut bytes)
        .await
        .map_err(|error| FilePeekerError::ConnectionClosed {
            message: format!("cannot read control handshake: {error}"),
        })?;

    if count == 0 {
        return Err(FilePeekerError::ConnectionClosed {
            message: "server closed during the control handshake".into(),
        });
    }
    if bytes.last() != Some(&b'\n') {
        return Err(FilePeekerError::Protocol {
            message: "server response is not newline terminated".into(),
        });
    }
    bytes.pop();
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(FilePeekerError::Protocol {
            message: "server response exceeds the size limit".into(),
        });
    }

    serde_json::from_slice(&bytes).map_err(|error| FilePeekerError::Protocol {
        message: format!("server returned invalid JSON: {error}"),
    })
}

async fn cleanup_startup_failure(
    mut child: Child,
    stderr_task: JoinHandle<Result<BoundedOutput, std::io::Error>>,
    error: FilePeekerError,
) -> FilePeekerError {
    let _ = child.kill().await;
    let _ = child.wait().await;
    let stderr = join_stderr(stderr_task).await;
    add_stderr_context(error, stderr.as_ref())
}

async fn supervise(
    running: RunningProcess,
    mut shutdown: mpsc::UnboundedReceiver<()>,
    closed: Arc<AtomicBool>,
    closed_notify: Arc<Notify>,
) {
    let RunningProcess {
        mut child,
        mut control,
        stderr_task,
        _endpoint,
        child_stdin,
    } = running;
    let mut probe = [0_u8; 1];
    tokio::select! {
        _ = shutdown.recv() => {
            let _ = control.shutdown().await;
            drop(child_stdin);
            if timeout(SHUTDOWN_TIMEOUT, child.wait()).await.is_err() {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
        _ = child.wait() => {}
        _ = control.read(&mut probe) => {
            if timeout(SHUTDOWN_TIMEOUT, child.wait()).await.is_err() {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
    }
    closed.store(true, Ordering::Release);
    closed_notify.notify_waiters();
    let _ = stderr_task.await;
}

#[derive(Debug)]
struct BoundedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<BoundedOutput, std::io::Error> {
    let mut bytes = Vec::with_capacity(limit.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    let mut truncated = false;

    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = remaining.min(count);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained < count;
    }

    Ok(BoundedOutput { bytes, truncated })
}

async fn join_stderr(
    task: JoinHandle<Result<BoundedOutput, std::io::Error>>,
) -> Option<BoundedOutput> {
    task.await.ok().and_then(Result::ok)
}

fn add_stderr_context(error: FilePeekerError, stderr: Option<&BoundedOutput>) -> FilePeekerError {
    let Some(stderr) = stderr.filter(|output| !output.bytes.is_empty()) else {
        return error;
    };
    let suffix = format!(
        "; server stderr: {}{}",
        String::from_utf8_lossy(&stderr.bytes).trim(),
        if stderr.truncated { " [truncated]" } else { "" }
    );

    match error {
        FilePeekerError::ServerStart { mut message } => {
            message.push_str(&suffix);
            FilePeekerError::ServerStart { message }
        }
        FilePeekerError::ServerExited { mut message } => {
            message.push_str(&suffix);
            FilePeekerError::ServerExited { message }
        }
        other => other,
    }
}

fn server_exited_error(status: ExitStatus, stderr: Option<&BoundedOutput>) -> FilePeekerError {
    add_stderr_context(
        FilePeekerError::ServerExited {
            message: format!("server exited with {status}"),
        },
        stderr,
    )
}

#[cfg(test)]
mod tests {
    use std::{path::Path, time::Duration};

    use tokio::io::AsyncWriteExt;

    use super::{
        BoundedOutput, read_bounded, shell_quote, validate_destination, validate_socket_length,
    };

    #[test]
    fn rejects_an_overlong_socket_path() {
        let path = format!("/tmp/{}", "x".repeat(110));
        assert!(validate_socket_length(Path::new(&path)).is_err());
    }

    #[tokio::test]
    async fn bounded_reader_discards_excess_output() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let task = tokio::spawn(async move { read_bounded(reader, 4).await });
        writer
            .write_all(b"abcdefgh")
            .await
            .expect("fixture output should be written");
        drop(writer);

        let BoundedOutput { bytes, truncated } = task
            .await
            .expect("reader task should finish")
            .expect("reader should succeed");
        assert_eq!(bytes, b"abcd");
        assert!(truncated);
    }

    #[test]
    fn lifecycle_timeouts_are_bounded() {
        assert!(super::STARTUP_TIMEOUT <= Duration::from_secs(10));
        assert!(super::SHUTDOWN_TIMEOUT <= Duration::from_secs(5));
    }

    #[test]
    fn requires_a_safe_ssh_destination() {
        assert!(validate_destination("").is_err());
        assert!(validate_destination("-oProxyCommand=bad").is_err());
        assert!(validate_destination("ntu").is_ok());
    }

    #[test]
    fn quotes_remote_shell_values() {
        assert_eq!(shell_quote("a b'c"), "'a b'\"'\"'c'");
    }
}
