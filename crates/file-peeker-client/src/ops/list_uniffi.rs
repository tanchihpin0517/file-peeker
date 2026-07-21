use std::{fmt, io, sync::Arc};

use futures::TryStreamExt;
use thiserror::Error;
use tokio::sync::Mutex;

use super::list::{DirectoryEntry, ListStream};

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
    /// Returns the next native listing batch, or `None` after completion.
    ///
    /// # Errors
    ///
    /// Returns the sticky terminal transport error when listing fails.
    pub async fn next_batch(&self) -> Result<Option<Vec<DirectoryEntry>>, ListError> {
        let mut state = self.state.lock().await;
        match &mut *state {
            ListingState::Active(stream) => match stream.try_next().await {
                Ok(Some(entries)) => Ok(Some(entries)),
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
    use super::{DirectoryEntry, ListError, Listing};
    use crate::EntryKind;
    use futures::{StreamExt, stream};
    use std::io;

    fn entry(name: &str) -> DirectoryEntry {
        DirectoryEntry {
            name: name.into(),
            kind: EntryKind::File,
            navigable: false,
        }
    }

    #[tokio::test]
    async fn yields_batches_then_completes_idempotently() {
        let listing = Listing::new(stream::iter([Ok(vec![entry("one"), entry("two")])]).boxed());
        assert_eq!(
            listing.next_batch().await.unwrap(),
            Some(vec![entry("one"), entry("two")])
        );
        assert_eq!(listing.next_batch().await.unwrap(), None);
        assert_eq!(listing.next_batch().await.unwrap(), None);
    }

    #[tokio::test]
    async fn repeats_terminal_stream_errors() {
        let listing = Listing::new(
            stream::iter([Err(io::Error::new(io::ErrorKind::UnexpectedEof, "closed"))]).boxed(),
        );
        let first = listing.next_batch().await.unwrap_err();
        assert_eq!(listing.next_batch().await.unwrap_err(), first);
        assert!(matches!(first, ListError::Operation { .. }));
    }
}
