use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
enum HostError {
    #[error("operation is not implemented: {operation}")]
    NotImplemented { operation: &'static str },
}

#[derive(Debug)]
struct HostConfig {
    socket_path: PathBuf,
}

#[tokio::main]
async fn main() {
    let result: Result<(), HostError> = parse_config().and_then(|config| {
        let _ = config.socket_path;
        Err(HostError::NotImplemented {
            operation: "host runtime",
        })
    });

    if let Err(error) = result {
        eprintln!("file-peeker-host: {error}");
        std::process::exit(1);
    }
}

fn parse_config() -> Result<HostConfig, HostError> {
    let socket_path =
        std::env::args_os()
            .nth(1)
            .map(PathBuf::from)
            .ok_or(HostError::NotImplemented {
                operation: "host argument parsing",
            })?;

    Ok(HostConfig { socket_path })
}

#[allow(dead_code, clippy::unused_async)]
async fn accept_control_connection() -> Result<(), HostError> {
    Err(HostError::NotImplemented {
        operation: "control connection",
    })
}

#[allow(dead_code, clippy::unused_async)]
async fn handle_listing() -> Result<(), HostError> {
    Err(HostError::NotImplemented {
        operation: "directory listing",
    })
}

#[allow(dead_code, clippy::unused_async)]
async fn handle_metadata() -> Result<(), HostError> {
    Err(HostError::NotImplemented {
        operation: "metadata",
    })
}
