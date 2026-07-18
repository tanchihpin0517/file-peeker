use std::{path::PathBuf, process::Stdio};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::UnixStream,
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, timeout},
};

use super::diagnostics::{add_stderr_context, join, log_event, read};
use super::protocol::{complete_handshake, connect_control};
use super::runtime::SessionDirectory;
use super::{SHUTDOWN_TIMEOUT, STARTUP_TIMEOUT, ServerHandle};
use crate::FilePeekerError;

struct LocalProcess {
    child: Child,
    control: UnixStream,
    stderr_task: JoinHandle<Result<Vec<u8>, std::io::Error>>,
    _endpoint: SessionDirectory,
}

pub(super) async fn start(server_executable_path: String) -> Result<ServerHandle, FilePeekerError> {
    let executable = validate_executable(&server_executable_path)?;
    let endpoint = SessionDirectory::create()?;
    let socket_path = endpoint.socket_path();
    log_event(endpoint.log_path(), "starting local server");

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
    let stderr_task = tokio::spawn(read(
        stderr,
        endpoint.log_path().to_path_buf(),
        "local server stderr",
    ));
    let deadline = Instant::now() + STARTUP_TIMEOUT;

    let mut control = match connect_control(&mut child, &socket_path, deadline).await {
        Ok(stream) => stream,
        Err(error) => return Err(cleanup_startup_failure(child, stderr_task, error).await),
    };
    if let Err(error) = complete_handshake(&mut child, &mut control, deadline).await {
        return Err(cleanup_startup_failure(child, stderr_task, error).await);
    }

    let running = LocalProcess {
        child,
        control,
        stderr_task,
        _endpoint: endpoint,
    };
    Ok(ServerHandle::spawn(
        socket_path,
        move |mut shutdown| async move {
            supervise(running, &mut shutdown).await;
        },
    ))
}

fn validate_executable(server_executable_path: &str) -> Result<PathBuf, FilePeekerError> {
    if server_executable_path.is_empty() {
        return Err(FilePeekerError::ServerStart {
            message: "server executable path is required".into(),
        });
    }
    Ok(PathBuf::from(server_executable_path))
}

async fn cleanup_startup_failure(
    mut child: Child,
    stderr_task: JoinHandle<Result<Vec<u8>, std::io::Error>>,
    error: FilePeekerError,
) -> FilePeekerError {
    let _ = child.kill().await;
    let _ = child.wait().await;
    let stderr = join(stderr_task).await;
    add_stderr_context(error, stderr.as_deref())
}

async fn supervise(running: LocalProcess, shutdown: &mut mpsc::UnboundedReceiver<()>) {
    let LocalProcess {
        mut child,
        mut control,
        stderr_task,
        _endpoint,
    } = running;
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
    let _ = stderr_task.await;
}
