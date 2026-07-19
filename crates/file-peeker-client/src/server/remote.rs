use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::UnixStream,
    process::{Child, ChildStdin, Command},
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};

use super::diagnostics::{add_named_stderr_context, join, log_event, read};
use super::protocol::{complete_handshake, connect_control};
use super::runtime::{SessionDirectory, validate_socket_path};
use super::ssh::{
    AUTHENTICATION_TIMEOUT, change_forward, multiplex_arguments, query_remote_home,
    request_master_exit, shell_quote, validate_destination, wait_for_master,
};
use super::{CONNECT_RETRY_DELAY, SHUTDOWN_TIMEOUT, STARTUP_TIMEOUT, ServerHandle};
use crate::FilePeekerError;
use crate::install::{RemoteInstallConfig, RemoteInstallPolicy, install_remote_server};

const REMOTE_LAUNCH_SCRIPT: &str = include_str!("remote-launch.sh");

struct RemoteProcess {
    master: Child,
    launcher: Child,
    control: UnixStream,
    master_stderr_task: JoinHandle<Result<Vec<u8>, std::io::Error>>,
    launcher_stderr_task: JoinHandle<Result<Vec<u8>, std::io::Error>>,
    _endpoint: SessionDirectory,
    launcher_stdin: Option<ChildStdin>,
    destination: String,
    control_socket: PathBuf,
    forward: String,
    log_path: PathBuf,
}

#[allow(clippy::too_many_lines)]
pub(super) async fn start(destination: String) -> Result<ServerHandle, FilePeekerError> {
    validate_destination(&destination)?;
    let endpoint = SessionDirectory::create()?;
    let socket_path = endpoint.socket_path();
    let control_socket = endpoint.control_socket_path();
    validate_socket_path(&socket_path)?;
    validate_socket_path(&control_socket)?;
    log_event(endpoint.log_path(), "starting SSH control master");

    let mut master = spawn_master(&control_socket, &destination)?;
    let master_stderr = master
        .stderr
        .take()
        .ok_or_else(|| FilePeekerError::ServerStart {
            message: "SSH control master stderr was not available".into(),
        })?;
    let master_stderr_task = tokio::spawn(read(
        master_stderr,
        endpoint.log_path().to_path_buf(),
        "SSH control master stderr",
    ));

    if let Err(error) = wait_for_master(
        &mut master,
        &control_socket,
        &destination,
        Instant::now() + AUTHENTICATION_TIMEOUT,
    )
    .await
    {
        return Err(cleanup_master_startup_failure(master, master_stderr_task, error).await);
    }
    log_event(endpoint.log_path(), "SSH control master is ready");

    let mut install_config = RemoteInstallConfig::for_current_build(
        destination.clone(),
        RemoteInstallPolicy::ReuseExisting,
    );
    install_config.ssh_arguments = multiplex_arguments(&control_socket);
    install_config.log_path = Some(endpoint.log_path().to_path_buf());
    log_event(endpoint.log_path(), "checking remote server installation");
    if let Err(error) = install_remote_server(&install_config).await {
        let error = FilePeekerError::ServerStart {
            message: error.to_string(),
        };
        return Err(cleanup_master_startup_failure(master, master_stderr_task, error).await);
    }

    let remote_home = match query_remote_home(&control_socket, &destination, endpoint.log_path())
        .await
    {
        Ok(path) => path,
        Err(error) => {
            return Err(cleanup_master_startup_failure(master, master_stderr_task, error).await);
        }
    };
    let remote_directory = remote_home
        .join(".file-peeker")
        .join("run")
        .join(endpoint.id());
    let remote_socket_path = remote_directory.join("server.sock");
    validate_socket_path(&remote_socket_path)?;
    let remote_socket = remote_socket_path
        .to_str()
        .ok_or_else(|| FilePeekerError::ServerStart {
            message: "remote server socket path is not valid UTF-8".into(),
        })?
        .to_owned();

    let mut launcher = match spawn_launcher(
        &control_socket,
        &destination,
        build_remote_script(&remote_directory),
    ) {
        Ok(child) => child,
        Err(error) => {
            return Err(cleanup_master_startup_failure(master, master_stderr_task, error).await);
        }
    };
    let launcher_stdin = launcher.stdin.take();
    let launcher_stderr = launcher
        .stderr
        .take()
        .ok_or_else(|| FilePeekerError::ServerStart {
            message: "SSH remote server stderr was not available".into(),
        })?;
    let launcher_stderr_task = tokio::spawn(read(
        launcher_stderr,
        endpoint.log_path().to_path_buf(),
        "SSH remote server stderr",
    ));

    let forward = format!("{}:{remote_socket}", socket_path.display());
    log_event(endpoint.log_path(), "creating StreamLocal forwarding");
    if let Err(error) = change_forward(
        "forward",
        &control_socket,
        &destination,
        &forward,
        endpoint.log_path(),
    )
    .await
    {
        return Err(cleanup_remote_startup_failure(
            master,
            launcher,
            launcher_stdin,
            master_stderr_task,
            launcher_stderr_task,
            &control_socket,
            &destination,
            error,
        )
        .await);
    }

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let control = loop {
        let mut stream = match connect_control(&mut launcher, &socket_path, deadline).await {
            Ok(stream) => stream,
            Err(error) => {
                return Err(cleanup_remote_startup_failure(
                    master,
                    launcher,
                    launcher_stdin,
                    master_stderr_task,
                    launcher_stderr_task,
                    &control_socket,
                    &destination,
                    error,
                )
                .await);
            }
        };
        match complete_handshake(&mut launcher, &mut stream, deadline).await {
            Ok(()) => break stream,
            Err(FilePeekerError::ConnectionClosed { .. }) if Instant::now() < deadline => {
                sleep(CONNECT_RETRY_DELAY).await;
            }
            Err(error) => {
                return Err(cleanup_remote_startup_failure(
                    master,
                    launcher,
                    launcher_stdin,
                    master_stderr_task,
                    launcher_stderr_task,
                    &control_socket,
                    &destination,
                    error,
                )
                .await);
            }
        }
    };
    log_event(endpoint.log_path(), "remote session is ready");

    let log_path = endpoint.log_path().to_path_buf();
    let running = RemoteProcess {
        master,
        launcher,
        control,
        master_stderr_task,
        launcher_stderr_task,
        _endpoint: endpoint,
        launcher_stdin,
        destination,
        control_socket,
        forward,
        log_path,
    };
    Ok(ServerHandle::spawn(
        socket_path,
        move |mut shutdown| async move {
            supervise(running, &mut shutdown).await;
        },
    ))
}

