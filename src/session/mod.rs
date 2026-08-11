use std::{io, sync::Arc};

use thiserror::Error;
use tokio::sync::{RwLock, RwLockReadGuard};

use self::connection::{ServerConnection, ServerLocation};

pub(crate) mod connection;
pub(crate) mod directory;
pub(crate) mod file;
mod path;
pub(crate) mod rpc;

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
    /// `None` once the session is closed.
    connection: RwLock<Option<ServerConnection>>,
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
        let connection = ServerConnection::open(target.location())
            .await
            .map_err(|error| SessionStartError::Backend {
                message: error.to_string(),
            })?;

        Ok(Arc::new(Self {
            id,
            connection: RwLock::new(Some(connection)),
        }))
    }

    /// Borrows the open connection, or fails when this session is closed.
    ///
    /// The returned guard holds the connection read lock, so callers must drop
    /// it before an owned stream is handed back to a caller. Session shutdown
    /// then cancels in-flight work instead of waiting on it.
    async fn connection(&self) -> io::Result<RwLockReadGuard<'_, ServerConnection>> {
        RwLockReadGuard::try_map(self.connection.read().await, Option::as_ref)
            .map_err(|_| session_closed())
    }

    /// Gracefully shuts down this session. Repeated calls succeed.
    ///
    /// # Errors
    ///
    /// Returns a shutdown error when the managed backend does not exit cleanly.
    pub(crate) async fn close(&self) -> Result<(), SessionShutdownError> {
        let Some(connection) = self.connection.write().await.take() else {
            return Ok(());
        };
        connection
            .shutdown()
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
    /// Builds a Session whose backend slot is already empty, as it is after a
    /// completed close.
    #[must_use]
    pub(crate) fn closed_for_test(id: impl Into<String>) -> Arc<Self> {
        Self::with_connection_for_test(id, None)
    }

    pub(crate) fn with_connection_for_test(
        id: impl Into<String>,
        connection: Option<ServerConnection>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id: id.into(),
            connection: RwLock::new(connection),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Session, SessionTarget};

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

    /// Close empties the connection slot before shutting the server down, so a
    /// closed Session rejects later operations rather than reaching a server
    /// that is going away.
    #[tokio::test]
    async fn close_is_terminal() {
        let session = open_local_session("terminal-id").await;

        session.close().await.unwrap();

        assert_eq!(
            session.op_resolve_path(".").await.unwrap_err().to_string(),
            "session is closed"
        );
        session.close().await.unwrap();
    }
}
