use std::path::{Path, PathBuf};

use file_peeker_protocol::{
    ClientMessage, ConnectionRole, EntryKind as ProtocolEntryKind, ErrorCode, PROTOCOL_VERSION,
    ServerMessage,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::{DirectoryEntry, EntryKind, FilePeekerError};

pub(crate) async fn collect(
    socket_path: PathBuf,
    path: String,
) -> Result<Vec<DirectoryEntry>, FilePeekerError> {
    let path = absolute_utf8_path(&path)?;
    let mut stream = UnixStream::connect(&socket_path).await.map_err(|error| {
        FilePeekerError::ConnectionClosed {
            message: format!("cannot open listing connection: {error}"),
        }
    })?;
    handshake_operation(&mut stream).await?;
    write_message(&mut stream, &ClientMessage::List { path }).await?;

    match read_message(&mut stream).await? {
        ServerMessage::ListResult { entries } => Ok(entries
            .into_iter()
            .map(|entry| DirectoryEntry {
                path: entry.path,
                name: entry.name,
                kind: map_entry_kind(entry.kind),
                navigable: entry.navigable,
            })
            .collect()),
        ServerMessage::Error { code, message } => Err(map_server_error(code, message)),
        message => Err(FilePeekerError::Protocol {
            message: format!("unexpected listing response: {message:?}"),
        }),
    }
}

pub(crate) async fn current_root(socket_path: PathBuf) -> Result<String, FilePeekerError> {
    let mut stream = UnixStream::connect(&socket_path).await.map_err(|error| {
        FilePeekerError::ConnectionClosed {
            message: format!("cannot open current-root connection: {error}"),
        }
    })?;
    handshake_operation(&mut stream).await?;
    write_message(&mut stream, &ClientMessage::CurrentRoot).await?;
    match read_message(&mut stream).await? {
        ServerMessage::CurrentRoot { path } => Ok(path),
        ServerMessage::Error { code, message } => Err(map_server_error(code, message)),
        message => Err(FilePeekerError::Protocol {
            message: format!("unexpected current-root response: {message:?}"),
        }),
    }
}

pub(crate) fn absolute_utf8_path(path: &str) -> Result<String, FilePeekerError> {
    if path.is_empty() {
        return Err(FilePeekerError::InvalidPath {
            message: "path is required".into(),
        });
    }
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| FilePeekerError::Io {
                message: format!("cannot read current directory: {error}"),
            })?
            .join(path)
    };
    absolute
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| FilePeekerError::InvalidPath {
            message: "path must be valid UTF-8".into(),
        })
}

async fn handshake_operation(stream: &mut UnixStream) -> Result<(), FilePeekerError> {
    write_message(
        stream,
        &ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            role: ConnectionRole::Operation,
        },
    )
    .await?;
    match read_message(stream).await? {
        ServerMessage::HelloOk { version } if version == PROTOCOL_VERSION => Ok(()),
        ServerMessage::Error { code, message } => Err(map_server_error(code, message)),
        message => Err(FilePeekerError::Protocol {
            message: format!("unexpected operation handshake response: {message:?}"),
        }),
    }
}

async fn write_message(
    stream: &mut UnixStream,
    message: &ClientMessage,
) -> Result<(), FilePeekerError> {
    let mut bytes = serde_json::to_vec(message).map_err(|error| FilePeekerError::Protocol {
        message: format!("cannot encode client message: {error}"),
    })?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .await
        .map_err(|error| FilePeekerError::ConnectionClosed {
            message: format!("cannot write server request: {error}"),
        })
}

