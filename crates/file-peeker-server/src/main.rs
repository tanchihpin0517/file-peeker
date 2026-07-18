use std::{
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand, ValueEnum};
use file_peeker_protocol::{
    ClientMessage, ConnectionRole, ErrorCode, PROTOCOL_VERSION, ServerMessage,
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    task::JoinSet,
};

mod ops;

const MAX_SOCKET_PATH_BYTES: usize = 100;
const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("invalid socket path: {message}")]
    InvalidSocketPath { message: String },
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
    /// Listen for a client on a Unix socket.
    Serve {
        #[arg(long = "socket", value_name = "PATH")]
        socket_path: PathBuf,
        /// Remove the private socket parent directory when the server exits.
        #[arg(long)]
        remove_parent_on_exit: bool,
    },
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
        ServerCommand::Serve {
            socket_path,
            remove_parent_on_exit,
        } => serve(socket_path, remove_parent_on_exit).await,
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

async fn serve(socket_path: PathBuf, remove_parent_on_exit: bool) -> Result<(), ServerError> {
    validate_socket_path(&socket_path)?;
    let listener = UnixListener::bind(&socket_path)?;
    let _socket_lease = SocketLease::new(socket_path, remove_parent_on_exit);

    let (mut control, _) = listener.accept().await?;
    handshake_control(&mut control).await?;
    let mut operations = JoinSet::new();
    let mut control_probe = [0_u8; 1];

    tokio::select! {
        result = run_operations(&listener, &mut control, &mut control_probe, &mut operations) => result,
        result = termination_signal() => result,
    }
}

async fn run_operations(
    listener: &UnixListener,
    control: &mut UnixStream,
    control_probe: &mut [u8; 1],
    operations: &mut JoinSet<()>,
) -> Result<(), ServerError> {
    loop {
        tokio::select! {
            result = control.read(control_probe) => {
                return match result? {
                    0 => Ok(()),
                    _ => Err(ServerError::Protocol {
                        message: "control connection does not accept messages after hello".into(),
                    }),
                };
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                operations.spawn(async move {
                    let _ = ops::handle(stream).await;
                });
            }
            Some(_) = operations.join_next(), if !operations.is_empty() => {}
        }
    }
}

fn validate_socket_path(socket_path: &Path) -> Result<(), ServerError> {
    if !socket_path.is_absolute() {
        return Err(ServerError::InvalidSocketPath {
            message: "path must be absolute".into(),
        });
    }
    if socket_path.as_os_str().as_bytes().len() > MAX_SOCKET_PATH_BYTES {
        return Err(ServerError::InvalidSocketPath {
            message: format!("path must not exceed {MAX_SOCKET_PATH_BYTES} bytes"),
        });
    }
    if socket_path.exists() {
        return Err(ServerError::InvalidSocketPath {
            message: "path already exists".into(),
        });
    }

    let parent = socket_path
        .parent()
        .ok_or_else(|| ServerError::InvalidSocketPath {
            message: "path must have a parent directory".into(),
        })?;
    let metadata = std::fs::metadata(parent).map_err(|error| ServerError::InvalidSocketPath {
        message: format!("cannot inspect parent directory: {error}"),
    })?;
    if !metadata.is_dir() {
        return Err(ServerError::InvalidSocketPath {
            message: "parent path is not a directory".into(),
        });
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(ServerError::InvalidSocketPath {
            message: "parent directory must be accessible only by its owner".into(),
        });
    }

    Ok(())
}

async fn handshake_control(stream: &mut UnixStream) -> Result<(), ServerError> {
    let message = read_client_message(stream).await?;
    match message {
        ClientMessage::Hello {
            version,
            role: ConnectionRole::Control,
        } if version == PROTOCOL_VERSION => {
            write_server_message(
                stream,
                &ServerMessage::HelloOk {
                    version: PROTOCOL_VERSION,
                },
            )
            .await
        }
        ClientMessage::Hello { version, .. } if version != PROTOCOL_VERSION => {
            write_server_message(
                stream,
                &ServerMessage::Error {
                    code: ErrorCode::UnsupportedVersion,
                    message: "Unsupported protocol version".into(),
                },
            )
            .await?;
            Err(ServerError::Protocol {
                message: format!("unsupported protocol version {version}"),
            })
        }
        ClientMessage::Hello { .. } => Err(ServerError::Protocol {
            message: "first connection must have the control role".into(),
        }),
        _ => Err(ServerError::Protocol {
            message: "first message must be hello".into(),
        }),
    }
}

pub(crate) async fn read_client_message(
    stream: &mut UnixStream,
) -> Result<ClientMessage, ServerError> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        if stream.read(&mut byte).await? == 0 {
            return Err(ServerError::Protocol {
                message: "connection closed before a complete message".into(),
            });
        }
        if byte[0] == b'\n' {
            break;
        }
        if bytes.len() == MAX_FRAME_BYTES {
            return Err(ServerError::Protocol {
                message: format!("message exceeds {MAX_FRAME_BYTES} bytes"),
            });
        }
        bytes.push(byte[0]);
    }

    serde_json::from_slice(&bytes).map_err(|error| ServerError::Protocol {
        message: format!("invalid JSON message: {error}"),
    })
}

