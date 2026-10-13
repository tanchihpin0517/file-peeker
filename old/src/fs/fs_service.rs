use tokio_util::sync::CancellationToken;

use crate::{
    fs::{
        directory::{EntryStream, WalkStream, list_dir, walk_dir},
        error::{FsError, service_cancelled_error},
        read::{ReadStream, read_file},
        resolve_path::resolve_path,
        write::{WriteConflict, WriteResult, write_file},
    },
    host_path::HostPath,
};

/// A clonable filesystem service whose clones share one terminal cancellation state.
#[derive(Clone, Debug, Default)]
pub(crate) struct FsService {
    cancellation_token: CancellationToken,
}

impl FsService {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Expands and lexically normalizes a path into an absolute one.
    ///
    /// This operation does not access the target path, require it to exist, or
    /// resolve symbolic links.
    ///
    /// # Errors
    ///
    /// Returns an error when shell expansion fails, the working directory cannot
    /// be read, the path is not correctly escaped, or the service has been
    /// cancelled. A resolved path that is not UTF-8 is no longer a failure: it comes
    /// back escaped like any other.
    pub(crate) fn resolve_path(&self, path: &HostPath) -> Result<HostPath, FsError> {
        self.ensure_active()?;
        Ok(HostPath::from_path(&resolve_path(path)?))
    }

    /// Starts a stream of directory entries.
    ///
    /// # Errors
    ///
    /// Returns an error when the path cannot be expanded or opened as a
    /// directory, or when the service has been cancelled. Enumeration and
    /// cancellation errors can also be emitted by the returned stream.
    pub(crate) async fn list_dir(&self, path: &HostPath) -> Result<EntryStream, FsError> {
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
    pub(crate) async fn walk_dir(&self, path: &HostPath) -> Result<WalkStream, FsError> {
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
    pub(crate) async fn read_file(&self, path: &HostPath) -> Result<ReadStream, FsError> {
        self.ensure_active()?;
        read_file(path, self.cancellation_token.clone()).await
    }

    /// Atomically writes a streamed regular file into a selected-host directory.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid filename, inaccessible destination,
    /// conflict, source/write failure, or service cancellation. Failed writes
    /// never publish a partial destination.
    pub(crate) async fn write_file(
        &self,
        directory: &HostPath,
        name: &HostPath,
        conflict: WriteConflict,
        source: ReadStream,
    ) -> Result<WriteResult, FsError> {
        self.ensure_active()?;
        write_file(
            directory,
            name,
            conflict,
            source,
            self.cancellation_token.clone(),
        )
        .await
    }

    /// Permanently cancels this service, all of its clones, and active streams.
    ///
    /// Every clone shares one cancellation group. Repeated calls are harmless,
    /// and operations started after cancellation return [`FsErrorKind::Cancelled`].
    pub(crate) fn cancel(&self) {
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
    use super::{FsService, HostPath};
    use crate::fs::FsErrorKind;

    /// A path written the way a person would write it.
    fn path(path: &str) -> HostPath {
        HostPath::from_literal(path)
    }

    #[test]
    fn service_exposes_resolved_absolute_paths() {
        let service = FsService::new();
        let resolved = service.resolve_path(&path(".")).unwrap();
        assert!(resolved.to_path_buf().unwrap().is_absolute());
    }

    #[tokio::test]
    async fn cancellation_is_terminal_for_every_clone_and_operation() {
        let service = FsService::new();
        let clone = service.clone();
        service.cancel();
        service.cancel();

        assert_eq!(
            clone.resolve_path(&path(".")).unwrap_err().kind(),
            FsErrorKind::Cancelled
        );
        assert_eq!(
            clone
                .list_dir(&path("."))
                .await
                .err()
                .expect("listing after cancellation should fail")
                .kind(),
            FsErrorKind::Cancelled
        );
        assert_eq!(
            clone
                .walk_dir(&path("."))
                .await
                .err()
                .expect("walking after cancellation should fail")
                .kind(),
            FsErrorKind::Cancelled
        );
        assert_eq!(
            clone
                .read_file(&path("."))
                .await
                .err()
                .expect("reading after cancellation should fail")
                .kind(),
            FsErrorKind::Cancelled
        );
    }
}