async fn read_message(stream: &mut UnixStream) -> Result<ServerMessage, FilePeekerError> {
    let mut bytes = Vec::new();
    let count = BufReader::new(stream)
        .read_until(b'\n', &mut bytes)
        .await
        .map_err(|error| FilePeekerError::ConnectionClosed {
            message: format!("cannot read server response: {error}"),
        })?;
    if count == 0 {
        return Err(FilePeekerError::ConnectionClosed {
            message: "server closed the operation connection".into(),
        });
    }
    if bytes.last() != Some(&b'\n') {
        return Err(FilePeekerError::ConnectionClosed {
            message: "server closed before completing the operation response".into(),
        });
    }
    bytes.pop();
    serde_json::from_slice(&bytes).map_err(|error| FilePeekerError::Protocol {
        message: format!("server returned invalid JSON: {error}"),
    })
}

fn map_entry_kind(kind: ProtocolEntryKind) -> EntryKind {
    match kind {
        ProtocolEntryKind::File => EntryKind::File,
        ProtocolEntryKind::Directory => EntryKind::Directory,
        ProtocolEntryKind::Symlink => EntryKind::Symlink,
        ProtocolEntryKind::Other => EntryKind::Other,
    }
}

fn map_server_error(code: ErrorCode, message: String) -> FilePeekerError {
    match code {
        ErrorCode::InvalidPath => FilePeekerError::InvalidPath { message },
        ErrorCode::NotFound
        | ErrorCode::PermissionDenied
        | ErrorCode::NotDirectory
        | ErrorCode::Io => FilePeekerError::Io { message },
        ErrorCode::UnsupportedVersion => FilePeekerError::Protocol { message },
    }
}

#[cfg(test)]
mod tests {
    use file_peeker_protocol::{
        ClientMessage, ConnectionRole, EntryKind, ListingEntry, PROTOCOL_VERSION, ServerMessage,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{UnixListener, UnixStream},
    };

    use super::{absolute_utf8_path, collect};

    #[test]
    fn relative_paths_become_absolute() {
        let path = absolute_utf8_path(".").expect("relative path should normalize");
        assert!(std::path::Path::new(&path).is_absolute());
    }

    #[tokio::test]
    async fn listing_response_may_exceed_one_mib() {
        let directory = tempfile::Builder::new()
            .prefix("fp-client-list-test-")
            .tempdir_in("/tmp")
            .expect("temporary directory should be created");
        let socket_path = directory.path().join("server.sock");
        let listener = UnixListener::bind(&socket_path).expect("test server should bind");
        let large_name = "x".repeat(1024 * 1024 + 1);
        let expected_name = large_name.clone();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("client should connect");
            assert_eq!(
                read_client_message(&mut stream).await,
                ClientMessage::Hello {
                    version: PROTOCOL_VERSION,
                    role: ConnectionRole::Operation,
                }
            );
            write_server_message(
                &mut stream,
                &ServerMessage::HelloOk {
                    version: PROTOCOL_VERSION,
                },
            )
            .await;
            assert_eq!(
                read_client_message(&mut stream).await,
                ClientMessage::List {
                    path: "/fixture".into()
                }
            );
            write_server_message(
                &mut stream,
                &ServerMessage::ListResult {
                    entries: vec![ListingEntry {
                        path: "/fixture/large".into(),
                        name: large_name,
                        kind: EntryKind::File,
                        navigable: false,
                    }],
                },
            )
            .await;
        });

        let entries = collect(socket_path, "/fixture".into())
            .await
            .expect("large listing result should be accepted");
        server.await.expect("test server should complete");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, expected_name);
    }

    async fn read_client_message(stream: &mut UnixStream) -> ClientMessage {
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0_u8; 1];
            stream
                .read_exact(&mut byte)
                .await
                .expect("client message should be readable");
            if byte[0] == b'\n' {
                break;
            }
            bytes.push(byte[0]);
        }
        serde_json::from_slice(&bytes).expect("client message should decode")
    }

    async fn write_server_message(stream: &mut UnixStream, message: &ServerMessage) {
        let mut bytes = serde_json::to_vec(message).expect("server message should encode");
        bytes.push(b'\n');
        stream
            .write_all(&bytes)
            .await
            .expect("server message should be writable");
    }
}