pub(crate) async fn write_server_message(
    stream: &mut UnixStream,
    message: &ServerMessage,
) -> Result<(), ServerError> {
    let mut bytes = serde_json::to_vec(message).map_err(|error| ServerError::Protocol {
        message: format!("cannot encode response: {error}"),
    })?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(ServerError::Protocol {
            message: format!("response exceeds {MAX_FRAME_BYTES} bytes"),
        });
    }
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
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

struct SocketLease {
    path: PathBuf,
    remove_parent_on_exit: bool,
}

impl SocketLease {
    fn new(path: PathBuf, remove_parent_on_exit: bool) -> Self {
        Self {
            path,
            remove_parent_on_exit,
        }
    }
}

impl Drop for SocketLease {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        if self.remove_parent_on_exit
            && let Some(parent) = self.path.parent()
        {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::Path};

    use clap::{CommandFactory, Parser, error::ErrorKind};
    use file_peeker_protocol::{ClientMessage, ConnectionRole, PROTOCOL_VERSION, ServerMessage};
    use tempfile::TempDir;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixStream,
        time::{Duration, sleep},
    };

    use super::{Cli, ServerCommand, VersionFormat, read_client_message, serve};

    #[test]
    fn parses_serve_command() {
        let cli = Cli::try_parse_from([
            "file-peeker-server",
            "serve",
            "--socket",
            "/tmp/file-peeker.sock",
        ])
        .expect("serve command should parse");

        assert!(matches!(
            cli.command,
            ServerCommand::Serve { socket_path, .. }
                if socket_path == Path::new("/tmp/file-peeker.sock")
        ));
    }

    #[test]
    fn parses_version_json_command() {
        let cli = Cli::try_parse_from(["file-peeker-server", "version", "--format", "json"])
            .expect("version command should parse");

        assert!(matches!(
            cli.command,
            ServerCommand::Version {
                format: VersionFormat::Json
            }
        ));
    }

    #[test]
    fn rejects_invalid_version_format() {
        let error = Cli::try_parse_from(["file-peeker-server", "version", "--format", "text"])
            .expect_err("unsupported version formats should fail");

        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn rejects_a_missing_socket_path() {
        let error = Cli::try_parse_from(["file-peeker-server", "serve"])
            .expect_err("a missing socket path should fail");

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn provides_generated_help_and_version() {
        Cli::command().debug_assert();

        let help = Cli::try_parse_from(["file-peeker-server", "--help"])
            .expect_err("help should exit before command dispatch");
        assert_eq!(help.kind(), ErrorKind::DisplayHelp);
        assert!(help.to_string().contains("Usage: file-peeker-server"));

        let version = Cli::try_parse_from(["file-peeker-server", "--version"])
            .expect_err("version should exit before command dispatch");
        assert_eq!(version.kind(), ErrorKind::DisplayVersion);
        assert!(version.to_string().contains(env!("CARGO_PKG_VERSION")));
    }

    #[tokio::test]
    async fn serves_control_handshake_and_cleans_up() {
        let directory = private_tempdir();
        let socket_path = directory.path().join("server.sock");
        let server = tokio::spawn(serve(socket_path.clone(), false));

        let mut stream = connect_with_retry(&socket_path).await;
        let hello = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            role: ConnectionRole::Control,
        };
        let mut request = serde_json::to_vec(&hello).expect("hello should encode");
        request.push(b'\n');
        stream
            .write_all(&request)
            .await
            .expect("hello should be written");

        let mut response = Vec::new();
        loop {
            let mut byte = [0_u8; 1];
            stream
                .read_exact(&mut byte)
                .await
                .expect("response should be read");
            if byte[0] == b'\n' {
                break;
            }
            response.push(byte[0]);
        }
        let response: ServerMessage =
            serde_json::from_slice(&response).expect("response should decode");
        assert_eq!(
            response,
            ServerMessage::HelloOk {
                version: PROTOCOL_VERSION
            }
        );

        drop(stream);
        server
            .await
            .expect("server task should complete")
            .expect("server should exit cleanly");
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn client_message_cannot_exceed_one_mib() {
        let path = format!("/{}", "x".repeat(1024 * 1024));
        let message = ClientMessage::List { path };
        let mut bytes = serde_json::to_vec(&message).expect("large request should encode");
        assert!(bytes.len() > 1024 * 1024);
        bytes.push(b'\n');
        let (mut server_stream, mut client_stream) =
            UnixStream::pair().expect("socket pair should be created");

        let writer = tokio::spawn(async move { client_stream.write_all(&bytes).await });
        let error = read_client_message(&mut server_stream)
            .await
            .expect_err("large request should be rejected");
        drop(server_stream);
        let _ = writer.await.expect("writer task should complete");

        assert!(matches!(error, super::ServerError::Protocol { .. }));
    }

    fn private_tempdir() -> TempDir {
        let directory = tempfile::Builder::new()
            .prefix("fp-server-test-")
            .tempdir_in("/tmp")
            .expect("temporary directory should be created");
        let mut permissions = std::fs::metadata(directory.path())
            .expect("temporary directory should exist")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(directory.path(), permissions)
            .expect("temporary directory should be private");
        directory
    }

    async fn connect_with_retry(socket_path: &std::path::Path) -> UnixStream {
        for _ in 0..100 {
            match UnixStream::connect(socket_path).await {
                Ok(stream) => return stream,
                Err(_) => sleep(Duration::from_millis(10)).await,
            }
        }
        panic!("server socket did not become ready");
    }
}
