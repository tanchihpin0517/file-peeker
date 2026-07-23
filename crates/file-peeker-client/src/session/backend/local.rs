use std::io;

use async_trait::async_trait;
use file_peeker_core::FsService;
use futures::StreamExt as _;

use super::{ReadStream, SessionBackend, error::fs_error};
use crate::{EntryStream, WalkStream};

#[async_trait]
impl SessionBackend for FsService {
    async fn resolve_path(&self, path: &str) -> io::Result<String> {
        FsService::resolve_path(self, path).map_err(|error| fs_error(&error))
    }

    async fn list_dir(&self, path: &str) -> io::Result<EntryStream> {
        let stream = FsService::list_dir(self, path)
            .await
            .map_err(|error| fs_error(&error))?;
        Ok(stream
            .map(|result| result.map_err(|error| fs_error(&error)))
            .boxed())
    }

    async fn walk_dir(&self, path: &str) -> io::Result<WalkStream> {
        let stream = FsService::walk_dir(self, path)
            .await
            .map_err(|error| fs_error(&error))?;
        Ok(stream
            .map(|result| result.map_err(|error| fs_error(&error)))
            .boxed())
    }

    async fn read_file(&self, path: &str) -> io::Result<ReadStream> {
        let stream = FsService::read_file(self, path)
            .await
            .map_err(|error| fs_error(&error))?;
        Ok(stream
            .map(|result| result.map_err(|error| fs_error(&error)))
            .boxed())
    }

    async fn close(self: Box<Self>) -> io::Result<()> {
        self.cancel();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use futures::{StreamExt as _, TryStreamExt as _};

    use super::{FsService, SessionBackend};

    #[tokio::test]
    async fn serves_native_operations_through_the_backend_seam() {
        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::write(fixture.path().join("entry.txt"), b"payload")
            .await
            .unwrap();
        let backend = FsService::new();

        assert!(
            std::path::Path::new(&SessionBackend::resolve_path(&backend, ".").await.unwrap())
                .is_absolute()
        );
        let entries = SessionBackend::list_dir(&backend, fixture.path().to_str().unwrap())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        tokio::fs::create_dir(fixture.path().join("nested"))
            .await
            .unwrap();
        tokio::fs::write(fixture.path().join("nested/child.txt"), b"")
            .await
            .unwrap();
        let walked = SessionBackend::walk_dir(&backend, fixture.path().to_str().unwrap())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(
            walked
                .iter()
                .any(|entry| { entry.relative_path == "nested/child.txt" && entry.depth == 2 })
        );
        let contents =
            SessionBackend::read_file(&backend, fixture.path().join("entry.txt").to_str().unwrap())
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
        assert_eq!(contents, b"payload");

        SessionBackend::close(Box::new(backend)).await.unwrap();
    }

    #[tokio::test]
    async fn read_maps_file_open_failures_to_io_errors() {
        let backend = FsService::new();

        let error = SessionBackend::read_file(
            &backend,
            "/definitely/missing/file-peeker-backend-read-fixture",
        )
        .await
        .err()
        .expect("missing file should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn read_maps_service_cancellation_to_a_terminal_io_error() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("payload.bin");
        tokio::fs::write(&path, vec![0x5a; 1024]).await.unwrap();
        let backend = FsService::new();
        let mut stream = SessionBackend::read_file(&backend, path.to_str().unwrap())
            .await
            .unwrap();

        assert!(stream.next().await.unwrap().is_ok());
        backend.cancel();
        let error = stream.next().await.unwrap().unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn walk_maps_service_cancellation_to_a_terminal_io_error() {
        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::write(fixture.path().join("one"), b"")
            .await
            .unwrap();
        tokio::fs::write(fixture.path().join("two"), b"")
            .await
            .unwrap();
        let backend = FsService::new();
        let mut stream = SessionBackend::walk_dir(&backend, fixture.path().to_str().unwrap())
            .await
            .unwrap();
        assert!(stream.next().await.unwrap().is_ok());
        backend.cancel();
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(stream.next().await.is_none());
    }
}
