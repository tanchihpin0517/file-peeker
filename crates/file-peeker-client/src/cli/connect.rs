use std::io;

use file_peeker_client::session::backend::connection::{ConnectionInfo, RemoteConnection};

pub async fn run(destination: &str, force_install: bool) -> io::Result<()> {
    tracing::debug!("---------------- connect ----------------");

    let connection = RemoteConnection::from(destination, force_install).await?;
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
