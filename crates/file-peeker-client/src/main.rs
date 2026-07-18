use std::{path::PathBuf, time::Instant};

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
    /// List the direct children of a local or remote directory.
    List {
        /// Directory path to list.
        #[arg(value_name = "PATH")]
        path: String,
        /// SSH destination, resolved through the user's SSH configuration.
        #[arg(long, value_name = "SSH_DESTINATION")]
        remote: Option<String>,
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
        Command::List { path, remote } => {
            run_list(path, remote).await?;
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

async fn run_list(path: String, remote: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let target = if let Some(destination) = remote {
        verbose(format!("list: path={path} remote={destination}"));
        SessionTarget::Ssh { destination }
    } else {
        let server = sibling_server()?;
        verbose(format!("list: path={path} server={}", server.display()));
        SessionTarget::Local {
            server_executable_path: server.to_string_lossy().into_owned(),
        }
    };
    let session = Client::new().connect(SessionConfig { target }).await?;
    let listed = async {
        let started = Instant::now();
        let mut batch_count = 0_u64;
        let mut entry_count = 0_u64;
        let listing = session.list(path).await?;
        while let Some(batch) = listing.next_batch().await? {
            batch_count += 1;
            entry_count += batch.len() as u64;
            for entry in batch {
                println!("{}", entry.path);
            }
        }
        Ok::<_, file_peeker_client::FilePeekerError>((entry_count, batch_count, started.elapsed()))
    }
    .await;
    let closed = session.close().await;
    let (entry_count, batch_count, elapsed) = listed?;
    closed?;
    let entries_per_second = if elapsed.is_zero() {
        0
    } else {
        u128::from(entry_count) * 1_000_000_000 / elapsed.as_nanos()
    };
    verbose(format!(
        "list: stats entries={entry_count} batches={batch_count} elapsed_ms={:.3} entries_per_second={entries_per_second}",
        elapsed.as_secs_f64() * 1_000.0,
    ));
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
    fn parses_local_list_path() {
        let cli = Cli::try_parse_from(["file-peeker-client", "list", "/tmp/report drafts"])
            .expect("local list command should parse");
        assert!(matches!(
            cli.command,
            Command::List { path, remote: None } if path == "/tmp/report drafts"
        ));
    }

    #[test]
    fn parses_remote_list_path() {
        let cli = Cli::try_parse_from([
            "file-peeker-client",
            "list",
            "--remote",
            "ntu",
            "/srv/report drafts",
        ])
        .expect("remote list command should parse");
        assert!(matches!(
            cli.command,
            Command::List {
                path,
                remote: Some(destination),
            } if path == "/srv/report drafts" && destination == "ntu"
        ));
    }

    #[test]
    fn list_requires_path() {
        let error =
            Cli::try_parse_from(["file-peeker-client", "list"]).expect_err("path must be required");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn list_remote_requires_destination() {
        let error = Cli::try_parse_from(["file-peeker-client", "list", "--remote"])
            .expect_err("remote destination must be required");
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
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
