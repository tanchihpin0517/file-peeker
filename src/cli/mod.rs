//! `fpk`'s command line: which role to run, and the commands that are neither
//! the server nor the terminal UI.

mod host;
mod machine;

use std::{
    ffi::OsString,
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
    protocol::{
        CLIENT_PROTOCOL, VERSION,
        api::{API_GENERATION, DemoParams},
    },
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
    /// Run the command on a saved machine instead of this one.
    #[arg(long, global = true, value_name = "LABEL")]
    machine: Option<String>,
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
    /// Work with the server's tasks.
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
    /// Manage saved SSH machines.
    Machine {
        #[command(subcommand)]
        action: MachineAction,
    },
    /// Relay standard input and output to this host's client socket. What a
    /// client runs over SSH to reach this host's server.
    #[command(hide = true)]
    Bridge,
    /// Print what is installed here, as one line of JSON, for a client
    /// deciding whether to install.
    #[command(hide = true)]
    Probe,
    /// Print what this client installs on a machine, as one line of JSON.
    #[command(hide = true)]
    BuildInfo,
}

#[derive(Debug, Subcommand)]
enum MachineAction {
    /// Save an SSH machine, installing fpk on it first if it needs it.
    Add {
        /// What the terminal UI and `--machine` call it.
        label: String,
        /// Anything `ssh` accepts: `user@host`, or a `Host` from your SSH
        /// configuration.
        ssh: String,
        /// Install, update and restart without asking.
        #[arg(long)]
        yes: bool,
    },
    /// List the saved machines.
    List,
    /// Forget a saved machine. Its server and tasks keep running.
    Remove { label: String },
    /// Sign in to a saved machine, for a host whose login needs answering.
    Login { label: String },
}

