use std::io;

use super::{Client, ConnectionId, rpc};

impl Client {
    /// Resolves a path into an absolute lexical path on the selected host.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the connection is closed, expansion fails, the
    /// working directory cannot be read, or a remote server returns an invalid
    /// response.
    pub(crate) async fn op_resolve_path(
        &self,
        id: &ConnectionId,
        path: &str,
    ) -> io::Result<String> {
        let connection = self.connection(id).await?;
        rpc::resolve_path(&connection, path).await
    }
}

#[cfg(test)]
mod tests {
    use super::Client;

    #[tokio::test]
    async fn op_resolve_path_rejects_a_closed_connection() {
        let (client, id) = Client::closed_for_test();

        let error = client.op_resolve_path(&id, ".").await.unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(error.to_string(), "connection is closed");
    }
}
