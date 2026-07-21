use std::io;

use file_peeker_client::connection::{Connection, ConnectionConfig, ConnectionInfo};

pub async fn run(destination: Option<&str>, force_install: bool) -> io::Result<()> {
    tracing::debug!("---------------- connect ----------------");

    let connection = create_connection(destination, force_install).await?;
    print_startup(connection.info());
    connection.close().await
}

pub(crate) async fn create_connection(
    destination: Option<&str>,
    force_install: bool,
) -> io::Result<Connection> {
    let config = match destination {
        Some(destination) => ConnectionConfig::Remote {
            destination: destination.to_owned(),
            force_install,
        },
        None => ConnectionConfig::Local { force_install },
    };
    Connection::from(config).await
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
