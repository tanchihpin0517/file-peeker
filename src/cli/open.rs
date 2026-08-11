use std::io;

use crate::{Client, SessionTarget};

pub(crate) async fn run(path: &str, remote: Option<&str>) -> io::Result<()> {
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
