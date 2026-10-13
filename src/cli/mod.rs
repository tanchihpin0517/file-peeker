//! `fpk`'s command line: which role to run, and the commands that are neither
//! the server nor the terminal UI.

use std::{
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use clap::{Parser, Subcommand};

use crate::{
    client::{
        self,
        api::{self, ApiClient, ApiClientError},
    },
    home::Home,
    protocol::{CLIENT_PROTOCOL, VERSION, api::API_GENERATION},
};

#[derive(Debug, Parser)]
#[command(
    name = "fpk",
    version,
    about = "File Peeker v2: browse files through a persistent server",
    long_about = "File Peeker v2: browse files through a persistent server.\n\n\
                  With no command, opens the terminal UI attached to this machine's \
                  server, starting one if none is running. The server keeps running \
                  after the UI exits."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show this client's version and the server's status.
    Status,
    /// Run as the server in the foreground, or control the running one.
    Server {
        #[command(subcommand)]
        action: Option<ServerAction>,
    },
}

#[derive(Debug, Subcommand)]
enum ServerAction {
    /// Start the server in the background if none is running, and show its status.
    Start,
    /// Stop the running server through its API socket.
    Stop,
}

/// Which role this process runs.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Invocation {
    Server,
    Tui,
    Client(ClientCommand),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClientCommand {
    Start,
    Status,
    Stop,
}

pub(crate) fn parse() -> Invocation {
    invocation(&Cli::parse())
}

fn invocation(cli: &Cli) -> Invocation {
    match &cli.command {
        None => Invocation::Tui,
        Some(Command::Status) => Invocation::Client(ClientCommand::Status),
        Some(Command::Server { action: None }) => Invocation::Server,
        Some(Command::Server {
            action: Some(ServerAction::Start),
        }) => Invocation::Client(ClientCommand::Start),
        Some(Command::Server {
            action: Some(ServerAction::Stop),
        }) => Invocation::Client(ClientCommand::Stop),
    }
}

pub(crate) fn execute(home: &Home, command: ClientCommand) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("fpk: cannot start the runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = runtime.block_on(async {
        match command {
            ClientCommand::Start => start(home).await,
            ClientCommand::Status => status(home).await,
            ClientCommand::Stop => stop(home).await,
        }
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("fpk: {message}");
            ExitCode::FAILURE
        }
    }
}

/// What a script runs in place of the terminal UI: make sure a server is up,
/// and say which one.
async fn start(home: &Home) -> Result<(), String> {
    let attached = client::attach_or_start(home)
        .await
        .map_err(|error| error.to_string())?;
    if attached.started {
        println!("started the server");
    } else {
        println!("the server was already running");
    }
    status(home).await
}

async fn status(home: &Home) -> Result<(), String> {
    println!("client:");
    println!("  version: {VERSION}");
    println!("  protocol: {CLIENT_PROTOCOL}");
    println!("  api generation: {API_GENERATION}");
    println!("server:");
    let status = match ApiClient::connect(home).await {
        Ok(mut api) => api.status().await,
        Err(error) => Err(error),
    };
    match status {
        Ok(status) => {
            let compatible = if status.client_protocol == CLIENT_PROTOCOL {
                "compatible"
            } else {
                "incompatible"
            };
            println!("  status: running");
            println!("  version: {}", status.version);
            println!("  pid: {}", status.pid);
            println!("  protocol: {} ({compatible})", status.client_protocol);
            println!("  api generation: {}", status.api_generation);
            println!("  uptime: {}", uptime(status.started_unix_seconds));
            println!("  home: {}", status.home.display());
            Ok(())
        }
        Err(ApiClientError::NotRunning) => {
            println!("  status: not running");
            println!("  home: {}", home.dir().display());
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

async fn stop(home: &Home) -> Result<(), String> {
    match api::stop(home).await {
        Ok(true) => println!("server stopped"),
        Ok(false) => println!("no server is running"),
        Err(error) => return Err(error.to_string()),
    }
    Ok(())
}

fn uptime(started_unix_seconds: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let seconds = now.saturating_sub(started_unix_seconds);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Invocation {
        invocation(
            &Cli::try_parse_from(std::iter::once("fpk").chain(args.iter().copied())).unwrap(),
        )
    }

    #[test]
    fn no_command_opens_the_terminal_ui() {
        assert_eq!(parse_args(&[]), Invocation::Tui);
    }

    #[test]
    fn server_alone_runs_the_server_and_stop_is_a_client_command() {
        assert_eq!(parse_args(&["server"]), Invocation::Server);
        assert_eq!(
            parse_args(&["server", "start"]),
            Invocation::Client(ClientCommand::Start)
        );
        assert_eq!(
            parse_args(&["server", "stop"]),
            Invocation::Client(ClientCommand::Stop)
        );
        assert_eq!(
            parse_args(&["status"]),
            Invocation::Client(ClientCommand::Status)
        );
    }

    #[test]
    fn uptime_reads_at_a_glance() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(uptime(now - 5), "5s");
        assert_eq!(uptime(now - 125), "2m 5s");
        assert_eq!(uptime(now - 7_260), "2h 1m");
        assert_eq!(uptime(now + 10), "0s");
    }
}
