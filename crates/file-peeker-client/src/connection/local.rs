use std::{ffi::OsStr, io, net::Ipv4Addr, path::PathBuf, process::Stdio};

use tokio::{
    io::BufReader,
    net::TcpStream,
    process::{Child, ChildStdin, ChildStdout, Command},
};

use super::{
    ConnectionInfo,
    common::{ensure_server_executable, read_server_startup, stop_child},
};

pub(super) async fn prepare(
    force_install: bool,
) -> io::Result<(Child, ChildStdin, BufReader<ChildStdout>, ConnectionInfo)> {
    tracing::debug!("ensuring local server executable");
    let server_executable = get_server_executable(force_install).await?;
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
    let info = match read_server_startup(&mut stdout).await {
        Ok(info) => info,
        Err(error) => {
            drop(stdin);
            drop(stdout);
            stop_child(&mut child).await;
            return Err(error);
        }
    };
    Ok((child, stdin, stdout, info))
}

/// Ensures the configured local server executable is installed.
///
/// # Errors
///
/// Returns an I/O error when the installer cannot be started, installation
/// fails, or the executable path cannot be read.
pub async fn get_server_executable(force_install: bool) -> io::Result<PathBuf> {
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
        force_install,
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

fn local_server_command(server_executable: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(server_executable);
    command
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    command
}

pub(super) async fn open_local_stream(server_port: u16) -> io::Result<TcpStream> {
    TcpStream::connect((Ipv4Addr::LOCALHOST, server_port)).await
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, path::PathBuf};

    use super::local_server_command;

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
}
