use std::io;

use futures::stream::BoxStream;

use super::DirectoryEntry;
use crate::client::{Client, ConnectionId, rpc};
use crate::host_path::HostPath;

pub(crate) type EntryStream = BoxStream<'static, io::Result<DirectoryEntry>>;

impl Client {
    /// Starts a native Rust stream of entries for one directory.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the connection is closed, the local operation
    /// cannot be started, or the remote list request cannot be sent.
    pub(crate) async fn op_list_dir(
        &self,
        id: &ConnectionId,
        path: &HostPath,
    ) -> io::Result<EntryStream> {
        let connection = self.connection(id).await?;
        rpc::list_dir(&connection, path).await
    }
}

#[cfg(test)]
mod tests {
    use crate::client::{Client, ConnectionId};

    #[tokio::test]
    async fn op_list_rejects_a_closed_connection() {
        let (client, id) = Client::closed_for_test();

        let error = client
            .op_list_dir(&id, &"/fixture".into())
            .await
            .err()
            .expect("a closed connection should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(error.to_string(), "connection is closed");
    }

    /// An unknown ID is the registry's half of the same rejection: a caller that
    /// kept an ID past `close` must not reach another connection.
    #[tokio::test]
    async fn op_list_rejects_an_unknown_connection() {
        let client = Client::new();

        let error = client
            .op_list_dir(&ConnectionId::new(), &"/fixture".into())
            .await
            .err()
            .expect("an unknown connection should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(error.to_string(), "connection is closed");
    }
}
