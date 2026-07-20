use std::{io, sync::Arc};

use thiserror::Error;
use tokio::sync::RwLock;

use crate::{
    ops::{
        ListError, Listing,
        list::{self, ListStream},
    },
    server::{LocalServer, LocalServerConfig, RemoteServer, Server},
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
    target: SessionTarget,
    server: RwLock<Option<SessionServer>>,
}

#[derive(Debug)]
enum SessionServer {
    Local(LocalServer),
    Remote(RemoteServer),
}

impl Session {
    pub(crate) async fn start(config: SessionConfig) -> Result<Arc<Self>, ConnectError> {
        let target = config.target.clone();
        let server = match config.target {
            SessionTarget::Local => {
                let mut server = LocalServer::default();
                server
                    .connect(LocalServerConfig::default())
                    .await
                    .map_err(|error| ConnectError::ServerStart {
                        message: error.to_string(),
                    })?;
                SessionServer::Local(server)
            }
            SessionTarget::Remote { destination } => {
                let mut server = RemoteServer::default();
                server
                    .connect(destination)
                    .await
                    .map_err(|error| ConnectError::ServerStart {
                        message: error.to_string(),
                    })?;
                SessionServer::Remote(server)
            }
        };

        Ok(Arc::new(Self {
            target,
            server: RwLock::new(Some(server)),
        }))
    }

    #[cfg(test)]
    fn from_server(target: SessionTarget, server: SessionServer) -> Arc<Self> {
        Arc::new(Self {
            target,
            server: RwLock::new(Some(server)),
        })
    }

    /// Starts a native Rust stream of entries for one directory.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the session is closed, an operation connection
    /// cannot be opened, or the list request cannot be sent.
    pub async fn op_list(&self, path: &str) -> io::Result<ListStream> {
        let stream = {
            let server = self.server.read().await;
            let server = server
                .as_ref()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "session is closed"))?;
            match server {
                SessionServer::Local(server) => server.operate().await?,
                SessionServer::Remote(server) => server.operate().await?,
            }
        };
        list::list(stream, path).await
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Session {
    /// Returns the immutable target associated with this session.
    #[must_use]
    pub fn target(&self) -> SessionTarget {
        self.target.clone()
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

    /// Gracefully shuts down this session. Repeated calls succeed.
    ///
    /// # Errors
    ///
    /// Returns a shutdown error when the server does not acknowledge shutdown.
    pub async fn close(&self) -> Result<(), CloseError> {
        let server = self.server.write().await.take();
        let result = match server {
            Some(SessionServer::Local(mut server)) => server.shutdown().await,
            Some(SessionServer::Remote(mut server)) => server.shutdown().await,
            None => return Ok(()),
        };
        result.map_err(|error| CloseError::ServerShutdown {
            message: error.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Session, SessionServer, SessionTarget};
    use crate::server::{LocalServer, RemoteServer};

    #[test]
    fn local_target_is_retained() {
        let target = SessionTarget::Local;
        let session =
            Session::from_server(target.clone(), SessionServer::Local(LocalServer::default()));

        assert_eq!(session.target(), target);
    }

    #[test]
    fn remote_target_is_retained() {
        let target = SessionTarget::Remote {
            destination: "example.test".into(),
        };
        let session = Session::from_server(
            target.clone(),
            SessionServer::Remote(RemoteServer::default()),
        );

        assert_eq!(session.target(), target);
    }

    #[tokio::test]
    async fn close_is_idempotent() {
        let session = Session::from_server(
            SessionTarget::Local,
            SessionServer::Local(LocalServer::default()),
        );

        session.close().await.unwrap();
        session.close().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_close_is_idempotent() {
        let session = Session::from_server(
            SessionTarget::Local,
            SessionServer::Local(LocalServer::default()),
        );

        let (first, second) = tokio::join!(session.close(), session.close());

        first.unwrap();
        second.unwrap();
    }

    #[tokio::test]
    async fn op_list_rejects_a_closed_session() {
        let session = Session::from_server(
            SessionTarget::Local,
            SessionServer::Local(LocalServer::default()),
        );
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
    async fn op_list_requires_a_connected_local_server() {
        let session = Session::from_server(
            SessionTarget::Local,
            SessionServer::Local(LocalServer::default()),
        );

        let error = session
            .op_list("/fixture")
            .await
            .err()
            .expect("disconnected local server should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
    }

    #[tokio::test]
    async fn op_list_requires_a_connected_remote_server() {
        let session = Session::from_server(
            SessionTarget::Remote {
                destination: "example.test".into(),
            },
            SessionServer::Remote(RemoteServer::default()),
        );

        let error = session
            .op_list("/fixture")
            .await
            .err()
            .expect("disconnected remote server should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
    }
}
