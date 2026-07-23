use std::{fmt, io, sync::Arc};

use futures::TryStreamExt;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::{DirectoryEntry, EntryStream};

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
    Active(EntryStream),
    Complete,
    Failed(ListError),
}

impl fmt::Debug for Listing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Listing").finish_non_exhaustive()
    }
}

impl Listing {
    pub(crate) fn new(stream: EntryStream) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ListingState::Active(stream)),
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Listing {
    /// Returns the next directory entry, or `None` after completion.
    ///
    /// # Errors
    ///
    /// Returns the sticky terminal transport error when listing fails.
    pub async fn next_entry(&self) -> Result<Option<DirectoryEntry>, ListError> {
        let mut state = self.state.lock().await;
        match &mut *state {
            ListingState::Active(stream) => match stream.try_next().await {
                Ok(Some(entry)) => Ok(Some(entry)),
                Ok(None) => {
                    *state = ListingState::Complete;
                    Ok(None)
                }
                Err(error) => {
                    let error = ListError::from(error);
                    *state = ListingState::Failed(error.clone());
                    Err(error)
                }
            },
            ListingState::Complete => Ok(None),
            ListingState::Failed(error) => Err(error.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use futures::{StreamExt, stream};

    use super::{DirectoryEntry, ListError, Listing};
    use crate::EntryKind;

    fn entry(name: &str) -> DirectoryEntry {
        DirectoryEntry {
            name: name.into(),
            kind: EntryKind::File,
            navigable: false,
        }
    }

    #[tokio::test]
    async fn yields_entries_then_completes_idempotently() {
        let listing = Listing::new(stream::iter([Ok(entry("one")), Ok(entry("two"))]).boxed());
        assert_eq!(listing.next_entry().await.unwrap(), Some(entry("one")));
        assert_eq!(listing.next_entry().await.unwrap(), Some(entry("two")));
        assert_eq!(listing.next_entry().await.unwrap(), None);
        assert_eq!(listing.next_entry().await.unwrap(), None);
    }

    #[tokio::test]
    async fn repeats_terminal_stream_errors() {
        let listing = Listing::new(
            stream::iter([Err(io::Error::new(io::ErrorKind::UnexpectedEof, "closed"))]).boxed(),
        );
        let first = listing.next_entry().await.unwrap_err();
        assert_eq!(listing.next_entry().await.unwrap_err(), first);
        assert!(matches!(first, ListError::Operation { .. }));
    }
}
