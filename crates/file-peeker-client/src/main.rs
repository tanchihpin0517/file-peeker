use std::path::PathBuf;

use clap::{Parser, Subcommand};
use file_peeker_client::{Client, SessionConfig, SessionTarget};

#[allow(dead_code)]
mod install;

use install::{RemoteInstallConfig, RemoteInstallPolicy, install_remote_server};

#[derive(Debug, Parser)]
#[command(version, about = "File Peeker client diagnostics")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Connect to a remote server, print its current root, and disconnect.
    Connect {
        /// SSH destination, resolved through the user's SSH configuration.
        #[arg(value_name = "SSH_DESTINATION")]
        destination: String,
    },
    /// Install or overwrite the server on an SSH destination.
    Install {
        /// SSH destination, resolved through the user's SSH configuration.
        #[arg(value_name = "SSH_DESTINATION")]
        destination: String,
    },
    /// Open a local path with the system default application.
    Open {
        /// Local path to open.
        #[arg(value_name = "PATH")]
        path: String,
    },
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("file-peeker-client: {error}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Connect { destination } => {
            verbose(format!(
                "connect: destination={destination} source={} version={}",
                install_source(),
                env!("CARGO_PKG_VERSION")
            ));
            verbose("connect: checking remote installation and installing only if needed");
            verbose("connect: opening SSH transport");
            let session = Client::new()
                .connect(SessionConfig {
                    target: SessionTarget::Ssh { destination },
                })
                .await?;
            verbose("connect: control handshake completed");
            verbose("connect: requesting remote current root");
            let root = session.current_root().await;
            if let Ok(path) = &root {
                verbose(format!("connect: remote current root={path}"));
            }
            verbose("connect: closing control connection and SSH transport");
            let closed = session.close().await;
            let root = root?;
            closed?;
            verbose("connect: shutdown completed");
            println!("{root}");
        }
        Command::Install { destination } => {
            verbose(format!(
                "install: destination={destination} source={} version={} policy=overwrite",
                install_source(),
                env!("CARGO_PKG_VERSION")
            ));
            if cfg!(debug_assertions) {
                verbose("install: packaging and transferring workspace crates");
            } else {
                verbose("install: installing official package from crates.io");
            }
            install_remote_server(&RemoteInstallConfig::for_current_build(
                destination,
                RemoteInstallPolicy::Overwrite,
            ))
            .await?;
            let path = format!(
                "~/.file-peeker/servers/{}/bin/file-peeker-server",
                env!("CARGO_PKG_VERSION")
            );
            verbose(format!("install: installation verified path={path}"));
            if cfg!(debug_assertions) {
                verbose("install: remote package fixture cleanup completed");
            }
            println!("{path}");
        }
        Command::Open { path } => {
            let server = sibling_server()?;
            verbose(format!("open: path={path} server={}", server.display()));
            let session = Client::new()
                .connect(SessionConfig {
                    target: SessionTarget::Local {
                        server_executable_path: server.to_string_lossy().into_owned(),
                    },
                })
                .await?;
            let opened = session.open(path).await;
            let closed = session.close().await;
            opened?;
            closed?;
            verbose("open: system application accepted the path");
        }
    }
    Ok(())
}

fn sibling_server() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    Ok(executable
        .parent()
        .ok_or("client executable has no parent directory")?
        .join("file-peeker-server"))
}

fn install_source() -> &'static str {
    if cfg!(debug_assertions) {
        "workspace"
    } else {
        "crates.io"
    }
}

fn verbose(message: impl std::fmt::Display) {
    eprintln!("[file-peeker-client] {message}");
}

#[cfg(test)]
mod tests {
    use clap::{Parser, error::ErrorKind};

    use super::{Cli, Command};

    #[test]
    fn parses_connect_destination() {
        let cli = Cli::try_parse_from(["file-peeker-client", "connect", "ntu"])
            .expect("connect command should parse");
        assert!(matches!(
            cli.command,
            Command::Connect { destination } if destination == "ntu"
        ));
    }

    #[test]
    fn connect_requires_destination() {
        let error = Cli::try_parse_from(["file-peeker-client", "connect"])
            .expect_err("destination must be required");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn parses_install_destination() {
        let cli = Cli::try_parse_from(["file-peeker-client", "install", "ntu"])
            .expect("install command should parse");
        assert!(matches!(
            cli.command,
            Command::Install { destination } if destination == "ntu"
        ));
    }

    #[test]
    fn install_requires_destination() {
        let error = Cli::try_parse_from(["file-peeker-client", "install"])
            .expect_err("destination must be required");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn parses_open_path() {
        let cli = Cli::try_parse_from(["file-peeker-client", "open", "/tmp/report draft.txt"])
            .expect("open command should parse");
        assert!(matches!(
            cli.command,
            Command::Open { path } if path == "/tmp/report draft.txt"
        ));
    }

    #[test]
    fn open_requires_path() {
        let error =
            Cli::try_parse_from(["file-peeker-client", "open"]).expect_err("path must be required");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }
}
