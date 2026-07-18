use std::sync::Arc;

use crate::{FilePeekerError, SessionConfig, session::Session};

#[derive(Debug, Default)]
pub(crate) struct Client;

impl Client {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self)
    }

    pub(crate) async fn connect(
        &self,
        config: SessionConfig,
    ) -> Result<Arc<Session>, FilePeekerError> {
        Session::start(config).await
    }
}

#[cfg(test)]
mod tests {
    use super::Client;
    use crate::{FilePeekerError, SessionConfig, SessionTarget};

    #[tokio::test]
    async fn connect_delegates_session_startup_errors() {
        let error = Client::new()
            .connect(SessionConfig {
                target: SessionTarget::Local {
                    server_executable_path: "/definitely/missing/file-peeker-server".into(),
                },
            })
            .await
            .expect_err("a missing server executable must fail");

        assert!(matches!(error, FilePeekerError::ServerStart { .. }));
    }
}
