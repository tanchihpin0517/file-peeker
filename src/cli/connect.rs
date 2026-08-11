use std::io;

use crate::session::backend::connection::{ConnectionInfo, ServerConnection, ServerLocation};

pub(crate) async fn run(destination: &str, force_install: bool) -> io::Result<()> {
    tracing::debug!("---------------- connect ----------------");

    let connection = ServerConnection::open(ServerLocation::Ssh {
        destination: destination.to_owned(),
        force_install,
    })
    .await?;
    print_startup(connection.info());
    connection.close().await
}

fn print_startup(info: &ConnectionInfo) {
    println!(
        "{}",
        serde_json::json!({
            "port": info.server_port,
            "token": info.token,
        })
    );
}
