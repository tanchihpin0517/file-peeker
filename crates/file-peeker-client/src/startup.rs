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
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep, timeout, timeout_at},
};

use crate::{ClientConfig, ClientError};

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
}

impl LifecycleHandle {
    pub(super) fn shutdown(&self) {
        let _ = self.shutdown.send(());
    }

    pub(super) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

pub(super) async fn start_local(config: ClientConfig) -> Result<LifecycleHandle, ClientError> {
    let executable = validate_config(&config)?;
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

    let mut child = command.spawn().map_err(|error| ClientError::ServerStart {
        message: format!("cannot launch `{}`: {error}", executable.display()),
    })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ClientError::ServerStart {
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
    tokio::spawn(supervise(
        child,
        control,
        stderr_task,
        endpoint,
        shutdown_receiver,
        Arc::clone(&closed),
    ));

    Ok(LifecycleHandle {
        shutdown,
        socket_path,
        closed,
    })
}

fn validate_config(config: &ClientConfig) -> Result<PathBuf, ClientError> {
    if config.server_executable_path.is_empty() {
        return Err(ClientError::ServerStart {
            message: "server executable path is required".into(),
        });
    }
    Ok(PathBuf::from(&config.server_executable_path))
}

fn create_endpoint() -> Result<TempDir, ClientError> {
    let endpoint = tempfile::Builder::new()
        .prefix("fp-")
        .tempdir_in("/tmp")
        .map_err(|error| ClientError::ServerStart {
            message: format!("cannot create private server directory: {error}"),
        })?;
    let mut permissions = std::fs::metadata(endpoint.path())
        .map_err(|error| ClientError::ServerStart {
            message: format!("cannot inspect private server directory: {error}"),
        })?
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(endpoint.path(), permissions).map_err(|error| {
        ClientError::ServerStart {
            message: format!("cannot secure private server directory: {error}"),
        }
    })?;
    Ok(endpoint)
}

fn validate_socket_length(socket_path: &Path) -> Result<(), ClientError> {
    if socket_path.as_os_str().as_bytes().len() > MAX_SOCKET_PATH_BYTES {
        return Err(ClientError::ServerStart {
            message: format!("generated server socket path exceeds {MAX_SOCKET_PATH_BYTES} bytes"),
        });
    }
    Ok(())
}

async fn connect_control(
    child: &mut Child,
    socket_path: &Path,
    deadline: Instant,
) -> Result<UnixStream, ClientError> {
    loop {
        if let Some(status) = child.try_wait().map_err(|error| ClientError::ServerStart {
            message: format!("cannot inspect server process: {error}"),
        })? {
            return Err(server_exited_error(status, None));
        }
        if Instant::now() >= deadline {
            return Err(ClientError::ServerStart {
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
                return Err(ClientError::ServerStart {
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
) -> Result<(), ClientError> {
    let handshake = handshake_control(stream);
    tokio::pin!(handshake);

    tokio::select! {
        result = timeout_at(deadline, &mut handshake) => {
            result.map_err(|_| ClientError::ServerStart {
                message: format!(
                    "timed out after {} ms during the control handshake",
                    STARTUP_TIMEOUT.as_millis()
                ),
            })?
        }
        status = child.wait() => {
            let status = status.map_err(|error| ClientError::ServerStart {
                message: format!("cannot wait for server process: {error}"),
            })?;
            Err(server_exited_error(status, None))
        }
    }
}

async fn handshake_control(stream: &mut UnixStream) -> Result<(), ClientError> {
    let hello = ClientMessage::Hello {
        version: PROTOCOL_VERSION,
        role: ConnectionRole::Control,
    };
    let mut bytes = serde_json::to_vec(&hello).map_err(|error| ClientError::Protocol {
        message: format!("cannot encode control hello: {error}"),
    })?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .await
        .map_err(|error| ClientError::ConnectionClosed {
            message: format!("cannot send control hello: {error}"),
        })?;
    stream
        .flush()
        .await
        .map_err(|error| ClientError::ConnectionClosed {
            message: format!("cannot flush control hello: {error}"),
        })?;

    let response = read_server_message(stream).await?;
    match response {
        ServerMessage::HelloOk { version } if version == PROTOCOL_VERSION => Ok(()),
        ServerMessage::HelloOk { version } => Err(ClientError::Protocol {
            message: format!("server accepted unexpected protocol version {version}"),
        }),
        ServerMessage::Error { code, message } => Err(ClientError::Protocol {
            message: format!("server rejected control handshake ({code:?}): {message}"),
        }),
        response => Err(ClientError::Protocol {
            message: format!("unexpected control handshake response: {response:?}"),
        }),
    }
}

async fn read_server_message(stream: &mut UnixStream) -> Result<ServerMessage, ClientError> {
    let reader = BufReader::new(stream);
    let mut bytes = Vec::new();
    let count = reader
        .take((MAX_MESSAGE_BYTES + 2) as u64)
        .read_until(b'\n', &mut bytes)
        .await
        .map_err(|error| ClientError::ConnectionClosed {
            message: format!("cannot read control handshake: {error}"),
        })?;

    if count == 0 {
        return Err(ClientError::ConnectionClosed {
            message: "server closed during the control handshake".into(),
        });
    }
    if bytes.last() != Some(&b'\n') {
        return Err(ClientError::Protocol {
            message: "server response is not newline terminated".into(),
        });
    }
    bytes.pop();
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(ClientError::Protocol {
            message: "server response exceeds the size limit".into(),
        });
    }

    serde_json::from_slice(&bytes).map_err(|error| ClientError::Protocol {
        message: format!("server returned invalid JSON: {error}"),
    })
}

async fn cleanup_startup_failure(
    mut child: Child,
    stderr_task: JoinHandle<Result<BoundedOutput, std::io::Error>>,
    error: ClientError,
) -> ClientError {
    let _ = child.kill().await;
    let _ = child.wait().await;
    let stderr = join_stderr(stderr_task).await;
    add_stderr_context(error, stderr.as_ref())
}

async fn supervise(
    mut child: Child,
    mut control: UnixStream,
    stderr_task: JoinHandle<Result<BoundedOutput, std::io::Error>>,
    _endpoint: TempDir,
    mut shutdown: mpsc::UnboundedReceiver<()>,
    closed: Arc<AtomicBool>,
) {
    let mut probe = [0_u8; 1];
    tokio::select! {
        _ = shutdown.recv() => {
            let _ = control.shutdown().await;
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

fn add_stderr_context(error: ClientError, stderr: Option<&BoundedOutput>) -> ClientError {
    let Some(stderr) = stderr.filter(|output| !output.bytes.is_empty()) else {
        return error;
    };
    let suffix = format!(
        "; server stderr: {}{}",
        String::from_utf8_lossy(&stderr.bytes).trim(),
        if stderr.truncated { " [truncated]" } else { "" }
    );

    match error {
        ClientError::ServerStart { mut message } => {
            message.push_str(&suffix);
            ClientError::ServerStart { message }
        }
        ClientError::ServerExited { mut message } => {
            message.push_str(&suffix);
            ClientError::ServerExited { message }
        }
        other => other,
    }
}

fn server_exited_error(status: ExitStatus, stderr: Option<&BoundedOutput>) -> ClientError {
    add_stderr_context(
        ClientError::ServerExited {
            message: format!("server exited with {status}"),
        },
        stderr,
    )
}

#[cfg(test)]
mod tests {
    use std::{path::Path, time::Duration};

    use tokio::io::AsyncWriteExt;

    use super::{BoundedOutput, read_bounded, validate_socket_length};

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
}
