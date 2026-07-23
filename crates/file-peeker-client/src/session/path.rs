use std::io;

use super::{Session, session_closed};

impl Session {
    /// Resolves a path into an absolute lexical path on the selected host.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the session is closed, expansion fails, the
    /// working directory cannot be read, or a remote server returns an invalid
    /// response.
    pub async fn op_resolve_path(&self, path: &str) -> io::Result<String> {
        let backend = self.backend.read().await;
        backend
            .as_ref()
            .ok_or_else(session_closed)?
            .resolve_path(path)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::Session;
    use crate::SessionTarget;

    #[tokio::test]
    async fn op_resolve_path_rejects_a_closed_session() {
        let session = Session::closed_for_test("resolve-path-id", SessionTarget::Local);

        let error = session.op_resolve_path(".").await.unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(error.to_string(), "session is closed");
    }
}
