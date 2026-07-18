use std::{
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use file_peeker_protocol::{
    ClientMessage, ConnectionRole, EntryKind as ProtocolEntryKind, ErrorCode, PROTOCOL_VERSION,
    ServerMessage,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::Mutex,
};

use crate::{DirectoryEntry, EntryKind, FilePeekerError, session::Session};

const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub(crate) struct Listing {
    _session: Arc<Session>,
    state: Mutex<ListingState>,
    next_in_progress: AtomicBool,
}

#[derive(Debug)]
enum ListingState {
    Active(ActiveListing),
    Complete,
    Failed(FilePeekerError),
}

#[derive(Debug)]
struct ActiveListing {
    reader: BufReader<UnixStream>,
    frame: Vec<u8>,
    parent_path: String,
}

enum ListingStep {
    Batch(Vec<DirectoryEntry>),
    End,
}

impl Listing {
    pub(crate) async fn start(
        session: Arc<Session>,
        path: String,
    ) -> Result<Arc<Self>, FilePeekerError> {
        session.ensure_open()?;
        let path = absolute_utf8_path(&path)?;
        let mut stream = UnixStream::connect(session.socket_path())
            .await
            .map_err(|error| FilePeekerError::ConnectionClosed {
                message: format!("cannot open listing connection: {error}"),
            })?;
        handshake_operation(&mut stream).await?;
        write_message(&mut stream, &ClientMessage::List { path: path.clone() }).await?;

        Ok(Arc::new(Self {
            _session: session,
            state: Mutex::new(ListingState::Active(ActiveListing {
                reader: BufReader::new(stream),
                frame: Vec::new(),
                parent_path: path,
            })),
            next_in_progress: AtomicBool::new(false),
        }))
    }

    pub(crate) async fn next_batch(&self) -> Result<Option<Vec<DirectoryEntry>>, FilePeekerError> {
        if self
            .next_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(FilePeekerError::Protocol {
                message: "Listing.next_batch cannot be called concurrently".into(),
            });
        }
        let _lease = NextLease(&self.next_in_progress);
        let mut state = self.state.lock().await;

        let result = match &mut *state {
            ListingState::Active(active) => read_listing_step(active).await,
            ListingState::Complete => return Ok(None),
            ListingState::Failed(error) => return Err(error.clone()),
        };
        match result {
            Ok(ListingStep::Batch(entries)) => Ok(Some(entries)),
            Ok(ListingStep::End) => {
                *state = ListingState::Complete;
                Ok(None)
            }
            Err(error) => {
                *state = ListingState::Failed(error.clone());
                Err(error)
            }
        }
    }
}

struct NextLease<'a>(&'a AtomicBool);

impl Drop for NextLease<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

