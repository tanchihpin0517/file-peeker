use std::{io, path::Path};

use file_peeker_client::connection::remote::{
    create_ssh_connection, get_server_executable, start_server,
};
use tokio::io::AsyncWriteExt as _;

pub async fn run(destination: &str) -> io::Result<()> {
    let (_socks_port, mut child, mut ssh_stdin, mut ssh_stdout) =
        create_ssh_connection(Path::new("ssh"), destination).await?;
    let mut command_output = tokio::io::stdout();
    let executable =
        get_server_executable(&mut ssh_stdin, &mut ssh_stdout, true, &mut command_output).await?;
    tracing::debug!("-------------- start-server --------------");
    let info = start_server(&mut ssh_stdin, &mut ssh_stdout, &executable).await?;

    println!(
        "{}",
        serde_json::json!({
            "port": info.server_port,
            "token": info.token,
        })
    );

    ssh_stdin.flush().await?;
    drop(ssh_stdin);
    drop(ssh_stdout);
    child.wait().await?;
    Ok(())
}
