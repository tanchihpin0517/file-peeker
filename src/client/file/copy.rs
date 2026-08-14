use std::io;

use super::with_context;
use crate::{
    client::{
        Client, ConnectionId,
        rpc::{self, WriteConflict, WriteResult},
    },
    host_path::HostPath,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CopyConflict {
    FailIfExists,
    Replace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CopyResult {
    pub destination_path: HostPath,
    pub bytes_copied: u64,
}

impl Client {
    /// Streams one regular file from `source` into `destination`.
    ///
    /// Each connection's lock is released before the transfer starts, so a long
    /// copy never blocks either close, and copying within one connection takes
    /// its lock twice in sequence rather than holding both at once.
    ///
    /// # Errors
    ///
    /// Returns an error when either connection is closed, source resolution or
    /// read startup fails, or the destination rejects or cannot publish the
    /// write.
    pub(crate) async fn op_copy_file(
        &self,
        source: &ConnectionId,
        source_path: &HostPath,
        destination: &ConnectionId,
        destination_directory: &HostPath,
        destination_name: &HostPath,
        conflict: CopyConflict,
    ) -> io::Result<CopyResult> {
        let stream = {
            let connection = self.connection(source).await?;
            let resolved_path =
                rpc::resolve_path(&connection, source_path)
                    .await
                    .map_err(|error| {
                        with_context(
                            &error,
                            format!(
                                "cannot resolve source file path `{}`",
                                source_path.display()
                            ),
                        )
                    })?;
            rpc::read_file(&connection, &resolved_path)
                .await
                .map_err(|error| {
                    with_context(
                        &error,
                        format!(
                            "cannot start reading source file `{}`",
                            resolved_path.display()
                        ),
                    )
                })?
        };

        let write = {
            let connection = self.connection(destination).await?;
            rpc::write_file(
                &connection,
                destination_directory.clone(),
                destination_name.clone(),
                match conflict {
                    CopyConflict::FailIfExists => WriteConflict::FailIfExists,
                    CopyConflict::Replace => WriteConflict::Replace,
                },
                stream,
            )
        };

        let WriteResult {
            path,
            bytes_written,
        } = write.await.map_err(|error| {
            with_context(
                &error,
                format!(
                    "cannot copy `{}` into `{}` as `{}`",
                    source_path.display(),
                    destination_directory.display(),
                    destination_name.display()
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
    use super::CopyConflict;
    use crate::client::{Client, connection::ServerLocation};
    use crate::host_path::HostPath;

    #[tokio::test]
    async fn local_connections_stream_files_and_preserve_conflicts() {
        let source_dir = tempfile::tempdir().unwrap();
        let destination_dir = tempfile::tempdir().unwrap();
        let source_path = source_dir.path().join("payload.bin");
        let expected = (0..150_000)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect::<Vec<_>>();
        tokio::fs::write(&source_path, &expected).await.unwrap();
        let client = Client::new();
        let source = client.open(ServerLocation::Embedded).await.unwrap();
        let destination = client.open(ServerLocation::Embedded).await.unwrap();

        let result = client
            .op_copy_file(
                &source,
                &HostPath::from_path(&source_path),
                &destination,
                &HostPath::from_path(destination_dir.path()),
                &"copied.bin".into(),
                CopyConflict::FailIfExists,
            )
            .await
            .unwrap();
        assert_eq!(result.bytes_copied, u64::try_from(expected.len()).unwrap());
        assert_eq!(
            tokio::fs::read(result.destination_path.to_path_buf().unwrap())
                .await
                .unwrap(),
            expected
        );

        let error = client
            .op_copy_file(
                &source,
                &HostPath::from_path(&source_path),
                &destination,
                &HostPath::from_path(destination_dir.path()),
                &"copied.bin".into(),
                CopyConflict::FailIfExists,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);

        client.close_all().await.unwrap();
    }

    /// Both halves of a copy can name one connection: the source lock is
    /// released before the destination lock is taken, so this neither deadlocks
    /// nor needs a second server.
    #[tokio::test]
    async fn a_connection_can_copy_within_itself() {
        let fixture = tempfile::tempdir().unwrap();
        let source_path = fixture.path().join("payload.bin");
        tokio::fs::write(&source_path, b"payload").await.unwrap();
        let client = Client::new();
        let id = client.open(ServerLocation::Embedded).await.unwrap();

        let result = client
            .op_copy_file(
                &id,
                &HostPath::from_path(&source_path),
                &id,
                &HostPath::from_path(fixture.path()),
                &"copied.bin".into(),
                CopyConflict::FailIfExists,
            )
            .await
            .unwrap();

        assert_eq!(result.bytes_copied, 7);
        assert_eq!(
            tokio::fs::read(result.destination_path.to_path_buf().unwrap())
                .await
                .unwrap(),
            b"payload"
        );
        client.close(&id).await.unwrap();
    }
}
