use file_peeker_server::protocol::v1::ReadChunk;
use futures::{StreamExt as _, stream, stream::BoxStream};
use tonic::Status;

use super::status::fs_status;

const GRPC_READ_CHUNK_MAX_BYTES: usize = 64 * 1024;

pub(super) fn read_chunks(
    stream: file_peeker_core::ReadStream,
) -> BoxStream<'static, Result<ReadChunk, Status>> {
    stream::unfold(Some((stream, None)), |state| async move {
        let (mut stream, pending) = state?;
        let mut bytes = match pending {
            Some(bytes) => bytes,
            None => match stream.next().await {
                Some(Ok(bytes)) => bytes,
                Some(Err(error)) => return Some((Err(fs_status(&error)), None)),
                None => return None,
            },
        };
        debug_assert!(!bytes.is_empty());
        let chunk = bytes.split_to(bytes.len().min(GRPC_READ_CHUNK_MAX_BYTES));
        let pending = if bytes.is_empty() { None } else { Some(bytes) };
        Some((
            Ok(ReadChunk {
                data: chunk.to_vec(),
            }),
            Some((stream, pending)),
        ))
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use file_peeker_core::{FsError, FsErrorKind};
    use futures::{StreamExt as _, TryStreamExt as _, stream};
    use tonic::Code;

    use super::{GRPC_READ_CHUNK_MAX_BYTES, read_chunks};

    #[tokio::test]
    async fn splits_arbitrary_core_chunks_at_the_grpc_limit() {
        let small = b"prefix".to_vec();
        let large = vec![0x5a; GRPC_READ_CHUNK_MAX_BYTES * 2 + 17];
        let expected = [small.as_slice(), large.as_slice()].concat();
        let stream: file_peeker_core::ReadStream =
            stream::iter([Ok(small.into()), Ok(large.into())]).boxed();

        let chunks = read_chunks(stream).try_collect::<Vec<_>>().await.unwrap();

        assert!(
            chunks.iter().all(
                |chunk| !chunk.data.is_empty() && chunk.data.len() <= GRPC_READ_CHUNK_MAX_BYTES
            )
        );
        assert_eq!(
            chunks
                .into_iter()
                .flat_map(|chunk| chunk.data)
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[tokio::test]
    async fn empty_files_emit_no_read_chunks() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("empty.bin");
        tokio::fs::write(&path, b"").await.unwrap();
        let reader = file_peeker_core::FsService::new()
            .read_file(path.to_str().unwrap())
            .await
            .unwrap();

        assert!(
            read_chunks(reader)
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn read_chunks_preserve_bytes_before_a_terminal_core_error() {
        let stream: file_peeker_core::ReadStream = stream::iter([
            Ok(b"kept".to_vec().into()),
            Err(FsError::new(FsErrorKind::PermissionDenied, "denied")),
        ])
        .boxed();
        let mut chunks = read_chunks(stream);

        assert_eq!(chunks.next().await.unwrap().unwrap().data, b"kept");
        let status = chunks.next().await.unwrap().unwrap_err();
        assert_eq!(status.code(), Code::PermissionDenied);
        assert_eq!(status.message(), "denied");
        assert!(chunks.next().await.is_none());
    }
}
