use std::{io, path::Path};

use file_peeker_client::server::RemoteServer;
use tokio::io::AsyncWriteExt as _;

use super::install::{DEFAULT_REMOTE_PROJECT_DIR, upload_current_project};

pub async fn run(destination: &str) -> io::Result<()> {
    upload_current_project(destination, DEFAULT_REMOTE_PROJECT_DIR).await?;

    let (_socks_port, mut child, mut ssh_stdin, mut ssh_stdout) =
        RemoteServer::create_ssh_connection(Path::new("ssh"), destination).await?;
    let mut command_output = tokio::io::stdout();
    let executable = RemoteServer::get_server_executable(
        &mut ssh_stdin,
        &mut ssh_stdout,
        true,
        Some(DEFAULT_REMOTE_PROJECT_DIR),
        &mut command_output,
    )
    .await?;
    tracing::debug!("-------------- start-server --------------");
    let startup = RemoteServer::start_server(&mut ssh_stdin, &mut ssh_stdout, &executable).await?;

    println!(
        "{}",
        serde_json::json!({
            "port": startup.forward_port,
            "token": startup.token,
        })
    );

    ssh_stdin.flush().await?;
    drop(ssh_stdin);
    drop(ssh_stdout);
    child.wait().await?;
    Ok(())
}
