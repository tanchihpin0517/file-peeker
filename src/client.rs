use std::{
    collections::{HashMap, hash_map::Entry},
    sync::Arc,
};

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{Session, SessionShutdownError, SessionStartError, SessionTarget};

/// Entry point and owner for independent File Peeker sessions.
#[derive(Debug, Default)]
pub(crate) struct Client {
    sessions: RwLock<HashMap<String, Arc<Session>>>,
}

fn new_session_id() -> String {
    Uuid::new_v4().to_string()
}

impl Client {
    #[must_use]
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Starts and retains a backend-backed Session.
    ///
    /// The Client keeps a strong reference, so a caller that drops the returned
    /// Session without closing it still leaves shutdown to `close_session` or
    /// `close_all`.
    ///
    /// # Errors
    ///
    /// Returns a backend-start error when the selected target cannot be started.
    pub(crate) async fn start_session(
        &self,
        target: SessionTarget,
    ) -> Result<Arc<Session>, SessionStartError> {
        let id = new_session_id();
        let session = Session::start(id.clone(), target).await?;
        let mut sessions = self.sessions.write().await;
        match sessions.entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(Arc::clone(&session));
                Ok(session)
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

    /// Removes and gracefully closes the retained Session with the requested
    /// UUID. Unknown and repeated IDs succeed.
    ///
    /// # Errors
    ///
    /// Returns a shutdown error when the selected backend does not exit cleanly.
    pub(crate) async fn close_session(&self, id: &str) -> Result<(), SessionShutdownError> {
        let Some(session) = self.sessions.write().await.remove(id) else {
            return Ok(());
        };
        session.close().await
    }

    /// Removes and gracefully closes every retained Session. Repeated calls
    /// succeed.
    ///
    /// # Errors
    ///
    /// Returns the first shutdown error after attempting every Session.
    pub(crate) async fn close_all(&self) -> Result<(), SessionShutdownError> {
        let sessions: Vec<_> = self
            .sessions
            .write()
            .await
            .drain()
            .map(|(_, s)| s)
            .collect();
        let mut first_error = None;
        for session in sessions {
            if let Err(error) = session.close().await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::TryStreamExt as _;
    use uuid::Uuid;

    use super::{Client, new_session_id};
    use crate::{SessionStartError, SessionTarget};

    #[tokio::test]
    async fn starts_and_retains_local_session() {
        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::write(fixture.path().join("entry.txt"), b"")
            .await
            .unwrap();
        let client = Client::new();
        let session = client
            .start_session(SessionTarget::Local)
            .await
            .expect("local startup should succeed");

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
            .close_session(&session.id())
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
    async fn retains_and_removes_sessions_by_id() {
        let client = Client::new();
        let session = client.start_session(SessionTarget::Local).await.unwrap();
        let id = session.id();

        assert!(client.sessions.read().await.contains_key(&id));
        session.close().await.unwrap();
        assert!(client.sessions.read().await.contains_key(&id));
        client.close_session(&id).await.unwrap();
        assert!(client.sessions.read().await.is_empty());
    }

    #[tokio::test]
    async fn closing_unknown_or_repeated_ids_succeeds() {
        let client = Client::new();
        let id = Uuid::new_v4().to_string();

        client.close_session(&id).await.unwrap();
        let session = client.start_session(SessionTarget::Local).await.unwrap();
        client.close_session(&session.id()).await.unwrap();
        client.close_session(&session.id()).await.unwrap();
    }

    #[tokio::test]
    async fn close_all_drains_every_retained_session() {
        let client = Client::new();
        let first = client.start_session(SessionTarget::Local).await.unwrap();
        let second = client.start_session(SessionTarget::Local).await.unwrap();
        assert_ne!(first.id(), second.id());

        client.close_all().await.unwrap();

        assert!(client.sessions.read().await.is_empty());
        client.close_all().await.unwrap();
        assert_eq!(Arc::strong_count(&first), 1);
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
