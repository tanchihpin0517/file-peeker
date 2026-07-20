use std::io;

use file_peeker_client::server::{
    LocalServer, LocalServerConfig, RemoteServer, RemoteServerStartup, Server,
};

use super::install::{DEFAULT_REMOTE_PROJECT_DIR, project_dir, upload_and_install};

pub async fn run(destination: Option<&str>) -> io::Result<()> {
    tracing::debug!("---------------- connect ----------------");

    match destination {
        Some(destination) => connect_remote(destination).await,
        None => connect_local().await,
    }
}

async fn connect_local() -> io::Result<()> {
    let mut server = connect_local_server().await?;
    print_startup(server.startup())?;
    server.shutdown().await
}

pub(crate) async fn connect_local_server() -> io::Result<LocalServer> {
    let mut server = LocalServer::default();
    server
        .connect(LocalServerConfig {
            force_install: true,
            local_source_path: Some(project_dir().to_path_buf()),
        })
        .await?;
    Ok(server)
}

async fn connect_remote(destination: &str) -> io::Result<()> {
    upload_and_install(destination, true, DEFAULT_REMOTE_PROJECT_DIR).await?;
    let mut server = RemoteServer::default();
    server.connect(destination.to_owned()).await?;
    print_startup(server.startup())?;
    server.shutdown().await
}

fn print_startup(startup: Option<&RemoteServerStartup>) -> io::Result<()> {
    let startup =
        startup.ok_or_else(|| io::Error::other("server connected without startup information"))?;

    println!(
        "{}",
        serde_json::json!({
            "port": startup.forward_port,
            "token": startup.token,
        })
    );
    Ok(())
}
