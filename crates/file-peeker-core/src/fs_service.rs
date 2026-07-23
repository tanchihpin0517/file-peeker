use tokio_util::sync::CancellationToken;

use crate::{
    directory::{EntryStream, WalkStream, list_dir, walk_dir},
    error::{FsError, FsErrorKind, service_cancelled_error},
    read::{ReadStream, read_file},
    resolve_path::resolve_path,
};

/// A clonable filesystem service whose clones share one terminal cancellation state.
#[derive(Clone, Debug, Default)]
pub struct FsService {
    cancellation_token: CancellationToken,
}

impl FsService {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Expands and lexically normalizes a path into an absolute UTF-8 path.
    ///
    /// This operation does not access the target path, require it to exist, or
    /// resolve symbolic links.
    ///
    /// # Errors
    ///
    /// Returns an error when shell expansion fails, the working directory cannot
    /// be read, the resolved path cannot be represented as UTF-8, or the service
    /// has been cancelled.
    pub fn resolve_path(&self, path: &str) -> Result<String, FsError> {
        self.ensure_active()?;
        resolve_path(path)?
            .into_os_string()
            .into_string()
            .map_err(|_| FsError::new(FsErrorKind::InvalidArgument, "Resolved path is not UTF-8"))
    }

    /// Starts a stream of directory entries.
    ///
    /// # Errors
    ///
    /// Returns an error when the path cannot be expanded or opened as a
    /// directory, or when the service has been cancelled. Enumeration and
    /// cancellation errors can also be emitted by the returned stream.
    pub async fn list_dir(&self, path: &str) -> Result<EntryStream, FsError> {
        self.ensure_active()?;
        list_dir(path, self.cancellation_token.clone()).await
    }

    /// Starts a pull-based, pre-order depth-first traversal below a directory.
    ///
    /// The root is excluded, direct children have depth 1, sibling order is
    /// filesystem-native, and symlinks are emitted but never followed.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be opened as a directory or the
    /// service is cancelled. Descendant and cancellation errors are emitted
    /// terminally after any earlier entries.
    pub async fn walk_dir(&self, path: &str) -> Result<WalkStream, FsError> {
        self.ensure_active()?;
        walk_dir(path, self.cancellation_token.clone()).await
    }

    /// Opens a file as an asynchronously consumed byte stream.
    ///
    /// # Errors
    ///
    /// Returns an ordered stream of non-empty byte chunks from the start of a file.
    ///
    /// Chunk boundaries and sizes have no semantic meaning, and an empty file
    /// produces an empty stream. Returns an error when the path cannot be expanded
    /// or the file cannot be opened as a regular file, or when the service has
    /// already been cancelled. Later filesystem and cancellation failures are
    /// emitted once by the stream before it terminates.
    pub async fn read_file(&self, path: &str) -> Result<ReadStream, FsError> {
        self.ensure_active()?;
        read_file(path, self.cancellation_token.clone()).await
    }

    /// Permanently cancels this service, all of its clones, and active streams.
    ///
    /// Every clone shares one cancellation group. Repeated calls are harmless,
    /// and operations started after cancellation return [`FsErrorKind::Cancelled`].
    pub fn cancel(&self) {
        self.cancellation_token.cancel();
    }

    fn ensure_active(&self) -> Result<(), FsError> {
        if self.cancellation_token.is_cancelled() {
            Err(service_cancelled_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FsErrorKind, FsService};

    #[test]
    fn service_exposes_resolved_utf8_paths() {
        let service = FsService::new();
        let resolved = service.resolve_path(".").unwrap();
        assert!(std::path::Path::new(&resolved).is_absolute());
    }

    #[tokio::test]
    async fn cancellation_is_terminal_for_every_clone_and_operation() {
        let service = FsService::new();
        let clone = service.clone();
        service.cancel();
        service.cancel();

        assert_eq!(
            clone.resolve_path(".").unwrap_err().kind(),
            FsErrorKind::Cancelled
        );
        assert_eq!(
            clone
                .list_dir(".")
                .await
                .err()
                .expect("listing after cancellation should fail")
                .kind(),
            FsErrorKind::Cancelled
        );
        assert_eq!(
            clone
                .walk_dir(".")
                .await
                .err()
                .expect("walking after cancellation should fail")
                .kind(),
            FsErrorKind::Cancelled
        );
        assert_eq!(
            clone
                .read_file(".")
                .await
                .err()
                .expect("reading after cancellation should fail")
                .kind(),
            FsErrorKind::Cancelled
        );
    }
}
