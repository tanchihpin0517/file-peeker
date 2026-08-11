//! The calling side of the File Peeker protocol.
//!
//! Every module here turns one `SessionBackend` capability into a request,
//! sends it, and validates the response. The handling side lives in
//! [`crate::server`]; nothing in this module performs filesystem work.

mod entry;
mod error;
mod list_dir;
mod read_file;
mod resolve_path;
mod walk_dir;
mod write_file;

use std::io;

use async_trait::async_trait;

use super::{
    BackendWriteConflict, ReadStream, SessionBackend, WriteFuture,
    connection::{ServerConnection, ServerLocation},
};
use crate::{EntryStream, WalkStream};

/// Issues every Session primitive as an authenticated RPC.
///
/// This is the only `SessionBackend`. A local Session gets a server embedded in
/// this process and a remote Session gets one over SSH, but both arrive here.
#[derive(Debug)]
pub(crate) struct RpcBackend {
    connection: ServerConnection,
}

impl RpcBackend {
    /// Starts a server at `location` and connects to it.
    ///
    /// # Errors
    ///
    /// Returns an error when the server cannot be started, reached, or
    /// authenticated.
    pub(crate) async fn open(location: ServerLocation) -> io::Result<Self> {
        Ok(Self {
            connection: ServerConnection::open(location).await?,
        })
    }
}

#[async_trait]
impl SessionBackend for RpcBackend {
    async fn resolve_path(&self, path: &str) -> io::Result<String> {
        resolve_path::resolve_path(&self.connection, path).await
    }

    async fn list_dir(&self, path: &str) -> io::Result<EntryStream> {
        list_dir::list_dir(&self.connection, path).await
    }

    async fn walk_dir(&self, path: &str) -> io::Result<WalkStream> {
        walk_dir::walk_dir(&self.connection, path).await
    }

    async fn read_file(&self, path: &str) -> io::Result<ReadStream> {
        read_file::read_file(&self.connection, path).await
    }

    fn write_file(
        &self,
        directory: String,
        name: String,
        conflict: BackendWriteConflict,
        source: ReadStream,
    ) -> WriteFuture {
        write_file::write_file(&self.connection, directory, name, conflict, source)
    }

    async fn close(self: Box<Self>) -> io::Result<()> {
        let Self { connection } = *self;
        connection.close().await
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use bytes::Bytes;
    use futures::{StreamExt as _, TryStreamExt as _, stream};

    use super::{BackendWriteConflict, RpcBackend, ServerLocation, SessionBackend};

    async fn embedded() -> Box<RpcBackend> {
        Box::new(
            RpcBackend::open(ServerLocation::Embedded)
                .await
                .expect("an embedded server should start"),
        )
    }

    #[tokio::test]
    async fn rejects_empty_destination_on_every_attempt() {
        for _ in 0..2 {
            let error = RpcBackend::open(ServerLocation::Ssh {
                destination: String::new(),
                force_install: false,
            })
            .await
            .expect_err("an empty destination should fail");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[tokio::test]
    async fn serves_every_operation_against_an_embedded_server() {
        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::write(fixture.path().join("entry.txt"), b"payload")
            .await
            .unwrap();
        tokio::fs::create_dir(fixture.path().join("nested"))
            .await
            .unwrap();
        tokio::fs::write(fixture.path().join("nested/child.txt"), b"")
            .await
            .unwrap();
        let root = fixture.path().to_str().unwrap();
        let backend = embedded().await;

        assert!(std::path::Path::new(&backend.resolve_path(".").await.unwrap()).is_absolute());

        let entries = backend
            .list_dir(root)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(entries.len(), 2);

        let walked = backend
            .walk_dir(root)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(
            walked
                .iter()
                .any(|entry| entry.relative_path == "nested/child.txt" && entry.depth == 2)
        );

        let contents = backend
            .read_file(fixture.path().join("entry.txt").to_str().unwrap())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .concat();
        assert_eq!(contents, b"payload");

        let written = backend
            .write_file(
                root.to_owned(),
                "copy.txt".into(),
                BackendWriteConflict::FailIfExists,
                stream::once(async { Ok(Bytes::from_static(b"payload")) }).boxed(),
            )
            .await
            .unwrap();
        assert_eq!(written.bytes_written, 7);
        assert_eq!(
            tokio::fs::read(fixture.path().join("copy.txt"))
                .await
                .unwrap(),
            b"payload"
        );

        SessionBackend::close(backend).await.unwrap();
    }

    #[tokio::test]
    async fn read_maps_missing_files_to_io_errors() {
        let backend = embedded().await;

        let error = backend
            .read_file("/definitely/missing/file-peeker-backend-read-fixture")
            .await
            .err()
            .expect("a missing file should fail");

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        SessionBackend::close(backend).await.unwrap();
    }

    /// Closing cancels the server's filesystem service before its listener
    /// stops, so an in-flight stream reports cancellation rather than a
    /// transport failure.
    #[tokio::test]
    async fn closing_ends_an_in_flight_read_with_a_terminal_error() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("payload.bin");
        tokio::fs::write(&path, vec![0x5a; 4 * 1024 * 1024])
            .await
            .unwrap();
        let backend = embedded().await;
        let mut stream = backend.read_file(path.to_str().unwrap()).await.unwrap();

        assert!(stream.next().await.unwrap().is_ok());
        SessionBackend::close(backend).await.unwrap();

        let error = loop {
            match stream.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("a cancelled read must end with an error, not EOF"),
            }
        };
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(stream.next().await.is_none());
    }

    /// Each Session owns its own server, so two backends never share a port,
    /// a token, or a cancellation.
    #[tokio::test]
    async fn embedded_backends_are_independent() {
        let first = embedded().await;
        let second = embedded().await;
        assert_ne!(
            first.connection.info().server_port,
            second.connection.info().server_port
        );
        assert_ne!(
            first.connection.info().token,
            second.connection.info().token
        );

        SessionBackend::close(first).await.unwrap();

        assert!(
            std::path::Path::new(&second.resolve_path(".").await.unwrap()).is_absolute(),
            "closing one embedded backend must not disturb another"
        );
        SessionBackend::close(second).await.unwrap();
    }
}
