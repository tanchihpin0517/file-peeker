use std::{
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand, ValueEnum};
use file_peeker_protocol::{
    ClientMessage, ConnectionRole, ErrorCode, ListingEntry, PROTOCOL_VERSION, ServerMessage,
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    task::JoinSet,
};

const MAX_SOCKET_PATH_BYTES: usize = 100;

#[derive(Debug, Error)]
enum ServerError {
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
                    let _ = handle_operation(stream).await;
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

async fn read_client_message(stream: &mut UnixStream) -> Result<ClientMessage, ServerError> {
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
        bytes.push(byte[0]);
    }

    serde_json::from_slice(&bytes).map_err(|error| ServerError::Protocol {
        message: format!("invalid JSON message: {error}"),
    })
}

async fn write_server_message(
    stream: &mut UnixStream,
    message: &ServerMessage,
) -> Result<(), ServerError> {
    let mut bytes = serde_json::to_vec(message).map_err(|error| ServerError::Protocol {
        message: format!("cannot encode response: {error}"),
    })?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

async fn handle_operation(mut stream: UnixStream) -> Result<(), ServerError> {
    let message = read_client_message(&mut stream).await?;
    match message {
        ClientMessage::Hello {
            version,
            role: ConnectionRole::Operation,
        } if version == PROTOCOL_VERSION => {
            write_server_message(
                &mut stream,
                &ServerMessage::HelloOk {
                    version: PROTOCOL_VERSION,
                },
            )
            .await?;
        }
        ClientMessage::Hello { version, .. } if version != PROTOCOL_VERSION => {
            write_server_message(
                &mut stream,
                &ServerMessage::Error {
                    code: ErrorCode::UnsupportedVersion,
                    message: "Unsupported protocol version".into(),
                },
            )
            .await?;
            return Ok(());
        }
        _ => {
            return Err(ServerError::Protocol {
                message: "operation connection must begin with operation hello".into(),
            });
        }
    }

    match read_client_message(&mut stream).await? {
        ClientMessage::List { path } => handle_listing(&mut stream, &path).await,
        ClientMessage::CurrentRoot => handle_current_root(&mut stream).await,
        _ => Err(ServerError::Protocol {
            message: "operation connection must contain one request".into(),
        }),
    }
}

async fn handle_current_root(stream: &mut UnixStream) -> Result<(), ServerError> {
    let path = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            return write_operation_error(stream, ErrorCode::Io, &error.to_string()).await;
        }
    };
    let Some(path) = path.to_str() else {
        return write_operation_error(
            stream,
            ErrorCode::InvalidPath,
            "Current directory is not valid UTF-8",
        )
        .await;
    };
    write_server_message(
        stream,
        &ServerMessage::CurrentRoot {
            path: path.to_owned(),
        },
    )
    .await
}

async fn handle_listing(stream: &mut UnixStream, path: &str) -> Result<(), ServerError> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return write_operation_error(
            stream,
            ErrorCode::InvalidPath,
            "Listing path must be absolute",
        )
        .await;
    }

    let directory_entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => return write_io_error(stream, error).await,
    };
    let mut entries = Vec::new();

    for entry in directory_entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => return write_io_error(stream, error).await,
        };
        let entry_path = entry.path();
        let Some(path) = entry_path.to_str() else {
            return write_operation_error(
                stream,
                ErrorCode::InvalidPath,
                "Encountered a non-UTF-8 path",
            )
            .await;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return write_operation_error(
                stream,
                ErrorCode::InvalidPath,
                "Encountered a non-UTF-8 filename",
            )
            .await;
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => return write_io_error(stream, error).await,
        };
        let (kind, navigable) = if file_type.is_dir() {
            (file_peeker_protocol::EntryKind::Directory, true)
        } else if file_type.is_file() {
            (file_peeker_protocol::EntryKind::File, false)
        } else if file_type.is_symlink() {
            (
                file_peeker_protocol::EntryKind::Symlink,
                entry_path
                    .metadata()
                    .is_ok_and(|metadata| metadata.is_dir()),
            )
        } else {
            (file_peeker_protocol::EntryKind::Other, false)
        };

        entries.push(ListingEntry {
            path: path.to_owned(),
            name,
            kind,
            navigable,
        });
    }

    write_server_message(stream, &ServerMessage::ListResult { entries }).await
}

