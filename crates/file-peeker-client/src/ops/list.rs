use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use file_peeker_protocol::{
    ClientMessage, ConnectionRole, EntryKind as ProtocolEntryKind, ErrorCode, MAX_MESSAGE_BYTES,
    PROTOCOL_VERSION, ServerMessage,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::{Mutex, mpsc},
    task::JoinHandle,
};

use crate::{DirectoryEntry, EntryKind, FilePeekerError};

const LISTING_QUEUE_CAPACITY: usize = 64;

#[derive(Debug)]
pub(crate) struct DirectoryListing {
    state: Arc<ListingState>,
}

impl DirectoryListing {
    /// Waits for the next directory entry or successful completion.
    ///
    /// # Errors
    ///
    /// Returns the next streamed entry or `None` when listing is complete.
    pub(crate) async fn next_entry(&self) -> Result<Option<DirectoryEntry>, FilePeekerError> {
        next(&self.state).await
    }
}

#[derive(Debug)]
enum ListingItem {
    Entry(DirectoryEntry),
    Done,
    Error(FilePeekerError),
}

#[derive(Debug)]
struct ListingState {
    receiver: Mutex<mpsc::Receiver<ListingItem>>,
    task: JoinHandle<()>,
    finished: AtomicBool,
}

impl Drop for ListingState {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start(socket_path: PathBuf, path: String) -> Result<Arc<ListingState>, FilePeekerError> {
    let path = absolute_utf8_path(&path)?;
    let mut stream = UnixStream::connect(&socket_path).await.map_err(|error| {
        FilePeekerError::ConnectionClosed {
            message: format!("cannot open listing connection: {error}"),
        }
    })?;
    handshake_operation(&mut stream).await?;

    let (sender, receiver) = mpsc::channel(LISTING_QUEUE_CAPACITY);
    let task = tokio::spawn(async move {
        if let Err(error) = run_listing(stream, path, &sender).await {
            let _ = sender.send(ListingItem::Error(error)).await;
        }
    });

    Ok(Arc::new(ListingState {
        receiver: Mutex::new(receiver),
        task,
        finished: AtomicBool::new(false),
    }))
}

pub(crate) async fn list(
    socket_path: PathBuf,
    path: String,
) -> Result<Arc<DirectoryListing>, FilePeekerError> {
    let state = start(socket_path, path).await?;
    Ok(Arc::new(DirectoryListing { state }))
}

pub(crate) async fn collect(
    socket_path: PathBuf,
    path: String,
) -> Result<Vec<DirectoryEntry>, FilePeekerError> {
    let state = start(socket_path, path).await?;
    let mut entries = Vec::new();
    while let Some(entry) = next(&state).await? {
        entries.push(entry);
    }
    Ok(entries)
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

async fn next(state: &ListingState) -> Result<Option<DirectoryEntry>, FilePeekerError> {
    if state.finished.load(Ordering::Acquire) {
        return Ok(None);
    }

    match state.receiver.lock().await.recv().await {
        Some(ListingItem::Entry(entry)) => Ok(Some(entry)),
        Some(ListingItem::Done) => {
            state.finished.store(true, Ordering::Release);
            Ok(None)
        }
        Some(ListingItem::Error(error)) => {
            state.finished.store(true, Ordering::Release);
            Err(error)
        }
        None => {
            state.finished.store(true, Ordering::Release);
            Err(FilePeekerError::ConnectionClosed {
                message: "listing task ended without a terminal response".into(),
            })
        }
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

async fn run_listing(
    mut stream: UnixStream,
    path: String,
    sender: &mpsc::Sender<ListingItem>,
) -> Result<(), FilePeekerError> {
    write_message(&mut stream, &ClientMessage::List { path }).await?;
    loop {
        match read_message(&mut stream).await? {
            ServerMessage::Entry {
                path,
                name,
                kind,
                navigable,
            } => {
                let entry = DirectoryEntry {
                    path,
                    name,
                    kind: map_entry_kind(kind),
                    navigable,
                };
                sender.send(ListingItem::Entry(entry)).await.map_err(|_| {
                    FilePeekerError::ConnectionClosed {
                        message: "listing was cancelled".into(),
                    }
                })?;
            }
            ServerMessage::Done => {
                let _ = sender.send(ListingItem::Done).await;
                return Ok(());
            }
            ServerMessage::Error { code, message } => return Err(map_server_error(code, message)),
            message => {
                return Err(FilePeekerError::Protocol {
                    message: format!("unexpected listing response: {message:?}"),
                });
            }
        }
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
    loop {
        let mut byte = [0_u8; 1];
        let count =
            stream
                .read(&mut byte)
                .await
                .map_err(|error| FilePeekerError::ConnectionClosed {
                    message: format!("cannot read server response: {error}"),
                })?;
        if count == 0 {
            return Err(FilePeekerError::ConnectionClosed {
                message: "server closed the operation connection".into(),
            });
        }
        if byte[0] == b'\n' {
            break;
        }
        bytes.push(byte[0]);
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(FilePeekerError::Protocol {
                message: "server response exceeds the size limit".into(),
            });
        }
    }
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
    use super::absolute_utf8_path;

    #[test]
    fn relative_paths_become_absolute() {
        let path = absolute_utf8_path(".").expect("relative path should normalize");
        assert!(std::path::Path::new(&path).is_absolute());
    }
}
