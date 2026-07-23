use std::{
    collections::{HashMap, hash_map::Entry},
    sync::Arc,
};

use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{Session, SessionShutdownError, SessionStartError, SessionTarget};

/// Entry point and owner for independent File Peeker sessions.
#[derive(Debug, Default, uniffi::Object)]
pub struct Client {
    sessions: RwLock<HashMap<String, Arc<Session>>>,
}

/// Failure to close a Client-owned Session.
#[derive(Clone, Debug, Eq, Error, PartialEq, uniffi::Error)]
pub enum ClientCloseSessionError {
    #[error("session not found: {id}")]
    NotFound { id: String },
    #[error("failed to shut down session backend: {message}")]
    Backend { message: String },
}

impl From<SessionShutdownError> for ClientCloseSessionError {
    fn from(error: SessionShutdownError) -> Self {
        match error {
            SessionShutdownError::Backend { message } => Self::Backend { message },
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

    /// Starts and retains a backend-backed Session, returning its UUID.
    ///
    /// # Errors
    ///
    /// Returns a backend-start error when the selected target cannot be started.
    pub async fn start_session(&self, target: SessionTarget) -> Result<String, SessionStartError> {
        let id = new_session_id();
        let session = Session::start(id.clone(), target).await?;
        let mut sessions = self.sessions.write().await;
        match sessions.entry(id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(session);
                Ok(id)
            }
            Entry::Occupied(_) => {
                drop(sessions);
                let _ = session.close().await;
                Err(SessionStartError::Backend {
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
    /// Returns `ClientCloseSessionError::NotFound` for an unknown UUID or a shutdown
    /// error when the selected backend does not shut down cleanly.
    pub async fn close_session(&self, id: String) -> Result<(), ClientCloseSessionError> {
        let session = self
            .sessions
            .write()
            .await
            .remove(&id)
            .ok_or(ClientCloseSessionError::NotFound { id })?;
        session.close().await.map_err(ClientCloseSessionError::from)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::TryStreamExt as _;
    use uuid::Uuid;

    use super::{Client, ClientCloseSessionError, new_session_id};
    use crate::{Session, SessionStartError, SessionTarget};

    #[tokio::test]
    async fn starts_and_retains_local_session() {
        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::write(fixture.path().join("entry.txt"), b"")
            .await
            .unwrap();
        let client = Client::new();
        let id = client
            .start_session(SessionTarget::Local)
            .await
            .expect("local startup should succeed");
        let session = client
            .get_session(id.clone())
            .await
            .expect("started Session should be retained");

        assert_eq!(session.id(), id);
        assert_eq!(session.target(), SessionTarget::Local);
        assert!(std::path::Path::new(&session.op_resolve_path("~").await.unwrap()).is_absolute());
        let entries = session
            .op_list_dir(fixture.path().to_str().unwrap())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "entry.txt");
        client
            .close_session(id)
            .await
            .expect("local shutdown should succeed");
    }

    #[tokio::test]
    async fn empty_remote_destination_is_rejected_without_retention() {
        let client = Client::new();
        let error = client
            .start_session(SessionTarget::Remote {
                destination: String::new(),
            })
            .await
            .expect_err("an empty remote destination should fail");

        assert!(matches!(error, SessionStartError::Backend { .. }));
        assert!(error.to_string().contains("remote server is required"));
        assert!(client.sessions.read().await.is_empty());
    }

    #[tokio::test]
    async fn retains_retrieves_and_removes_sessions_by_id() {
        let client = Client::new();
        let id = Uuid::new_v4().to_string();
        let session = Session::closed_for_test(id.clone(), SessionTarget::Local);
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
            ClientCloseSessionError::NotFound { id }
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
