use std::io;

use futures::stream::BoxStream;

use crate::session::{Session, rpc};

pub(crate) type WalkEntry = crate::fs::WalkEntry;
pub(crate) type WalkStream = BoxStream<'static, io::Result<WalkEntry>>;

impl Session {
    /// Starts a native, pull-based recursive traversal on the selected host.
    ///
    /// The root is excluded, direct children have depth 1, traversal is
    /// pre-order depth-first, and symlinks are never followed.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the Session is closed or the selected-host
    /// traversal cannot be started. Later errors are terminal stream items.
    pub(crate) async fn op_walk_dir(&self, path: &str) -> io::Result<WalkStream> {
        let connection = self.connection().await?;
        rpc::walk_dir(&connection, path).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::fs::{DirectoryEntry, EntryKind};

    use super::{Session, WalkEntry};
    use crate::SessionTarget;

    #[tokio::test]
    async fn rejects_closed_sessions() {
        let error = Session::closed_for_test("walk-test")
            .op_walk_dir("/fixture")
            .await
            .err()
            .expect("closed Session should reject walking");
        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
    }

    /// `op_walk_dir` hands back an owned `'static` stream, so the connection
    /// read lock is released before the caller sees it. Holding one open must
    /// therefore never delay shutdown.
    #[tokio::test]
    async fn close_is_not_blocked_by_an_outstanding_walk_stream() {
        let fixture = tempfile::tempdir().unwrap();
        for index in 0..64 {
            tokio::fs::write(fixture.path().join(format!("entry-{index}")), b"")
                .await
                .unwrap();
        }
        let session = Session::start("walk-test".into(), SessionTarget::Local)
            .await
            .unwrap();
        let _walk = session
            .op_walk_dir(fixture.path().to_str().unwrap())
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(5), session.close())
            .await
            .expect("close must not wait on an outstanding walk stream")
            .unwrap();
    }

    #[test]
    fn walk_entry_shape_is_transport_independent() {
        let entry = WalkEntry {
            relative_path: "dir/file".into(),
            entry: DirectoryEntry {
                name: "file".into(),
                kind: EntryKind::File,
                navigable: false,
            },
            depth: 2,
        };
        assert_eq!(entry.depth, 2);
    }
}
