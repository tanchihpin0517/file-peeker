use std::io;

use super::{Session, backend::rpc};

impl Session {
    /// Resolves a path into an absolute lexical path on the selected host.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the session is closed, expansion fails, the
    /// working directory cannot be read, or a remote server returns an invalid
    /// response.
    pub(crate) async fn op_resolve_path(&self, path: &str) -> io::Result<String> {
        let connection = self.connection().await?;
        rpc::resolve_path(&connection, path).await
    }
}

#[cfg(test)]
mod tests {
    use super::Session;

    #[tokio::test]
    async fn op_resolve_path_rejects_a_closed_session() {
        let session = Session::closed_for_test("resolve-path-id");

        let error = session.op_resolve_path(".").await.unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(error.to_string(), "session is closed");
    }
}
