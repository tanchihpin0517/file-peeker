use std::{
    collections::{HashMap, hash_map::Entry},
    sync::Arc,
};

use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{CloseError, ConnectError, Session, SessionConfig};

/// Entry point and owner for independent File Peeker sessions.
#[derive(Debug, Default, uniffi::Object)]
pub struct Client {
    sessions: RwLock<HashMap<String, Arc<Session>>>,
}

/// Failure to close a Client-owned Session.
#[derive(Clone, Debug, Eq, Error, PartialEq, uniffi::Error)]
pub enum CloseSessionError {
    #[error("session not found: {id}")]
    NotFound { id: String },
    #[error("failed to shut down server: {message}")]
    ServerShutdown { message: String },
}

impl From<CloseError> for CloseSessionError {
    fn from(error: CloseError) -> Self {
        match error {
            CloseError::ServerShutdown { message } => Self::ServerShutdown { message },
        }
    }
}

fn new_session_id() -> String {
    Uuid::new_v4().to_string()
}

#[uniffi::export(async_runtime = "tokio")]
impl Client {
    #[uniffi::constructor]
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Starts and retains a server-backed Session, returning its UUID.
    ///
    /// # Errors
    ///
    /// Returns a server-start error when the target server cannot be started
    /// and authenticated.
    pub async fn start_session(&self, config: SessionConfig) -> Result<String, ConnectError> {
        let id = new_session_id();
        let session = Session::start(id.clone(), config).await?;
        let mut sessions = self.sessions.write().await;
        match sessions.entry(id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(session);
                Ok(id)
            }
            Entry::Occupied(_) => {
                drop(sessions);
                let _ = session.close().await;
                Err(ConnectError::ServerStart {
                    message: "generated duplicate Session UUID".into(),
                })
            }
        }
    }

    /// Returns the retained Session with the requested UUID.
    pub async fn get_session(&self, id: String) -> Option<Arc<Session>> {
        self.sessions.read().await.get(&id).cloned()
    }

    /// Removes and gracefully closes the retained Session with the requested UUID.
    ///
    /// # Errors
    ///
    /// Returns `CloseSessionError::NotFound` for an unknown UUID or a shutdown
    /// error when the server does not acknowledge shutdown.
    pub async fn close_session(&self, id: String) -> Result<(), CloseSessionError> {
        let session = self
            .sessions
            .write()
            .await
            .remove(&id)
            .ok_or(CloseSessionError::NotFound { id })?;
        session.close().await.map_err(CloseSessionError::from)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::{Client, CloseSessionError, new_session_id};
    use crate::{ConnectError, Session, SessionConfig, SessionTarget};

    #[tokio::test]
    #[ignore = "starts a managed local server"]
    async fn starts_and_retains_local_session() {
        let client = Client::new();
        let id = client
            .start_session(SessionConfig {
                target: SessionTarget::Local,
            })
            .await
            .expect("local startup should succeed");
        let session = client
            .get_session(id.clone())
            .await
            .expect("started Session should be retained");

        assert_eq!(session.id(), id);
        assert_eq!(session.target(), SessionTarget::Local);
        client
            .close_session(id)
            .await
            .expect("local shutdown should succeed");
    }

    #[tokio::test]
    async fn empty_remote_destination_is_rejected_without_retention() {
        let client = Client::new();
        let error = client
            .start_session(SessionConfig {
                target: SessionTarget::Remote {
                    destination: String::new(),
                },
            })
            .await
            .expect_err("an empty remote destination should fail");

        assert!(matches!(error, ConnectError::ServerStart { .. }));
        assert!(error.to_string().contains("remote server is required"));
        assert!(client.sessions.read().await.is_empty());
    }

    #[tokio::test]
    async fn retains_retrieves_and_removes_sessions_by_id() {
        let client = Client::new();
        let id = Uuid::new_v4().to_string();
        let session = Session::without_connection(id.clone(), SessionTarget::Local);
        client
            .sessions
            .write()
            .await
            .insert(id.clone(), Arc::clone(&session));

        let retained = client.get_session(id.clone()).await.unwrap();
        assert!(Arc::ptr_eq(&retained, &session));
        session.close().await.unwrap();
        assert!(client.get_session(id.clone()).await.is_some());
        client.close_session(id.clone()).await.unwrap();
        assert!(client.get_session(id).await.is_none());
    }

    #[tokio::test]
    async fn unknown_session_ids_are_reported() {
        let client = Client::new();
        let id = Uuid::new_v4().to_string();

        assert!(client.get_session(id.clone()).await.is_none());
        assert_eq!(
            client.close_session(id.clone()).await.unwrap_err(),
            CloseSessionError::NotFound { id }
        );
    }

    #[test]
    fn generated_session_ids_are_unique_uuids() {
        let first = new_session_id();
        let second = new_session_id();

        Uuid::parse_str(&first).expect("first ID should be a UUID");
        Uuid::parse_str(&second).expect("second ID should be a UUID");
        assert_ne!(first, second);
    }
}
