use std::io::Write as _;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use file_peeker_protocol::{
    ClientMessage, ServerMessage,
    io::{read_message, send_message},
};
use thiserror::Error;
use tokio::{
    io::{AsyncBufRead, AsyncReadExt, AsyncWrite},
    net::TcpListener,
    sync::{Semaphore, mpsc},
    task::JoinSet,
};

mod ops;

const MAX_CONNECTIONS: usize = 128;
const SERVER_STARTUP_PREFIX: &str = "FILE_PEEKER_SERVER_STARTUP=";

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("protocol error: {message}")]
    Protocol { message: String },
    #[error("server I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Parser)]
#[command(version, about = "File Peeker filesystem server")]
struct Cli {
    #[command(subcommand)]
    command: ServerCommand,
}

#[derive(Debug, Subcommand)]
enum ServerCommand {
    /// Listen on an ephemeral IPv4 loopback TCP port until stdin closes.
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
        ServerCommand::Serve => serve().await,
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
        file_peeker_protocol::PROTOCOL_VERSION
    );
}

async fn serve() -> Result<(), ServerError> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let token = generate_token()?;
    println!(
        "{SERVER_STARTUP_PREFIX}{}",
        serde_json::json!({ "port": port, "token": token })
    );
    std::io::stdout().flush()?;

    let token = Arc::new(token);
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let (shutdown_sender, mut shutdown_receiver) = mpsc::unbounded_channel();
    let mut operations = JoinSet::new();
    let mut stdin = tokio::io::stdin();
    let mut stdin_probe = [0_u8; 1];

    loop {
        tokio::select! {
            result = stdin.read(&mut stdin_probe) => {
                match result? {
                    0 => break,
                    _ => return Err(ServerError::Protocol {
                        message: "server stdin is a lifetime lease and does not accept data".into(),
                    }),
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let token = Arc::clone(&token);
                let shutdown_sender = shutdown_sender.clone();
                operations.spawn(async move {
                    let _permit = permit;
                    let _ = ops::handle(stream, &token, &shutdown_sender).await;
                });
            }
            _ = shutdown_receiver.recv() => break,
            Some(_) = operations.join_next(), if !operations.is_empty() => {}
            result = termination_signal() => {
                result?;
                break;
            }
        }
    }

    operations.abort_all();
    while operations.join_next().await.is_some() {}
    Ok(())
}

fn generate_token() -> Result<String, ServerError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| ServerError::Protocol {
        message: format!("cannot generate authentication token: {error}"),
    })?;
    let mut token = String::with_capacity(64);
    for byte in bytes {
        token.push(char::from(HEX[usize::from(byte >> 4)]));
        token.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(token)
}

pub(crate) async fn read_client_message<S>(stream: &mut S) -> Result<ClientMessage, ServerError>
where
    S: AsyncBufRead + Unpin,
{
    read_message(stream).await.map_err(map_message_error)
}

pub(crate) async fn write_server_message<S>(
    stream: &mut S,
    message: &ServerMessage,
) -> Result<(), ServerError>
where
    S: AsyncWrite + Unpin,
{
    send_message(stream, message)
        .await
        .map_err(map_message_error)
}

fn map_message_error(error: std::io::Error) -> ServerError {
    match error.kind() {
        std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => {
            ServerError::Protocol {
                message: error.to_string(),
            }
        }
        _ => ServerError::Io(error),
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

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use file_peeker_protocol::{ClientMessage, ErrorCode, ServerMessage};

    use super::{Cli, ServerCommand, read_client_message, write_server_message};

    #[test]
    fn parses_serve_command() {
        let cli = Cli::try_parse_from(["file-peeker-server", "serve"]).unwrap();
        assert!(matches!(cli.command, ServerCommand::Serve));
        Cli::command().debug_assert();
    }

    #[test]
    fn token_is_256_bits_of_hex() {
        let token = super::generate_token().unwrap();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn client_message_can_exceed_one_mib() {
        let expected = ClientMessage::List {
            path: format!("/{}", "x".repeat(1024 * 1024 + 1)),
        };
        let mut bytes = serde_json::to_vec(&expected).unwrap();
        bytes.push(b'\n');
        assert!(bytes.len() > 1024 * 1024);
        let mut input = bytes.as_slice();

        assert_eq!(read_client_message(&mut input).await.unwrap(), expected);
    }

    #[tokio::test]
    async fn server_message_can_exceed_one_mib() {
        let message = ServerMessage::Error {
            code: ErrorCode::Io,
            message: "x".repeat(1024 * 1024 + 1),
        };
        let mut output = Vec::new();

        write_server_message(&mut output, &message).await.unwrap();

        assert!(output.len() > 1024 * 1024);
    }
}
