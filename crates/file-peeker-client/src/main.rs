use std::{io, path::Path};

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod cli;

#[derive(Debug, Parser)]
#[command(version, about = "File Peeker client test CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Eq, PartialEq, Subcommand)]
enum Command {
    Test {
        #[command(subcommand)]
        command: TestCommand,
    },
}

#[derive(Debug, Eq, PartialEq, Subcommand)]
enum TestCommand {
    Connect {
        #[arg(long)]
        force: bool,
        server: String,
    },
    Install {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        from_source: Option<String>,
        server: String,
    },
    List {
        path: String,
        #[arg(long)]
        remote: Option<String>,
    },
    SshConnection {
        server: String,
    },
    StartServer {
        server: String,
    },
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,file_peeker_client=debug"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
    run(&Cli::parse()).await
}

async fn run(cli: &Cli) -> io::Result<()> {
    match &cli.command {
        Command::Test { command } => match command {
            TestCommand::Connect { force, server } => {
                cli::connect::run(server, *force).await?;
            }
            TestCommand::Install {
                force,
                from_source,
                server,
            } => {
                cli::install::run(server, *force, from_source.as_deref().map(Path::new)).await?;
            }
            TestCommand::List { path, remote } => {
                cli::list::run(path, remote.as_deref()).await?;
            }
            TestCommand::SshConnection { server } => cli::ssh_connection::run(server).await?,
            TestCommand::StartServer { server } => cli::start_server::run(server).await?,
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::{Parser, error::ErrorKind};

    use super::{Cli, Command, TestCommand};

    #[test]
    fn parses_test_list() {
        let cli = Cli::try_parse_from(["file-peeker-client", "test", "list", "/tmp/reports"])
            .expect("test list command should parse");

        assert_eq!(
            cli.command,
            Command::Test {
                command: TestCommand::List {
                    path: "/tmp/reports".into(),
                    remote: None
                }
            }
        );
    }

    #[test]
    fn parses_remote_test_list() {
        let cli = Cli::try_parse_from([
            "file-peeker-client",
            "test",
            "list",
            ".",
            "--remote",
            "example.test",
        ])
        .expect("remote test list command should parse");

        assert_eq!(
            cli.command,
            Command::Test {
                command: TestCommand::List {
                    path: ".".into(),
                    remote: Some("example.test".into())
                }
            }
        );
    }

    #[test]
    fn test_list_requires_path() {
        let error = Cli::try_parse_from(["file-peeker-client", "test", "list"])
            .expect_err("test list should require a path");

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn parses_test_install_server() {
        let cli = Cli::try_parse_from(["file-peeker-client", "test", "install", "example.test"])
            .expect("test install command should parse");

        assert_eq!(
            cli.command,
            Command::Test {
                command: TestCommand::Install {
                    force: false,
                    from_source: None,
                    server: "example.test".into()
                }
            }
        );
    }

    #[test]
    fn parses_test_install_force() {
        let cli = Cli::try_parse_from([
            "file-peeker-client",
            "test",
            "install",
            "--force",
            "example.test",
        ])
        .expect("test install --force command should parse");

        assert_eq!(
            cli.command,
            Command::Test {
                command: TestCommand::Install {
                    force: true,
                    from_source: None,
                    server: "example.test".into()
                }
            }
        );
    }

    #[test]
    fn parses_test_install_without_server() {
        let error = Cli::try_parse_from(["file-peeker-client", "test", "install"])
            .expect_err("test install should require a remote server");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn test_install_from_source_requires_server() {
        let error = Cli::try_parse_from([
            "file-peeker-client",
            "test",
            "install",
            "--from-source",
            "/tmp/file-peeker-source",
        ])
        .expect_err("source install should require a remote server");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn parses_forced_remote_test_install_from_source() {
        let cli = Cli::try_parse_from([
            "file-peeker-client",
            "test",
            "install",
            "--force",
            "--from-source",
            "/tmp/file-peeker-source",
            "example.test",
        ])
        .expect("remote source install command should parse");

        assert_eq!(
            cli.command,
            Command::Test {
                command: TestCommand::Install {
                    force: true,
                    from_source: Some("/tmp/file-peeker-source".into()),
                    server: "example.test".into(),
                }
            }
        );
    }

    #[test]
    fn parses_test_ssh_connection_server() {
        let cli = Cli::try_parse_from([
            "file-peeker-client",
            "test",
            "ssh-connection",
            "example.test",
        ])
        .expect("test ssh-connection command should parse");

        assert_eq!(
            cli.command,
            Command::Test {
                command: TestCommand::SshConnection {
                    server: "example.test".into()
                }
            }
        );
    }

    #[test]
    fn parses_test_start_server() {
        let cli =
            Cli::try_parse_from(["file-peeker-client", "test", "start-server", "example.test"])
                .expect("test start-server command should parse");

        assert_eq!(
            cli.command,
            Command::Test {
                command: TestCommand::StartServer {
                    server: "example.test".into()
                }
            }
        );
    }

    #[test]
    fn test_upload_is_rejected() {
        let error = Cli::try_parse_from(["file-peeker-client", "test", "upload", "example.test"])
            .expect_err("test upload should not be a supported command");

        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn parses_test_connect() {
        let cli = Cli::try_parse_from(["file-peeker-client", "test", "connect", "example.test"])
            .expect("test connect command should parse");

        assert_eq!(
            cli.command,
            Command::Test {
                command: TestCommand::Connect {
                    force: false,
                    server: "example.test".into()
                }
            }
        );
    }

    #[test]
    fn parses_test_connect_without_server() {
        let error = Cli::try_parse_from(["file-peeker-client", "test", "connect"])
            .expect_err("test connect should require a remote server");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn parses_forced_remote_test_connect() {
        let cli = Cli::try_parse_from([
            "file-peeker-client",
            "test",
            "connect",
            "--force",
            "example.test",
        ])
        .expect("remote test connect --force command should parse");

        assert_eq!(
            cli.command,
            Command::Test {
                command: TestCommand::Connect {
                    force: true,
                    server: "example.test".into()
                }
            }
        );
    }

    #[test]
    fn forced_test_connect_requires_server() {
        let error = Cli::try_parse_from(["file-peeker-client", "test", "connect", "--force"])
            .expect_err("forced test connect should require a remote server");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }
}
