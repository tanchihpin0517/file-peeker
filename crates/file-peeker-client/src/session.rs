use std::{io, sync::Arc};

use file_peeker_protocol::v1::{CurrentRootRequest, ListRequest};
use thiserror::Error;
use tokio::sync::RwLock;
use tonic::{Request, transport::Channel};

use crate::{
    connection::{Connection, ConnectionConfig},
    ops::{
        CurrentRootError, ListError, Listing, current_root,
        list::{self, ListStream},
    },
};

/// Configuration used to create a client session.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct SessionConfig {
    pub target: SessionTarget,
}

/// Server target retained by a session.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum SessionTarget {
    Local,
    Remote { destination: String },
}

/// Failure to start a server-backed session.
#[derive(Clone, Debug, Eq, Error, PartialEq, uniffi::Error)]
pub enum ConnectError {
    #[error("failed to start server: {message}")]
    ServerStart { message: String },
}

/// Failure to shut down a server-backed session.
#[derive(Clone, Debug, Eq, Error, PartialEq, uniffi::Error)]
pub enum CloseError {
    #[error("failed to shut down server: {message}")]
    ServerShutdown { message: String },
}

/// An independent File Peeker session.
#[derive(Debug, uniffi::Object)]
pub struct Session {
    id: String,
    target: SessionTarget,
    connection: RwLock<Option<Connection>>,
}

impl Session {
    pub(crate) async fn start(
        id: String,
        config: SessionConfig,
    ) -> Result<Arc<Self>, ConnectError> {
        let target = config.target.clone();
        let connection = Connection::from(match config.target {
            SessionTarget::Local => ConnectionConfig::Local {
                force_install: false,
            },
            SessionTarget::Remote { destination } => ConnectionConfig::Remote {
                destination,
                force_install: false,
            },
        })
        .await
        .map_err(|error| ConnectError::ServerStart {
            message: error.to_string(),
        })?;

        Ok(Arc::new(Self {
            id,
            target,
            connection: RwLock::new(Some(connection)),
        }))
    }

    /// Starts a native Rust stream of entry batches for one directory.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the session is closed, an operation connection
    /// cannot be opened, or the list request cannot be sent.
    pub async fn op_list(&self, path: &str) -> io::Result<ListStream> {
        let (channel, request) = self
            .grpc_request(ListRequest {
                path: path.to_owned(),
            })
            .await?;
        list::list(channel, request).await
    }

    /// Returns the server process's absolute working directory.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the session is closed, the operation
    /// connection fails, or the server returns an invalid response.
    pub async fn op_current_root(&self) -> io::Result<String> {
        let (channel, request) = self.grpc_request(CurrentRootRequest {}).await?;
        current_root::current_root(channel, request).await
    }

    /// Gracefully shuts down this session. Repeated calls succeed.
    ///
    /// # Errors
    ///
    /// Returns a shutdown error when the managed server does not exit cleanly.
    pub async fn close(&self) -> Result<(), CloseError> {
        let connection = self.connection.write().await.take();
        let result = match connection {
            Some(connection) => connection.close().await,
            None => return Ok(()),
        };
        result.map_err(|error| CloseError::ServerShutdown {
            message: error.to_string(),
        })
    }
}

impl Session {
    async fn grpc_request<T>(&self, message: T) -> io::Result<(Channel, Request<T>)> {
        let connection = self.connection.read().await;
        let connection = connection
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "session is closed"))?;
        Ok((connection.channel()?, connection.request(message)?))
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Session {
    /// Returns this session's immutable UUID.
    #[must_use]
    pub fn id(&self) -> String {
        self.id.clone()
    }

    /// Returns the immutable target associated with this session.
    #[must_use]
    pub fn target(&self) -> SessionTarget {
        self.target.clone()
    }

    /// Returns the server process's absolute working directory through `UniFFI`.
    ///
    /// # Errors
    ///
    /// Returns an operation error when the session is closed, the operation
    /// connection fails, or the server returns an invalid response.
    pub async fn op_current_root_uniffi(&self) -> Result<String, CurrentRootError> {
        self.op_current_root().await.map_err(CurrentRootError::from)
    }

    /// Starts a Swift-compatible listing adapter for one directory.
    ///
    /// # Errors
    ///
    /// Returns a listing error when the operation cannot be started.
    pub async fn op_list_uniffi(&self, path: String) -> Result<Arc<Listing>, ListError> {
        self.op_list(&path)
            .await
            .map(Listing::new)
            .map_err(ListError::from)
    }

    /// Gracefully shuts down this session through `UniFFI`. Repeated calls succeed.
    ///
    /// # Errors
    ///
    /// Returns a shutdown error when the managed server does not exit cleanly.
    pub async fn close_uniffi(&self) -> Result<(), CloseError> {
        self.close().await
    }
}

#[cfg(test)]
impl Session {
    pub(crate) fn without_connection(id: impl Into<String>, target: SessionTarget) -> Arc<Self> {
        Arc::new(Self {
            id: id.into(),
            target,
            connection: RwLock::new(None),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CurrentRootError, Session, SessionTarget};

    #[test]
    fn local_target_is_retained() {
        let target = SessionTarget::Local;
        let session = Session::without_connection("local-id", target.clone());

        assert_eq!(session.id(), "local-id");
        assert_eq!(session.target(), target);
    }

    #[test]
    fn remote_target_is_retained() {
        let target = SessionTarget::Remote {
            destination: "example.test".into(),
        };
        let session = Session::without_connection("remote-id", target.clone());

        assert_eq!(session.target(), target);
    }

    #[tokio::test]
    async fn close_is_idempotent() {
        let session = Session::without_connection("close-id", SessionTarget::Local);

        session.close().await.unwrap();
        session.close().await.unwrap();
    }

    #[tokio::test]
    async fn close_uniffi_is_idempotent() {
        let session = Session::without_connection("close-uniffi-id", SessionTarget::Local);

        session.close_uniffi().await.unwrap();
        session.close_uniffi().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_close_is_idempotent() {
        let session = Session::without_connection("concurrent-id", SessionTarget::Local);

        let (first, second) = tokio::join!(session.close(), session.close());

        first.unwrap();
        second.unwrap();
    }

    #[tokio::test]
    async fn op_list_rejects_a_closed_session() {
        let session = Session::without_connection("closed-list-id", SessionTarget::Local);
        session.close().await.unwrap();

        let error = session
            .op_list("/fixture")
            .await
            .err()
            .expect("closed session should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(error.to_string(), "session is closed");
    }

    #[tokio::test]
    async fn op_list_requires_an_open_connection() {
        let session = Session::without_connection("missing-list-id", SessionTarget::Local);

        let error = session
            .op_list("/fixture")
            .await
            .err()
            .expect("closed session should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(error.to_string(), "session is closed");
    }

    #[tokio::test]
    async fn op_current_root_rejects_a_closed_session() {
        let session = Session::without_connection("current-root-id", SessionTarget::Local);

        let error = session.op_current_root().await.unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(error.to_string(), "session is closed");
    }

    #[tokio::test]
    async fn op_current_root_uniffi_maps_native_errors() {
        let session = Session::without_connection("current-root-uniffi-id", SessionTarget::Local);

        let error = session.op_current_root_uniffi().await.unwrap_err();

        assert_eq!(
            error,
            CurrentRootError::Operation {
                message: "session is closed".into()
            }
        );
    }
}
