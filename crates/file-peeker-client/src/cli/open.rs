use std::io;

use file_peeker_client::{Client, SessionTarget};

pub async fn run(path: &str, remote: Option<&str>) -> io::Result<()> {
    tracing::debug!("---------------- open ----------------");
    let target = match remote {
        Some(destination) => SessionTarget::Remote {
            destination: destination.to_owned(),
        },
        None => SessionTarget::Local,
    };
    let client = Client::new();
    let session = client
        .start_session(target)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    let result = session.op_open_file(path).await;
    let shutdown = client
        .close_all()
        .await
        .map_err(|error| io::Error::other(error.to_string()));
    result?;
    shutdown
}
