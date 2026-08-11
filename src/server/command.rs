//! The `file-peeker-server` command line: one long-lived server process whose
//! stdin is its lifetime lease.

use std::io::Write as _;

use clap::{Parser, Subcommand, ValueEnum};
use tokio::io::{AsyncReadExt as _, stdin};

use super::serve::{self, ServerError};
use crate::protocol::{PROTOCOL_VERSION, SERVER_STARTUP_PREFIX};

#[derive(Debug, Parser)]
#[command(version, about = "File Peeker filesystem server")]
struct Cli {
    #[command(subcommand)]
    command: ServerCommand,
}

#[derive(Debug, Subcommand)]
enum ServerCommand {
    /// Listen on an ephemeral IPv4 loopback gRPC endpoint until stdin closes.
    Serve,
    /// Print server and protocol version information.
    Version {
        #[arg(long, value_enum)]
        format: VersionFormat,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum VersionFormat {
    Json,
}

/// Runs the `file-peeker-server` command line.
///
/// Exits the process with status 1 after reporting a failure to stderr.
///
/// # Panics
///
/// Panics if writing the failure report or the version output to the standard
/// streams fails.
#[tokio::main]
pub async fn run() {
    let result = match Cli::parse().command {
        ServerCommand::Serve => serve_until_stdin_closes().await,
        ServerCommand::Version {
            format: VersionFormat::Json,
        } => {
            print_version_json();
            Ok(())
        }
    };

    if let Err(error) = result {
        eprintln!("file-peeker-server: {error}");
        std::process::exit(1);
    }
}

/// Starts a server, reports where to reach it, and serves until the lease ends.
async fn serve_until_stdin_closes() -> Result<(), ServerError> {
    let handle = serve::start().await?;
    println!(
        "{SERVER_STARTUP_PREFIX}{}",
        serde_json::json!({ "port": handle.port(), "token": handle.token() })
    );
    std::io::stdout().flush()?;

    let lease = wait_for_shutdown().await;
    let shutdown = handle.shutdown().await;
    lease.and(shutdown)
}

async fn wait_for_shutdown() -> Result<(), ServerError> {
    let mut stdin = stdin();
    let mut probe = [0_u8; 1];
    tokio::select! {
        result = stdin.read(&mut probe) => match result? {
            0 => Ok(()),
            _ => Err(ServerError::Protocol {
                message: "server stdin is a lifetime lease and does not accept data".into(),
            }),
        },
        result = termination_signal() => result,
    }
}

#[cfg(unix)]
async fn termination_signal() -> Result<(), ServerError> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = terminate.recv() => Ok(()),
        _ = interrupt.recv() => Ok(()),
    }
}

fn print_version_json() {
    println!(
        r#"{{"server_version":"{}","protocol_versions":[{}]}}"#,
        env!("CARGO_PKG_VERSION"),
        PROTOCOL_VERSION
    );
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{Cli, ServerCommand};

    #[test]
    fn parses_serve_command() {
        let cli = Cli::try_parse_from(["file-peeker-server", "serve"]).unwrap();
        assert!(matches!(cli.command, ServerCommand::Serve));
        Cli::command().debug_assert();
    }
}
