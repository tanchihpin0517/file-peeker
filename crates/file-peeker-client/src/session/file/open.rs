use std::io;

use super::with_context;
use crate::session::{Session, session_closed};

impl Session {
    /// Opens a regular file from the selected host with the client operating
    /// system's default application.
    ///
    /// Remote files are completely downloaded into a client-local cache before
    /// the open request is sent.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the session is closed, the selected-host path
    /// cannot be resolved or read, a remote copy cannot be staged, or the
    /// operating system rejects the open request.
    pub async fn op_open_file(&self, path: &str) -> io::Result<()> {
        let (resolved_path, stream) = {
            let backend = self.backend.read().await;
            let backend = backend.as_ref().ok_or_else(session_closed)?;
            let resolved_path = backend.resolve_path(path).await.map_err(|error| {
                with_context(&error, format!("cannot resolve file path `{path}`"))
            })?;
            let stream = backend.read_file(&resolved_path).await.map_err(|error| {
                with_context(&error, format!("cannot start reading `{resolved_path}`"))
            })?;
            (resolved_path, stream)
        };

        self.file_service
            .open(&self.target, &self.id, &resolved_path, stream)
            .await
    }
}

#[cfg(test)]
impl Session {
    fn with_file_service_for_test(
        id: impl Into<String>,
        target: crate::session::SessionTarget,
        backend: Option<Box<dyn crate::session::backend::SessionBackend>>,
        cache_root: std::path::PathBuf,
        file_opener: std::sync::Arc<dyn super::opener::FileOpener>,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            id: id.into(),
            target,
            backend: tokio::sync::RwLock::new(backend),
            file_service: super::FileService::for_test(cache_root, file_opener),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{io, sync::Arc, time::Duration};

    use async_trait::async_trait;
    use futures::{StreamExt as _, stream};
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    use super::Session;
    use crate::{
        EntryStream, SessionTarget,
        session::{
            backend::{ReadStream, SessionBackend},
            file::opener::RecordingOpener,
        },
    };

    #[derive(Debug, Clone, Copy)]
    enum StartFailureBackend {
        Resolve,
        Read,
    }

    #[async_trait]
    impl SessionBackend for StartFailureBackend {
        async fn resolve_path(&self, _path: &str) -> io::Result<String> {
            match self {
                Self::Resolve => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "fixture resolve failed",
                )),
                Self::Read => Ok("/remote/unreadable.txt".into()),
            }
        }

        async fn list_dir(&self, _path: &str) -> io::Result<EntryStream> {
            unreachable!()
        }

        async fn walk_dir(&self, _path: &str) -> io::Result<crate::WalkStream> {
            unreachable!("walk is not used by this test")
        }

        async fn read_file(&self, _path: &str) -> io::Result<ReadStream> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture read start failed",
            ))
        }

        async fn close(self: Box<Self>) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn resolve_and_read_start_failures_do_not_reach_the_file_service() {
        for (backend, expected_kind) in [
            (StartFailureBackend::Resolve, io::ErrorKind::NotFound),
            (StartFailureBackend::Read, io::ErrorKind::PermissionDenied),
        ] {
            let cache = tempfile::tempdir().unwrap();
            let opener = Arc::new(RecordingOpener::default());
            let session = Session::with_file_service_for_test(
                "remote-start-error",
                SessionTarget::Remote {
                    destination: "fixture".into(),
                },
                Some(Box::new(backend)),
                cache.path().to_path_buf(),
                opener.clone(),
            );

            let error = session.op_open_file("unreadable.txt").await.unwrap_err();

            assert_eq!(error.kind(), expected_kind);
            assert!(opener.paths().is_empty());
        }
    }

    #[derive(Debug)]
    struct CancelBackend {
        cancellation: CancellationToken,
        polled: Arc<Notify>,
    }

    #[async_trait]
    impl SessionBackend for CancelBackend {
        async fn resolve_path(&self, _path: &str) -> io::Result<String> {
            Ok("/remote/pending.txt".into())
        }

        async fn list_dir(&self, _path: &str) -> io::Result<EntryStream> {
            unreachable!()
        }

        async fn walk_dir(&self, _path: &str) -> io::Result<crate::WalkStream> {
            unreachable!("walk is not used by this test")
        }

        async fn read_file(&self, _path: &str) -> io::Result<ReadStream> {
            let cancellation = self.cancellation.clone();
            let polled = self.polled.clone();
            Ok(stream::once(async move {
                polled.notify_one();
                cancellation.cancelled().await;
                Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
            })
            .boxed())
        }

        async fn close(self: Box<Self>) -> io::Result<()> {
            self.cancellation.cancel();
            Ok(())
        }
    }

    #[tokio::test]
    async fn close_is_not_blocked_while_a_remote_stream_is_pending() {
        let cache = tempfile::tempdir().unwrap();
        let polled = Arc::new(Notify::new());
        let opener = Arc::new(RecordingOpener::default());
        let session = Session::with_file_service_for_test(
            "remote-close",
            SessionTarget::Remote {
                destination: "fixture".into(),
            },
            Some(Box::new(CancelBackend {
                cancellation: CancellationToken::new(),
                polled: polled.clone(),
            })),
            cache.path().to_path_buf(),
            opener.clone(),
        );
        let operation = tokio::spawn({
            let session = session.clone();
            async move { session.op_open_file("pending.txt").await }
        });
        polled.notified().await;

        tokio::time::timeout(Duration::from_millis(250), session.close())
            .await
            .expect("close retained the backend read lock")
            .unwrap();
        let error = operation.await.unwrap().unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(opener.paths().is_empty());
    }

    #[tokio::test]
    async fn closed_sessions_do_not_reach_the_file_service() {
        let cache = tempfile::tempdir().unwrap();
        let opener = Arc::new(RecordingOpener::default());
        let session = Session::with_file_service_for_test(
            "closed",
            SessionTarget::Local,
            None,
            cache.path().to_path_buf(),
            opener.clone(),
        );

        let error = session.op_open_file("anything").await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotConnected);
        assert!(opener.paths().is_empty());
    }
}
