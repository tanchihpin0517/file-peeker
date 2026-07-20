use std::{fmt, io, sync::Arc};

use file_peeker_protocol::{EntryKind as ProtocolEntryKind, ListingEntry};
use futures::TryStreamExt;
use thiserror::Error;
use tokio::sync::Mutex;

use super::list::ListStream;

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
    Active(ListStream),
    Complete,
    Failed(ListError),
}

impl fmt::Debug for Listing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Listing").finish_non_exhaustive()
    }
}

impl Listing {
    pub(crate) fn new(stream: ListStream) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ListingState::Active(stream)),
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Listing {
    /// Returns the next entry, or `None` after the listing completes.
    ///
    /// # Errors
    ///
    /// Returns a listing error when the underlying Rust stream fails.
    pub async fn next(&self) -> Result<Option<DirectoryEntry>, ListError> {
        let mut state = self.state.lock().await;
        let result = match &mut *state {
            ListingState::Active(stream) => stream.try_next().await,
            ListingState::Complete => return Ok(None),
            ListingState::Failed(error) => return Err(error.clone()),
        };

        match result {
            Ok(Some(entry)) => Ok(Some(entry.into())),
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

impl From<ListingEntry> for DirectoryEntry {
    fn from(entry: ListingEntry) -> Self {
        Self {
            name: entry.name,
            kind: entry.kind.into(),
            navigable: entry.navigable,
        }
    }
}

impl From<ProtocolEntryKind> for EntryKind {
    fn from(kind: ProtocolEntryKind) -> Self {
        match kind {
            ProtocolEntryKind::File => Self::File,
            ProtocolEntryKind::Directory => Self::Directory,
            ProtocolEntryKind::Symlink => Self::Symlink,
            ProtocolEntryKind::Other => Self::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use file_peeker_protocol::{EntryKind as ProtocolEntryKind, ListingEntry};
    use futures::{StreamExt, stream};

    use super::{DirectoryEntry, EntryKind, ListError, Listing};

    #[tokio::test]
    async fn yields_converted_entries_then_completes_idempotently() {
        let stream = stream::iter([
            Ok(ListingEntry {
                name: "notes.txt".into(),
                kind: ProtocolEntryKind::File,
                navigable: false,
            }),
            Ok(ListingEntry {
                name: "docs".into(),
                kind: ProtocolEntryKind::Directory,
                navigable: true,
            }),
        ])
        .boxed();
        let listing = Listing::new(stream);

        assert_eq!(
            listing.next().await.unwrap(),
            Some(DirectoryEntry {
                name: "notes.txt".into(),
                kind: EntryKind::File,
                navigable: false,
            })
        );
        assert_eq!(listing.next().await.unwrap().unwrap().name, "docs");
        assert_eq!(listing.next().await.unwrap(), None);
        assert_eq!(listing.next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn repeats_terminal_stream_errors() {
        let stream = stream::iter([Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "listing connection closed",
        ))])
        .boxed();
        let listing = Listing::new(stream);

        let first = listing.next().await.unwrap_err();
        let second = listing.next().await.unwrap_err();

        assert_eq!(first, second);
        assert_eq!(
            first,
            ListError::Operation {
                message: "listing connection closed".into()
            }
        );
    }
}