async fn write_io_error(stream: &mut UnixStream, error: std::io::Error) -> Result<(), ServerError> {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => ErrorCode::NotFound,
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        std::io::ErrorKind::NotADirectory => ErrorCode::NotDirectory,
        _ => ErrorCode::Io,
    };
    write_operation_error(stream, code, &error.to_string()).await
}

async fn write_operation_error(
    stream: &mut UnixStream,
    code: ErrorCode,
    message: &str,
) -> Result<(), ServerError> {
    write_server_message(
        stream,
        &ServerMessage::Error {
            code,
            message: message.into(),
        },
    )
    .await
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
    use std::{collections::HashSet, os::unix::fs::PermissionsExt, path::Path};

    use clap::{CommandFactory, Parser, error::ErrorKind};
    use file_peeker_protocol::{
        ClientMessage, ConnectionRole, ErrorCode, PROTOCOL_VERSION, ServerMessage,
    };
    use tempfile::TempDir;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixStream,
        time::{Duration, sleep},
    };

    use super::{Cli, ServerCommand, VersionFormat, handle_listing, read_client_message, serve};

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
    async fn listing_sends_one_atomic_result() {
        let directory = private_tempdir();
        std::fs::write(directory.path().join("notes.txt"), "hello")
            .expect("fixture file should be created");
        std::fs::create_dir(directory.path().join("docs"))
            .expect("fixture directory should be created");
        let listing_path = directory.path().to_string_lossy().into_owned();
        let (mut server_stream, mut client_stream) =
            UnixStream::pair().expect("socket pair should be created");

        let server =
            tokio::spawn(async move { handle_listing(&mut server_stream, &listing_path).await });
        let mut response = String::new();
        client_stream
            .read_to_string(&mut response)
            .await
            .expect("listing response should be readable");
        server
            .await
            .expect("listing task should complete")
            .expect("listing should succeed");

        assert_eq!(response.lines().count(), 1);
        let message: ServerMessage =
            serde_json::from_str(response.trim_end()).expect("listing response should decode");
        let ServerMessage::ListResult { entries } = message else {
            panic!("listing should return one list_result message");
        };
        let names: HashSet<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, HashSet::from(["docs", "notes.txt"]));
    }

    #[tokio::test]
    async fn listing_error_sends_only_an_error_result() {
        let directory = private_tempdir();
        let listing_path = directory
            .path()
            .join("missing")
            .to_string_lossy()
            .into_owned();
        let (mut server_stream, mut client_stream) =
            UnixStream::pair().expect("socket pair should be created");

        let server =
            tokio::spawn(async move { handle_listing(&mut server_stream, &listing_path).await });
        let mut response = String::new();
        client_stream
            .read_to_string(&mut response)
            .await
            .expect("listing error should be readable");
        server
            .await
            .expect("listing task should complete")
            .expect("listing error should be written");

        assert_eq!(response.lines().count(), 1);
        let message: ServerMessage =
            serde_json::from_str(response.trim_end()).expect("listing error should decode");
        assert!(matches!(
            message,
            ServerMessage::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn client_message_may_exceed_one_mib() {
        let path = format!("/{}", "x".repeat(1024 * 1024));
        let message = ClientMessage::List { path };
        let mut bytes = serde_json::to_vec(&message).expect("large request should encode");
        assert!(bytes.len() > 1024 * 1024);
        bytes.push(b'\n');
        let (mut server_stream, mut client_stream) =
            UnixStream::pair().expect("socket pair should be created");

        let writer = tokio::spawn(async move {
            client_stream
                .write_all(&bytes)
                .await
                .expect("large request should be writable");
        });
        let decoded = read_client_message(&mut server_stream)
            .await
            .expect("large request should be readable");
        writer.await.expect("writer task should complete");

        assert_eq!(decoded, message);
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
