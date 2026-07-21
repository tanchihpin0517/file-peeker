use std::{io, path::Path};

use file_peeker_client::connection::remote::create_ssh_connection;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

pub async fn run(destination: &str) -> io::Result<()> {
    let (_port, mut child, mut ssh_stdin, mut ssh_stdout) =
        create_ssh_connection(Path::new("ssh"), destination).await?;
    let destination = destination.replace('\'', "'\"'\"'");
    ssh_stdin
        .write_all(format!("echo 'Connect to {destination}'\nexit\n").as_bytes())
        .await?;
    ssh_stdin.flush().await?;
    drop(ssh_stdin);

    let mut output = String::new();
    ssh_stdout.read_to_string(&mut output).await?;
    child.wait().await?;
    if let Some(last_line) = output.lines().last() {
        println!("{last_line}");
    }
    Ok(())
}