async fn read_listing_step(active: &mut ActiveListing) -> Result<ListingStep, FilePeekerError> {
    let message = read_buffered_message(active).await?;
    match message {
        ServerMessage::ListBatch { entries } if entries.is_empty() => {
            Err(FilePeekerError::Protocol {
                message: "server returned an empty listing batch".into(),
            })
        }
        ServerMessage::ListBatch { entries } => entries
            .into_iter()
            .map(|entry| {
                let path = child_path(&active.parent_path, &entry.name)?;
                Ok(DirectoryEntry {
                    path,
                    name: entry.name,
                    kind: map_entry_kind(entry.kind),
                    navigable: entry.navigable,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(ListingStep::Batch),
        ServerMessage::ListEnd => Ok(ListingStep::End),
        ServerMessage::Error { code, message } => Err(map_server_error(code, message)),
        message => Err(FilePeekerError::Protocol {
            message: format!("unexpected listing response: {message:?}"),
        }),
    }
}

async fn read_buffered_message(
    active: &mut ActiveListing,
) -> Result<ServerMessage, FilePeekerError> {
    loop {
        let available = active
            .reader
            .fill_buf()
            .await
            .map_err(|error| connection_read_error(&error))?;
        if available.is_empty() {
            return Err(FilePeekerError::ConnectionClosed {
                message: if active.frame.is_empty() {
                    "server closed before completing the listing".into()
                } else {
                    "server closed before completing an operation frame".into()
                },
            });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let retained = newline.unwrap_or(available.len());
        if active.frame.len().saturating_add(retained) > MAX_FRAME_BYTES {
            return Err(FilePeekerError::Protocol {
                message: format!("server response exceeds {MAX_FRAME_BYTES} bytes"),
            });
        }
        active.frame.extend_from_slice(&available[..retained]);
        active
            .reader
            .consume(retained + usize::from(newline.is_some()));
        if newline.is_some() {
            break;
        }
    }

    let frame = std::mem::take(&mut active.frame);
    serde_json::from_slice(&frame).map_err(|error| FilePeekerError::Protocol {
        message: format!("server returned invalid JSON: {error}"),
    })
}

fn child_path(parent: &str, name: &str) -> Result<String, FilePeekerError> {
    let mut components = Path::new(name).components();
    let valid =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if !valid {
        return Err(FilePeekerError::Protocol {
            message: format!("server returned an invalid child name: {name:?}"),
        });
    }
    Path::new(parent)
        .join(name)
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| FilePeekerError::InvalidPath {
            message: "listed child path must be valid UTF-8".into(),
        })
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
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(FilePeekerError::Protocol {
            message: format!("client request exceeds {MAX_FRAME_BYTES} bytes"),
        });
    }
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
        let count = stream
            .read(&mut byte)
            .await
            .map_err(|error| connection_read_error(&error))?;
        if count == 0 {
            return Err(FilePeekerError::ConnectionClosed {
                message: "server closed before completing the operation response".into(),
            });
        }
        if byte[0] == b'\n' {
            break;
        }
        if bytes.len() == MAX_FRAME_BYTES {
            return Err(FilePeekerError::Protocol {
                message: format!("server response exceeds {MAX_FRAME_BYTES} bytes"),
            });
        }
        bytes.push(byte[0]);
    }
    serde_json::from_slice(&bytes).map_err(|error| FilePeekerError::Protocol {
        message: format!("server returned invalid JSON: {error}"),
    })
}

fn connection_read_error(error: &std::io::Error) -> FilePeekerError {
    FilePeekerError::ConnectionClosed {
        message: format!("cannot read server response: {error}"),
    }
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
    use file_peeker_protocol::{EntryKind, ListingEntry, ServerMessage};
    use tokio::{io::AsyncWriteExt, net::UnixStream};

    use super::{ActiveListing, ListingStep, absolute_utf8_path, child_path, read_listing_step};

    #[test]
    fn relative_paths_become_absolute() {
        let path = absolute_utf8_path(".").expect("relative path should normalize");
        assert!(std::path::Path::new(&path).is_absolute());
    }

    #[test]
    fn child_names_must_be_one_normal_component() {
        assert_eq!(
            child_path("/fixture", "notes.txt").unwrap(),
            "/fixture/notes.txt"
        );
        assert!(child_path("/fixture", "../escape").is_err());
        assert!(child_path("/fixture", "nested/name").is_err());
        assert!(child_path("/fixture", ".").is_err());
    }

    #[tokio::test]
    async fn persistent_reader_preserves_coalesced_listing_frames() {
        let (client, mut server) = UnixStream::pair().expect("socket pair should be created");
        let messages = [
            ServerMessage::ListBatch {
                entries: vec![ListingEntry {
                    name: "first.txt".into(),
                    kind: EntryKind::File,
                    navigable: false,
                }],
            },
            ServerMessage::ListBatch {
                entries: vec![ListingEntry {
                    name: "docs".into(),
                    kind: EntryKind::Directory,
                    navigable: true,
                }],
            },
            ServerMessage::ListEnd,
        ];
        let mut encoded = Vec::new();
        for message in messages {
            encoded.extend(serde_json::to_vec(&message).unwrap());
            encoded.push(b'\n');
        }
        server.write_all(&encoded).await.unwrap();
        drop(server);

        let mut active = ActiveListing {
            reader: tokio::io::BufReader::new(client),
            frame: Vec::new(),
            parent_path: "/fixture".into(),
        };
        let ListingStep::Batch(first) = read_listing_step(&mut active).await.unwrap() else {
            panic!("first response should be a batch");
        };
        let ListingStep::Batch(second) = read_listing_step(&mut active).await.unwrap() else {
            panic!("second response should be a batch");
        };
        assert_eq!(first[0].path, "/fixture/first.txt");
        assert_eq!(second[0].path, "/fixture/docs");
        assert!(matches!(
            read_listing_step(&mut active).await.unwrap(),
            ListingStep::End
        ));
    }

    #[tokio::test]
    async fn rejects_empty_listing_batches() {
        let (client, mut server) = UnixStream::pair().expect("socket pair should be created");
        let mut encoded =
            serde_json::to_vec(&ServerMessage::ListBatch { entries: vec![] }).unwrap();
        encoded.push(b'\n');
        server.write_all(&encoded).await.unwrap();

        let mut active = ActiveListing {
            reader: tokio::io::BufReader::new(client),
            frame: Vec::new(),
            parent_path: "/fixture".into(),
        };
        assert!(read_listing_step(&mut active).await.is_err());
    }
}
