use std::{io, sync::Arc};

use thiserror::Error;
use tokio::sync::{RwLock, RwLockReadGuard};

use self::backend::{RpcBackend, SessionBackend, connection::ServerLocation};

pub(crate) mod backend;
pub(crate) mod directory;
pub(crate) mod file;
mod path;

pub(crate) use file::copy::{CopyConflict, CopyResult};

/// The host a session browses.
///
/// Both variants run a File Peeker server and are reached the same way; the
/// target only decides where that server runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionTarget {
    Local,
    Remote { destination: String },
}

impl SessionTarget {
    fn location(&self) -> ServerLocation {
        match self {
            Self::Local => ServerLocation::Embedded,
            Self::Remote { destination } => ServerLocation::Ssh {
                destination: destination.clone(),
                force_install: false,
            },
        }
    }
}

/// Failure to start a session backend.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum SessionStartError {
    #[error("failed to start session backend: {message}")]
    Backend { message: String },
}

/// Failure to shut down a session backend.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum SessionShutdownError {
    #[error("failed to shut down session backend: {message}")]
    Backend { message: String },
}

/// An independent File Peeker session.
#[derive(Debug)]
pub(crate) struct Session {
    id: String,
    backend: RwLock<Option<Box<dyn SessionBackend>>>,
}

impl Session {
    /// Returns this session's immutable UUID.
    #[must_use]
    pub(crate) fn id(&self) -> String {
        self.id.clone()
    }

    /// Starts a Session against `target`.
    ///
    /// The target only selects where the server runs and is not retained: the
    /// TUI records its own copy before startup, which is the only place it is
    /// displayed.
    ///
    /// # Errors
    ///
    /// Returns a backend-start error when the server cannot be started,
    /// reached, or authenticated.
    pub(crate) async fn start(
        id: String,
        target: SessionTarget,
    ) -> Result<Arc<Self>, SessionStartError> {
        let backend = RpcBackend::open(target.location()).await.map_err(|error| {
            SessionStartError::Backend {
                message: error.to_string(),
            }
        })?;

        Ok(Arc::new(Self {
            id,
            backend: RwLock::new(Some(Box::new(backend))),
        }))
    }

    /// Borrows the open backend, or fails when this session is closed.
    ///
    /// The returned guard holds the backend read lock, so callers must drop it
    /// before an owned stream is handed back to a caller. Session shutdown then
    /// cancels in-flight work instead of waiting on it.
    async fn backend(&self) -> io::Result<RwLockReadGuard<'_, dyn SessionBackend>> {
        RwLockReadGuard::try_map(self.backend.read().await, Option::as_deref)
            .map_err(|_| session_closed())
    }

    /// Gracefully shuts down this session. Repeated calls succeed.
    ///
    /// # Errors
    ///
    /// Returns a shutdown error when the managed backend does not exit cleanly.
    pub(crate) async fn close(&self) -> Result<(), SessionShutdownError> {
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
    /// Builds a Session over a scripted backend instead of a real filesystem or
    /// remote host.
    #[must_use]
    pub(crate) fn for_test(
        id: impl Into<String>,
        backend: crate::test_support::FakeBackend,
    ) -> Arc<Self> {
        Self::with_backend_for_test(id, Some(Box::new(backend)))
    }

    /// Builds a Session whose backend slot is already empty, as it is after a
    /// completed close.
    #[must_use]
    pub(crate) fn closed_for_test(id: impl Into<String>) -> Arc<Self> {
        Self::with_backend_for_test(id, None)
    }

    pub(crate) fn with_backend_for_test(
        id: impl Into<String>,
        backend: Option<Box<dyn SessionBackend>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id: id.into(),
            backend: RwLock::new(backend),
        })
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::{Session, SessionBackend, SessionTarget, backend::ReadStream};
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

        fn write_file(
            &self,
            _directory: String,
            _name: String,
            _conflict: crate::session::backend::BackendWriteConflict,
            _source: ReadStream,
        ) -> crate::session::backend::WriteFuture {
            unreachable!("writing is not used by this test")
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
    fn id_is_retained() {
        assert_eq!(Session::closed_for_test("local-id").id(), "local-id");
    }

    #[tokio::test]
    async fn close_is_idempotent() {
        let session = open_local_session("close-id").await;

        session.close().await.unwrap();
        session.close().await.unwrap();
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
        let session =
            Session::with_backend_for_test("failed-close-id", Some(Box::new(FailingCloseBackend)));

        assert!(session.close().await.is_err());
        assert_eq!(
            session.op_resolve_path(".").await.unwrap_err().to_string(),
            "session is closed"
        );
        session.close().await.unwrap();
    }
}
