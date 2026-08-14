use std::io;

use futures::stream::BoxStream;

use crate::client::{Client, ConnectionId, rpc};
use crate::host_path::HostPath;

pub(crate) type WalkEntry = crate::fs::WalkEntry;
pub(crate) type WalkStream = BoxStream<'static, io::Result<WalkEntry>>;

impl Client {
    /// Starts a native, pull-based recursive traversal on the selected host.
    ///
    /// The root is excluded, direct children have depth 1, traversal is
    /// pre-order depth-first, and symlinks are never followed.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the connection is closed or the selected-host
    /// traversal cannot be started. Later errors are terminal stream items.
    pub(crate) async fn op_walk_dir(
        &self,
        id: &ConnectionId,
        path: &HostPath,
    ) -> io::Result<WalkStream> {
        let connection = self.connection(id).await?;
        rpc::walk_dir(&connection, path).await
    }
}

#[cfg(test)]
mod tests {
    use crate::host_path::HostPath;
    use std::time::Duration;

    use crate::fs::{DirectoryEntry, EntryKind};

    use super::WalkEntry;
    use crate::client::{Client, connection::ServerLocation};

    #[tokio::test]
    async fn rejects_closed_connections() {
        let (client, id) = Client::closed_for_test();

        let error = client
            .op_walk_dir(&id, &"/fixture".into())
            .await
            .err()
            .expect("a closed connection should reject walking");

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
    }

    /// `op_walk_dir` hands back an owned `'static` stream, so the connection
    /// read lock is released before the caller sees it. Holding one open must
    /// therefore never delay close.
    #[tokio::test]
    async fn close_is_not_blocked_by_an_outstanding_walk_stream() {
        let fixture = tempfile::tempdir().unwrap();
        for index in 0..64 {
            tokio::fs::write(fixture.path().join(format!("entry-{index}")), b"")
                .await
                .unwrap();
        }
        let client = Client::new();
        let id = client.open(ServerLocation::Embedded).await.unwrap();
        let _walk = client
            .op_walk_dir(&id, &HostPath::from_path(fixture.path()))
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(5), client.close(&id))
            .await
            .expect("close must not wait on an outstanding walk stream")
            .unwrap();
    }

    #[test]
    fn walk_entry_shape_is_transport_independent() {
        let entry = WalkEntry {
            relative_path: "dir/file".into(),
            entry: DirectoryEntry::readable("file".into(), EntryKind::File, false),
            depth: 2,
        };
        assert_eq!(entry.depth, 2);
    }
}
