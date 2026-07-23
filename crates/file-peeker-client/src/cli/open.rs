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
    let session_id = client
        .start_session(target)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    let result = match client.get_session(session_id.clone()).await {
        Some(session) => session.op_open_file(path).await,
        None => Err(io::Error::other("started Session was not retained")),
    };
    let shutdown = client
        .close_session(session_id)
        .await
        .map_err(|error| io::Error::other(error.to_string()));
    result?;
    shutdown
}
