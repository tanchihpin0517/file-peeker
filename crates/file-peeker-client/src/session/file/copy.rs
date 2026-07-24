use std::io;

use super::with_context;
use crate::session::{
    Session,
    backend::{BackendWriteConflict, BackendWriteResult},
    session_closed,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyConflict {
    FailIfExists,
    Replace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyResult {
    pub destination_path: String,
    pub bytes_copied: u64,
}

impl Session {
    /// Streams one regular file from `source` into this destination session.
    ///
    /// # Errors
    ///
    /// Returns an error when either session is closed, source resolution/read
    /// startup fails, or the destination rejects or cannot publish the write.
    pub async fn op_copy_file_from(
        &self,
        source: &Session,
        source_path: &str,
        destination_directory: &str,
        destination_name: &str,
        conflict: CopyConflict,
    ) -> io::Result<CopyResult> {
        let stream = {
            let backend = source.backend.read().await;
            let backend = backend.as_ref().ok_or_else(session_closed)?;
            let resolved_path = backend.resolve_path(source_path).await.map_err(|error| {
                with_context(
                    &error,
                    format!("cannot resolve source file path `{source_path}`"),
                )
            })?;
            backend.read_file(&resolved_path).await.map_err(|error| {
                with_context(
                    &error,
                    format!("cannot start reading source file `{resolved_path}`"),
                )
            })?
        };

        let write = {
            let backend = self.backend.read().await;
            let backend = backend.as_ref().ok_or_else(session_closed)?;
            backend.write_file(
                destination_directory.to_owned(),
                destination_name.to_owned(),
                match conflict {
                    CopyConflict::FailIfExists => BackendWriteConflict::FailIfExists,
                    CopyConflict::Replace => BackendWriteConflict::Replace,
                },
                stream,
            )
        };

        let BackendWriteResult {
            path,
            bytes_written,
        } = write.await.map_err(|error| {
            with_context(
                &error,
                format!(
                    "cannot copy `{source_path}` into `{destination_directory}` as `{destination_name}`"
                ),
            )
        })?;
        Ok(CopyResult {
            destination_path: path,
            bytes_copied: bytes_written,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CopyConflict, Session};
    use crate::SessionTarget;

    #[tokio::test]
    async fn local_sessions_stream_files_and_preserve_conflicts() {
        let source_dir = tempfile::tempdir().unwrap();
        let destination_dir = tempfile::tempdir().unwrap();
        let source_path = source_dir.path().join("payload.bin");
        let expected = (0..150_000)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect::<Vec<_>>();
        tokio::fs::write(&source_path, &expected).await.unwrap();
        let source = Session::start("source".into(), SessionTarget::Local)
            .await
            .unwrap();
        let destination = Session::start("destination".into(), SessionTarget::Local)
            .await
            .unwrap();

        let result = destination
            .op_copy_file_from(
                &source,
                source_path.to_str().unwrap(),
                destination_dir.path().to_str().unwrap(),
                "copied.bin",
                CopyConflict::FailIfExists,
            )
            .await
            .unwrap();
        assert_eq!(result.bytes_copied, u64::try_from(expected.len()).unwrap());
        assert_eq!(
            tokio::fs::read(&result.destination_path).await.unwrap(),
            expected
        );

        let error = destination
            .op_copy_file_from(
                &source,
                source_path.to_str().unwrap(),
                destination_dir.path().to_str().unwrap(),
                "copied.bin",
                CopyConflict::FailIfExists,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);

        source.close().await.unwrap();
        destination.close().await.unwrap();
    }
}
