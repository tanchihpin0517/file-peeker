use std::io;

use futures::stream::BoxStream;

use crate::session::{Session, session_closed};

pub type WalkEntry = file_peeker_core::WalkEntry;
pub type WalkStream = BoxStream<'static, io::Result<WalkEntry>>;

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
    pub async fn op_walk_dir(&self, path: &str) -> io::Result<WalkStream> {
        let backend = self.backend.read().await;
        backend
            .as_ref()
            .ok_or_else(session_closed)?
            .walk_dir(path)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::{io, sync::Arc, time::Duration};

    use async_trait::async_trait;
    use file_peeker_core::{DirectoryEntry, EntryKind};
    use futures::{StreamExt as _, stream};
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    use super::{Session, WalkEntry, WalkStream};
    use crate::{
        EntryStream, SessionTarget,
        session::{
            backend::{ReadStream, SessionBackend},
            file::FileService,
        },
    };

    #[derive(Debug)]
    struct WalkBackend {
        cancellation: CancellationToken,
        polled: Arc<Notify>,
    }

    #[async_trait]
    impl SessionBackend for WalkBackend {
        async fn resolve_path(&self, _path: &str) -> io::Result<String> {
            unreachable!()
        }

        async fn list_dir(&self, _path: &str) -> io::Result<EntryStream> {
            unreachable!()
        }

        async fn walk_dir(&self, _path: &str) -> io::Result<WalkStream> {
            let cancellation = self.cancellation.clone();
            let polled = self.polled.clone();
            Ok(stream::once(async move {
                polled.notify_one();
                cancellation.cancelled().await;
                Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
            })
            .boxed())
        }

        async fn read_file(&self, _path: &str) -> io::Result<ReadStream> {
            unreachable!()
        }

        async fn close(self: Box<Self>) -> io::Result<()> {
            self.cancellation.cancel();
            Ok(())
        }
    }

    fn session(backend: Option<Box<dyn SessionBackend>>) -> Arc<Session> {
        Arc::new(Session {
            id: "walk-test".into(),
            target: SessionTarget::Local,
            backend: tokio::sync::RwLock::new(backend),
            file_service: FileService::default(),
        })
    }

    #[tokio::test]
    async fn rejects_closed_sessions() {
        let error = session(None)
            .op_walk_dir("/fixture")
            .await
            .err()
            .expect("closed Session should reject walking");
        assert_eq!(error.kind(), io::ErrorKind::NotConnected);
    }

    #[tokio::test]
    async fn returns_owned_streams_without_retaining_the_backend_lock() {
        let cancellation = CancellationToken::new();
        let polled = Arc::new(Notify::new());
        let session = session(Some(Box::new(WalkBackend {
            cancellation,
            polled: polled.clone(),
        })));
        let mut walk = session.op_walk_dir("/fixture").await.unwrap();
        let poll = tokio::spawn(async move { walk.next().await.unwrap().unwrap_err() });
        polled.notified().await;

        tokio::time::timeout(Duration::from_millis(250), session.close())
            .await
            .expect("walk retained the backend read lock")
            .unwrap();
        assert_eq!(poll.await.unwrap().kind(), io::ErrorKind::Interrupted);
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
