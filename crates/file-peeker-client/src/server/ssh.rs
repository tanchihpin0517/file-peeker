use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use tokio::{
    process::{Child, Command},
    time::{Instant, sleep, timeout},
};

use super::diagnostics::append_log_bytes;
use super::{CONNECT_RETRY_DELAY, SHUTDOWN_TIMEOUT, STARTUP_TIMEOUT};
use crate::FilePeekerError;

pub(super) const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(300);

pub(super) fn validate_destination(destination: &str) -> Result<(), FilePeekerError> {
    if destination.is_empty() || destination.starts_with('-') {
        return Err(FilePeekerError::ServerStart {
            message: "SSH destination is required and must not begin with `-`".into(),
        });
    }
    Ok(())
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub(super) fn multiplex_arguments(control_socket: &Path) -> Vec<OsString> {
    vec![
        "-S".into(),
        control_socket.as_os_str().to_owned(),
        "-o".into(),
        "ControlMaster=no".into(),
        "-o".into(),
        "BatchMode=yes".into(),
    ]
}

pub(super) async fn wait_for_master(
    master: &mut Child,
    control_socket: &Path,
    destination: &str,
    deadline: Instant,
) -> Result<(), FilePeekerError> {
    loop {
        if let Some(status) = master
            .try_wait()
            .map_err(|error| FilePeekerError::ServerStart {
                message: format!("cannot inspect SSH control master: {error}"),
            })?
        {
            return Err(FilePeekerError::ServerExited {
                message: format!("SSH control master exited with {status}"),
            });
        }
        if Instant::now() >= deadline {
            return Err(FilePeekerError::ServerStart {
                message: format!(
                    "timed out after {} ms waiting for SSH authentication",
                    AUTHENTICATION_TIMEOUT.as_millis()
                ),
            });
        }
        if control_socket.exists() {
            let status = timeout(
                Duration::from_secs(1),
                Command::new("ssh")
                    .arg("-S")
                    .arg(control_socket)
                    .arg("-O")
                    .arg("check")
                    .arg(destination)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status(),
            )
            .await;
            if matches!(status, Ok(Ok(status)) if status.success()) {
                return Ok(());
            }
        }
        sleep(CONNECT_RETRY_DELAY).await;
    }
}

pub(super) async fn query_remote_home(
    control_socket: &Path,
    destination: &str,
    log_path: &Path,
) -> Result<PathBuf, FilePeekerError> {
    let output = timeout(
        STARTUP_TIMEOUT,
        Command::new("ssh")
            .args(multiplex_arguments(control_socket))
            .arg(destination)
            .arg("printf '%s\\n' \"$HOME\"")
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| FilePeekerError::ServerStart {
        message: "timed out querying the remote home directory".into(),
    })?
    .map_err(|error| FilePeekerError::ServerStart {
        message: format!("cannot query the remote home directory: {error}"),
    })?;
    append_log_bytes(log_path, "remote home stderr", &output.stderr);
    if !output.status.success() {
        return Err(FilePeekerError::ServerStart {
            message: format!(
                "cannot query the remote home directory: SSH exited with {}",
                output.status
            ),
        });
    }
    let home = std::str::from_utf8(&output.stdout)
        .map_err(|error| FilePeekerError::ServerStart {
            message: format!("remote home directory is not valid UTF-8: {error}"),
        })?
        .trim_end_matches(['\r', '\n']);
    if home.is_empty() || home.contains('\r') || home.contains('\n') {
        return Err(FilePeekerError::ServerStart {
            message: "remote home directory response was empty or malformed".into(),
        });
    }
    let path = PathBuf::from(home);
    if !path.is_absolute() {
        return Err(FilePeekerError::ServerStart {
            message: "remote HOME must be an absolute path".into(),
        });
    }
    Ok(path)
}

pub(super) async fn change_forward(
    operation: &str,
    control_socket: &Path,
    destination: &str,
    forward: &str,
    log_path: &Path,
) -> Result<(), FilePeekerError> {
    let output = timeout(
        STARTUP_TIMEOUT,
        Command::new("ssh")
            .arg("-S")
            .arg(control_socket)
            .arg("-O")
            .arg(operation)
            .arg("-o")
            .arg("ExitOnForwardFailure=yes")
            .arg("-o")
            .arg("StreamLocalBindUnlink=yes")
            .arg("-L")
            .arg(forward)
            .arg(destination)
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| FilePeekerError::ServerStart {
        message: format!("timed out requesting SSH {operation}"),
    })?
    .map_err(|error| FilePeekerError::ServerStart {
        message: format!("cannot request SSH {operation}: {error}"),
    })?;
    append_log_bytes(log_path, &format!("SSH {operation} stderr"), &output.stderr);
    if output.status.success() {
        Ok(())
    } else {
        Err(FilePeekerError::ServerStart {
            message: format!(
                "SSH {operation} failed with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        })
    }
}

pub(super) async fn request_master_exit(control_socket: &Path, destination: &str, log_path: &Path) {
    let output = timeout(
        SHUTDOWN_TIMEOUT,
        Command::new("ssh")
            .arg("-S")
            .arg(control_socket)
            .arg("-O")
            .arg("exit")
            .arg(destination)
            .stdin(Stdio::null())
            .output(),
    )
    .await;
    if let Ok(Ok(output)) = output {
        append_log_bytes(log_path, "SSH exit stderr", &output.stderr);
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, time::Duration};

    use super::{multiplex_arguments, shell_quote, validate_destination};

    #[test]
    fn authentication_timeout_allows_interaction() {
        assert_eq!(super::AUTHENTICATION_TIMEOUT, Duration::from_secs(300));
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

    #[test]
    fn multiplexed_commands_are_non_interactive() {
        let arguments = multiplex_arguments(Path::new("/tmp/cm.sock"));
        let arguments = arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            [
                "-S",
                "/tmp/cm.sock",
                "-o",
                "ControlMaster=no",
                "-o",
                "BatchMode=yes"
            ]
        );
    }
}
