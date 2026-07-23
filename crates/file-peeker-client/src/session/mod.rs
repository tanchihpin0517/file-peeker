use std::{io, sync::Arc};

use file_peeker_core::FsService;
use thiserror::Error;
use tokio::sync::RwLock;

use self::{
    backend::{RemoteBackend, SessionBackend},
    file::FileService,
};

pub mod backend;
pub(crate) mod directory;
pub(crate) mod ffi;
mod file;
mod path;

/// Backend target retained by a session.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum SessionTarget {
    Local,
    Remote { destination: String },
}

/// Failure to start a session backend.
#[derive(Clone, Debug, Eq, Error, PartialEq, uniffi::Error)]
pub enum SessionStartError {
    #[error("failed to start session backend: {message}")]
    Backend { message: String },
}

/// Failure to shut down a session backend.
#[derive(Clone, Debug, Eq, Error, PartialEq, uniffi::Error)]
pub enum SessionShutdownError {
    #[error("failed to shut down session backend: {message}")]
    Backend { message: String },
}

/// An independent File Peeker session.
#[derive(Debug, uniffi::Object)]
pub struct Session {
    id: String,
    target: SessionTarget,
    backend: RwLock<Option<Box<dyn SessionBackend>>>,
    file_service: FileService,
}

impl Session {
    pub(crate) async fn start(
        id: String,
        target: SessionTarget,
    ) -> Result<Arc<Self>, SessionStartError> {
        let backend: Box<dyn SessionBackend> = match &target {
            SessionTarget::Local => Box::new(FsService::new()),
            SessionTarget::Remote { destination } => Box::new(
                RemoteBackend::connect(destination, false)
                    .await
                    .map_err(|error| SessionStartError::Backend {
                        message: error.to_string(),
                    })?,
            ),
        };

        Ok(Arc::new(Self {
            id,
            target,
            backend: RwLock::new(Some(backend)),
            file_service: FileService::default(),
        }))
    }

    /// Gracefully shuts down this session. Repeated calls succeed.
    ///
    /// # Errors
    ///
    /// Returns a shutdown error when the managed backend does not exit cleanly.
    pub async fn close(&self) -> Result<(), SessionShutdownError> {
        let mut backend = self.backend.write().await;
        let Some(backend) = backend.take() else {
            return Ok(());
        };
        backend
            .close()
            .await
            .map_err(|error| SessionShutdownError::Backend {
                message: error.to_string(),
            })
    }
}

fn session_closed() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "session is closed")
}

#[cfg(test)]
impl Session {
    pub(crate) fn closed_for_test(id: impl Into<String>, target: SessionTarget) -> Arc<Self> {
        Arc::new(Self {
            id: id.into(),
            target,
            backend: RwLock::new(None),
            file_service: FileService::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::{FileService, Session, SessionBackend, SessionTarget, backend::ReadStream};
    use crate::EntryStream;

    #[derive(Debug)]
    struct FailingCloseBackend;

    #[async_trait]
    impl SessionBackend for FailingCloseBackend {
        async fn resolve_path(&self, _path: &str) -> std::io::Result<String> {
            Ok("/fixture".into())
        }

        async fn list_dir(&self, _path: &str) -> std::io::Result<EntryStream> {
            unreachable!("listing is not used by this test")
        }

        async fn walk_dir(&self, _path: &str) -> std::io::Result<crate::WalkStream> {
            unreachable!("walk is not used by this test")
        }

        async fn read_file(&self, _path: &str) -> std::io::Result<ReadStream> {
            unreachable!("reading is not used by this test")
        }

        async fn close(self: Box<Self>) -> std::io::Result<()> {
            Err(std::io::Error::other("fixture shutdown failure"))
        }
    }

    async fn open_local_session(id: &str) -> std::sync::Arc<Session> {
        Session::start(id.into(), SessionTarget::Local)
            .await
            .unwrap()
    }

    #[test]
    fn local_target_is_retained() {
        let target = SessionTarget::Local;
        let session = Session::closed_for_test("local-id", target.clone());

        assert_eq!(session.id(), "local-id");
        assert_eq!(session.target(), target);
    }

    #[test]
    fn remote_target_is_retained() {
        let target = SessionTarget::Remote {
            destination: "example.test".into(),
        };
        let session = Session::closed_for_test("remote-id", target.clone());

        assert_eq!(session.target(), target);
    }

    #[tokio::test]
    async fn close_is_idempotent() {
        let session = open_local_session("close-id").await;

        session.close().await.unwrap();
        session.close().await.unwrap();
    }

    #[tokio::test]
    async fn close_uniffi_is_idempotent() {
        let session = open_local_session("close-uniffi-id").await;

        session.close_uniffi().await.unwrap();
        session.close_uniffi().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_close_is_idempotent() {
        let session = open_local_session("concurrent-id").await;

        let (first, second) = tokio::join!(session.close(), session.close());

        first.unwrap();
        second.unwrap();
    }

    #[tokio::test]
    async fn failed_close_is_terminal() {
        let session = std::sync::Arc::new(Session {
            id: "failed-close-id".into(),
            target: SessionTarget::Local,
            backend: tokio::sync::RwLock::new(Some(Box::new(FailingCloseBackend))),
            file_service: FileService::default(),
        });

        assert!(session.close().await.is_err());
        assert_eq!(
            session.op_resolve_path(".").await.unwrap_err().to_string(),
            "session is closed"
        );
        session.close().await.unwrap();
    }
}