#[derive(Debug, Subcommand)]
enum TaskAction {
    /// Start a demo task that counts for a while, to watch in the terminal UI.
    /// Starts the server if none is running.
    Demo {
        /// How long it counts.
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        /// Fail halfway instead of finishing.
        #[arg(long)]
        fail: bool,
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
    /// A command that cannot run as given, and why.
    Refused(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ClientCommand {
    Start,
    Status,
    Stop,
    Demo {
        seconds: u64,
        fail: bool,
    },
    Bridge,
    Probe,
    BuildInfo,
    MachineAdd {
        label: String,
        ssh: String,
        yes: bool,
    },
    MachineList,
    MachineRemove {
        label: String,
    },
    MachineLogin {
        label: String,
    },
    /// Any command, run on a saved machine with these arguments.
    Remote {
        label: String,
        args: Vec<OsString>,
    },
}

pub(crate) fn parse() -> Invocation {
    let raw: Vec<OsString> = std::env::args_os().skip(1).collect();
    invocation(&Cli::parse(), &raw)
}

fn invocation(cli: &Cli, raw: &[OsString]) -> Invocation {
    if let Some(label) = &cli.machine {
        return match &cli.command {
            Some(
                Command::Status
                | Command::Server {
                    action: Some(ServerAction::Start | ServerAction::Stop),
                }
                | Command::Task { .. },
            ) => Invocation::Client(ClientCommand::Remote {
                label: label.clone(),
                args: without_machine(raw),
            }),
            None => Invocation::Refused(
                "the terminal UI already lists every machine; run `fpk` without --machine"
                    .to_owned(),
            ),
            Some(_) => Invocation::Refused("this command does not run on a machine".to_owned()),
        };
    }
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
        Some(Command::Task {
            action: TaskAction::Demo { seconds, fail },
        }) => Invocation::Client(ClientCommand::Demo {
            seconds: *seconds,
            fail: *fail,
        }),
        Some(Command::Bridge) => Invocation::Client(ClientCommand::Bridge),
        Some(Command::Probe) => Invocation::Client(ClientCommand::Probe),
        Some(Command::BuildInfo) => Invocation::Client(ClientCommand::BuildInfo),
        Some(Command::Machine { action }) => Invocation::Client(match action {
            MachineAction::Add { label, ssh, yes } => ClientCommand::MachineAdd {
                label: label.clone(),
                ssh: ssh.clone(),
                yes: *yes,
            },
            MachineAction::List => ClientCommand::MachineList,
            MachineAction::Remove { label } => ClientCommand::MachineRemove {
                label: label.clone(),
            },
            MachineAction::Login { label } => ClientCommand::MachineLogin {
                label: label.clone(),
            },
        }),
    }
}

/// The arguments as given, without `--machine <label>`: what runs on the host.
fn without_machine(raw: &[OsString]) -> Vec<OsString> {
    let mut args = Vec::new();
    let mut skip = false;
    for arg in raw {
        if skip {
            skip = false;
        } else if arg == "--machine" {
            skip = true;
        } else if !arg.to_string_lossy().starts_with("--machine=") {
            args.push(arg.clone());
        }
    }
    args
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
        let done = |result: Result<(), String>| result.map(|()| ExitCode::SUCCESS);
        match command {
            ClientCommand::Start => done(start(home).await),
            ClientCommand::Status => done(status(home).await),
            ClientCommand::Stop => done(stop(home).await),
            ClientCommand::Demo { seconds, fail } => done(demo(home, seconds, fail).await),
            // The bridge and a remote command end with an exit code of their
            // own: the relay's, or the host's.
            ClientCommand::Bridge => host::bridge(home).await,
            ClientCommand::Probe => done(host::probe(home).await),
            ClientCommand::BuildInfo => done(host::build_info()),
            ClientCommand::MachineAdd { label, ssh, yes } => {
                done(machine::add(home, &label, &ssh, yes).await)
            }
            ClientCommand::MachineList => done(machine::list(home)),
            ClientCommand::MachineRemove { label } => done(machine::remove(home, &label)),
            ClientCommand::MachineLogin { label } => done(machine::login(home, &label).await),
            ClientCommand::Remote { label, args } => machine::run_remote(home, &label, &args).await,
        }
    });
    match outcome {
        Ok(code) => code,
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

async fn demo(home: &Home, seconds: u64, fail: bool) -> Result<(), String> {
    client::attach_or_start(home)
        .await
        .map_err(|error| error.to_string())?;
    let mut api = ApiClient::connect(home)
        .await
        .map_err(|error| error.to_string())?;
    let id = api
        .submit_demo(&DemoParams { seconds, fail })
        .await
        .map_err(|error| error.to_string())?;
    println!("started demo.count task {id}");
    Ok(())
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
        let raw: Vec<OsString> = args.iter().map(OsString::from).collect();
        let cli = Cli::try_parse_from(std::iter::once("fpk").chain(args.iter().copied())).unwrap();
        invocation(&cli, &raw)
    }

    #[test]
    fn machine_commands_parse() {
        assert_eq!(
            parse_args(&["machine", "add", "box", "me@box", "--yes"]),
            Invocation::Client(ClientCommand::MachineAdd {
                label: "box".to_owned(),
                ssh: "me@box".to_owned(),
                yes: true
            })
        );
        assert_eq!(
            parse_args(&["machine", "login", "box"]),
            Invocation::Client(ClientCommand::MachineLogin {
                label: "box".to_owned()
            })
        );
    }

    #[test]
    fn machine_flag_forwards_everything_else_wherever_it_was_given() {
        let remote = |label: &str, args: &[&str]| {
            Invocation::Client(ClientCommand::Remote {
                label: label.to_owned(),
                args: args.iter().map(OsString::from).collect(),
            })
        };
        assert_eq!(
            parse_args(&["--machine", "box", "task", "demo", "--seconds", "3"]),
            remote("box", &["task", "demo", "--seconds", "3"])
        );
        assert_eq!(
            parse_args(&["status", "--machine=box"]),
            remote("box", &["status"])
        );
        assert!(matches!(
            parse_args(&["--machine", "box"]),
            Invocation::Refused(_)
        ));
        assert!(matches!(
            parse_args(&["--machine", "box", "machine", "list"]),
            Invocation::Refused(_)
        ));
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
        assert_eq!(
            parse_args(&["task", "demo", "--seconds", "2", "--fail"]),
            Invocation::Client(ClientCommand::Demo {
                seconds: 2,
                fail: true
            })
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