fn spawn_master(control_socket: &Path, destination: &str) -> Result<Child, FilePeekerError> {
    let mut command = Command::new("ssh");
    command
        .arg("-M")
        .arg("-S")
        .arg(control_socket)
        .arg("-o")
        .arg("ControlPersist=no")
        .arg("-N")
        .arg("-T")
        .arg(destination)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
        .spawn()
        .map_err(|error| FilePeekerError::ServerStart {
            message: format!("cannot launch SSH control master: {error}"),
        })
}

fn spawn_launcher(
    control_socket: &Path,
    destination: &str,
    remote_script: String,
) -> Result<Child, FilePeekerError> {
    let mut command = Command::new("ssh");
    command
        .args(multiplex_arguments(control_socket))
        .arg("-T")
        .arg(destination)
        .arg(remote_script)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
        .spawn()
        .map_err(|error| FilePeekerError::ServerStart {
            message: format!("cannot launch SSH remote server: {error}"),
        })
}

fn build_remote_script(remote_directory: &Path) -> String {
    format!(
        "sh -c {} -- {} {} {}",
        shell_quote(REMOTE_LAUNCH_SCRIPT),
        shell_quote(remote_directory.to_string_lossy().as_ref()),
        shell_quote(
            remote_directory
                .parent()
                .expect("remote session directory has a parent")
                .to_string_lossy()
                .as_ref()
        ),
        shell_quote(env!("CARGO_PKG_VERSION"))
    )
}

async fn cleanup_master_startup_failure(
    mut master: Child,
    master_stderr_task: JoinHandle<Result<Vec<u8>, std::io::Error>>,
    error: FilePeekerError,
) -> FilePeekerError {
    let _ = master.kill().await;
    let _ = master.wait().await;
    let stderr = join(master_stderr_task).await;
    add_named_stderr_context(error, "SSH control master", stderr.as_deref())
}

#[allow(clippy::too_many_arguments)]
async fn cleanup_remote_startup_failure(
    mut master: Child,
    mut launcher: Child,
    launcher_stdin: Option<ChildStdin>,
    master_stderr_task: JoinHandle<Result<Vec<u8>, std::io::Error>>,
    launcher_stderr_task: JoinHandle<Result<Vec<u8>, std::io::Error>>,
    control_socket: &Path,
    destination: &str,
    error: FilePeekerError,
) -> FilePeekerError {
    drop(launcher_stdin);
    let _ = launcher.kill().await;
    let _ = launcher.wait().await;
    request_master_exit(control_socket, destination, Path::new("/dev/null")).await;
    let _ = master.kill().await;
    let _ = master.wait().await;
    let launcher_stderr = join(launcher_stderr_task).await;
    let master_stderr = join(master_stderr_task).await;
    let error = add_named_stderr_context(error, "SSH remote server", launcher_stderr.as_deref());
    add_named_stderr_context(error, "SSH control master", master_stderr.as_deref())
}

async fn supervise(running: RemoteProcess, shutdown: &mut mpsc::UnboundedReceiver<()>) {
    let RemoteProcess {
        mut master,
        mut launcher,
        mut control,
        master_stderr_task,
        launcher_stderr_task,
        _endpoint,
        launcher_stdin,
        destination,
        control_socket,
        forward,
        log_path,
    } = running;
    let mut probe = [0_u8; 1];
    tokio::select! {
        _ = shutdown.recv() => {
            log_event(&log_path, "closing remote session");
            let _ = control.shutdown().await;
            drop(launcher_stdin);
            if timeout(SHUTDOWN_TIMEOUT, launcher.wait()).await.is_err() {
                let _ = launcher.kill().await;
                let _ = launcher.wait().await;
            }
        }
        _ = master.wait() => {
            let _ = control.shutdown().await;
            drop(launcher_stdin);
            let _ = launcher.kill().await;
            let _ = launcher.wait().await;
        }
        _ = launcher.wait() => {
            let _ = control.shutdown().await;
        }
        _ = control.read(&mut probe) => {
            drop(launcher_stdin);
            if timeout(SHUTDOWN_TIMEOUT, launcher.wait()).await.is_err() {
                let _ = launcher.kill().await;
                let _ = launcher.wait().await;
            }
        }
    }

    let _ = change_forward("cancel", &control_socket, &destination, &forward, &log_path).await;
    request_master_exit(&control_socket, &destination, &log_path).await;
    if timeout(SHUTDOWN_TIMEOUT, master.wait()).await.is_err() {
        let _ = master.kill().await;
        let _ = master.wait().await;
    }
    let _ = launcher_stderr_task.await;
    let _ = master_stderr_task.await;
    log_event(&log_path, "remote session closed");
}
