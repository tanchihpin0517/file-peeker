use clap::{Parser, Subcommand, ValueEnum};
use file_peeker_server::protocol::PROTOCOL_VERSION;

mod ops;
mod server;

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

#[tokio::main]
async fn main() {
    let result = match Cli::parse().command {
        ServerCommand::Serve => server::serve().await,
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
