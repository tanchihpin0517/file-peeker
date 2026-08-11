//! The calling side of the File Peeker protocol.
//!
//! The calling side of the File Peeker protocol.
//!
//! Each module turns one Session operation into a request against a
//! [`ServerConnection`](crate::session::connection::ServerConnection) and
//! validates the response. The handling side lives in [`crate::server`];
//! nothing here performs filesystem work.

mod entry;
mod error;
mod list_dir;
mod read_file;
mod resolve_path;
mod walk_dir;
mod write_file;

pub(crate) use list_dir::list_dir;
pub(crate) use read_file::read_file;
pub(crate) use resolve_path::resolve_path;
pub(crate) use walk_dir::walk_dir;
pub(crate) use write_file::write_file;

use std::io;

use bytes::Bytes;
use futures::{future::BoxFuture, stream::BoxStream};

/// A file's bytes, as the caller of a Read request sees them.
pub(crate) type ReadStream = BoxStream<'static, io::Result<Bytes>>;

/// A Write request in flight. Built while the connection lock is held, awaited
/// after it is released, so a long transfer never blocks a session close.
pub(crate) type WriteFuture = BoxFuture<'static, io::Result<WriteResult>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WriteConflict {
    FailIfExists,
    Replace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WriteResult {
    pub(crate) path: String,
    pub(crate) bytes_written: u64,
}

#[cfg(test)]
mod tests {
    use std::io;

    use bytes::Bytes;
    use futures::{StreamExt as _, TryStreamExt as _, stream};

    use super::{WriteConflict, list_dir, read_file, resolve_path, walk_dir, write_file};
    use crate::session::connection::{ServerConnection, ServerLocation};

    async fn embedded() -> ServerConnection {
        ServerConnection::open(ServerLocation::Embedded)
            .await
            .expect("an embedded server should start")
    }

    #[tokio::test]
    async fn rejects_empty_destination_on_every_attempt() {
        for _ in 0..2 {
            let error = ServerConnection::open(ServerLocation::Ssh {
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
        let connection = embedded().await;

        assert!(std::path::Path::new(&resolve_path(&connection, ".").await.unwrap()).is_absolute());

        let entries = list_dir(&connection, root)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(entries.len(), 2);

        let walked = walk_dir(&connection, root)
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

        let contents = read_file(
            &connection,
            fixture.path().join("entry.txt").to_str().unwrap(),
        )
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap()
        .concat();
        assert_eq!(contents, b"payload");

        let written = write_file(
            &connection,
            root.to_owned(),
            "copy.txt".into(),
            WriteConflict::FailIfExists,
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

        connection.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn read_maps_missing_files_to_io_errors() {
        let connection = embedded().await;

        let error = read_file(
            &connection,
            "/definitely/missing/file-peeker-backend-read-fixture",
        )
        .await
        .err()
        .expect("a missing file should fail");

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        connection.shutdown().await.unwrap();
    }

    /// Shutdown cancels the server's filesystem service before its listener
    /// stops, so an in-flight stream reports cancellation rather than a
    /// transport failure.
    #[tokio::test]
    async fn shutdown_ends_an_in_flight_read_with_a_terminal_error() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("payload.bin");
        tokio::fs::write(&path, vec![0x5a; 4 * 1024 * 1024])
            .await
            .unwrap();
        let connection = embedded().await;
        let mut stream = read_file(&connection, path.to_str().unwrap())
            .await
            .unwrap();

        assert!(stream.next().await.unwrap().is_ok());
        connection.shutdown().await.unwrap();

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

    /// Each Session owns its own server, so two connections never share a port,
    /// a token, or a cancellation.
    #[tokio::test]
    async fn embedded_connections_are_independent() {
        let first = embedded().await;
        let second = embedded().await;
        assert_ne!(first.info().server_port, second.info().server_port);
        assert_ne!(first.info().token, second.info().token);

        first.shutdown().await.unwrap();

        assert!(
            std::path::Path::new(&resolve_path(&second, ".").await.unwrap()).is_absolute(),
            "shutting down one embedded server must not disturb another"
        );
        second.shutdown().await.unwrap();
    }
}
