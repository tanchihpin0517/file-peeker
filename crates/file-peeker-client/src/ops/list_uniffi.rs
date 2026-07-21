use std::{fmt, io, sync::Arc};

use file_peeker_protocol::v1::{EntryKind as ProtocolEntryKind, ListingEntry};
use futures::TryStreamExt;
use thiserror::Error;
use tokio::sync::Mutex;

use super::list::ListBatchStream;

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: EntryKind,
    pub navigable: bool,
}

#[derive(Clone, Debug, Eq, Error, PartialEq, uniffi::Error)]
pub enum ListError {
    #[error("listing failed: {message}")]
    Operation { message: String },
}

impl From<io::Error> for ListError {
    fn from(error: io::Error) -> Self {
        Self::Operation {
            message: error.to_string(),
        }
    }
}

#[derive(uniffi::Object)]
pub struct Listing {
    state: Mutex<ListingState>,
}

enum ListingState {
    Active(ListBatchStream),
    Complete,
    Failed(ListError),
}

impl fmt::Debug for Listing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Listing").finish_non_exhaustive()
    }
}

impl Listing {
    pub(crate) fn new(stream: ListBatchStream) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ListingState::Active(stream)),
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Listing {
    /// Returns the next non-empty server batch, or `None` after completion.
    ///
    /// # Errors
    ///
    /// Returns the terminal gRPC or protocol-conversion failure. Repeated calls
    /// return the same failure.
    pub async fn next_batch(&self) -> Result<Option<Vec<DirectoryEntry>>, ListError> {
        let mut state = self.state.lock().await;
        let result = match &mut *state {
            ListingState::Active(stream) => stream.try_next().await,
            ListingState::Complete => return Ok(None),
            ListingState::Failed(error) => return Err(error.clone()),
        };

        match result {
            Ok(Some(entries)) => entries
                .into_iter()
                .map(DirectoryEntry::try_from)
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
                .inspect_err(|error| {
                    *state = ListingState::Failed(error.clone());
                }),
            Ok(None) => {
                *state = ListingState::Complete;
                Ok(None)
            }
            Err(error) => {
                let error = ListError::from(error);
                *state = ListingState::Failed(error.clone());
                Err(error)
            }
        }
    }
}

impl TryFrom<ListingEntry> for DirectoryEntry {
    type Error = ListError;

    fn try_from(entry: ListingEntry) -> Result<Self, Self::Error> {
        let kind = ProtocolEntryKind::try_from(entry.kind).map_err(|_| ListError::Operation {
            message: format!("server returned unknown entry kind {}", entry.kind),
        })?;
        let kind = match kind {
            ProtocolEntryKind::Unspecified => {
                return Err(ListError::Operation {
                    message: "server returned an unspecified entry kind".into(),
                });
            }
            ProtocolEntryKind::File => EntryKind::File,
            ProtocolEntryKind::Directory => EntryKind::Directory,
            ProtocolEntryKind::Symlink => EntryKind::Symlink,
            ProtocolEntryKind::Other => EntryKind::Other,
        };
        Ok(Self {
            name: entry.name,
            kind,
            navigable: entry.navigable,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use file_peeker_protocol::v1::{EntryKind as ProtocolEntryKind, ListingEntry};
    use futures::{StreamExt, stream};

    use super::{DirectoryEntry, EntryKind, ListError, Listing};

    #[tokio::test]
    async fn yields_batches_then_completes_idempotently() {
        let stream = stream::iter([Ok(vec![
            ListingEntry {
                name: "notes.txt".into(),
                kind: ProtocolEntryKind::File.into(),
                navigable: false,
            },
            ListingEntry {
                name: "docs".into(),
                kind: ProtocolEntryKind::Directory.into(),
                navigable: true,
            },
        ])])
        .boxed();
        let listing = Listing::new(stream);

        assert_eq!(
            listing.next_batch().await.unwrap(),
            Some(vec![
                DirectoryEntry {
                    name: "notes.txt".into(),
                    kind: EntryKind::File,
                    navigable: false,
                },
                DirectoryEntry {
                    name: "docs".into(),
                    kind: EntryKind::Directory,
                    navigable: true,
                },
            ])
        );
        assert_eq!(listing.next_batch().await.unwrap(), None);
        assert_eq!(listing.next_batch().await.unwrap(), None);
    }

    #[tokio::test]
    async fn repeats_terminal_stream_errors() {
        let stream = stream::iter([Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "listing connection closed",
        ))])
        .boxed();
        let listing = Listing::new(stream);

        let first = listing.next_batch().await.unwrap_err();
        let second = listing.next_batch().await.unwrap_err();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn rejects_unspecified_entry_kind_stickily() {
        let stream = stream::iter([Ok(vec![ListingEntry {
            name: "unknown".into(),
            kind: ProtocolEntryKind::Unspecified.into(),
            navigable: false,
        }])])
        .boxed();
        let listing = Listing::new(stream);

        let first = listing.next_batch().await.unwrap_err();
        let second = listing.next_batch().await.unwrap_err();
        assert_eq!(first, second);
        assert!(matches!(first, ListError::Operation { .. }));
    }
}
