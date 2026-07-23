use std::io;

use futures::stream::BoxStream;

use super::DirectoryEntry;
use crate::session::{Session, session_closed};

pub type EntryStream = BoxStream<'static, io::Result<DirectoryEntry>>;

impl Session {
    /// Starts a native Rust stream of entries for one directory.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the session is closed, the local operation
    /// cannot be started, or the remote list request cannot be sent.
    pub async fn op_list_dir(&self, path: &str) -> io::Result<EntryStream> {
        let backend = self.backend.read().await;
        backend
            .as_ref()
            .ok_or_else(session_closed)?
            .list_dir(path)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::Session;
    use crate::SessionTarget;

    #[tokio::test]
    async fn op_list_rejects_a_closed_session() {
        let session = Session::closed_for_test("closed-list-id", SessionTarget::Local);
        session.close().await.unwrap();

        let error = session
            .op_list_dir("/fixture")
            .await
            .err()
            .expect("closed session should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(error.to_string(), "session is closed");
    }

    #[tokio::test]
    async fn op_list_requires_an_open_connection() {
        let session = Session::closed_for_test("missing-list-id", SessionTarget::Local);

        let error = session
            .op_list_dir("/fixture")
            .await
            .err()
            .expect("closed session should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(error.to_string(), "session is closed");
    }
}
