use std::sync::Arc;

use crate::{ConnectError, Session, SessionConfig};

/// Entry point for creating independent File Peeker sessions.
#[derive(Debug, Default, uniffi::Object)]
pub struct Client;

#[uniffi::export(async_runtime = "tokio")]
impl Client {
    #[uniffi::constructor]
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }

    /// Starts a server-backed session for the configured target.
    ///
    /// # Errors
    ///
    /// Returns a server-start error when the target server cannot be started
    /// and authenticated.
    pub async fn start_session(&self, config: SessionConfig) -> Result<Arc<Session>, ConnectError> {
        Session::start(config).await
    }
}

#[cfg(test)]
mod tests {
    use crate::{Client, ConnectError, SessionConfig, SessionTarget};

    #[tokio::test]
    #[ignore = "starts a managed local server"]
    async fn starts_local_session() {
        let session = Client::new()
            .start_session(SessionConfig {
                target: SessionTarget::Local,
            })
            .await
            .expect("local startup should succeed");

        assert_eq!(session.target(), SessionTarget::Local);
        session
            .close()
            .await
            .expect("local shutdown should succeed");
    }

    #[tokio::test]
    async fn empty_remote_destination_is_rejected() {
        let error = Client::new()
            .start_session(SessionConfig {
                target: SessionTarget::Remote {
                    destination: String::new(),
                },
            })
            .await
            .expect_err("an empty remote destination should fail");

        assert!(matches!(error, ConnectError::ServerStart { .. }));
        assert!(error.to_string().contains("remote server is required"));
    }
}
