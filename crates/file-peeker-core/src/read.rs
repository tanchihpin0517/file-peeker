use bytes::Bytes;
use futures::{StreamExt as _, stream, stream::BoxStream};
use tokio::fs::File;
use tokio_util::{io::ReaderStream, sync::CancellationToken};

use crate::{FsError, FsErrorKind, error::service_cancelled_error, resolve_path::resolve_path};

/// A demand-driven stream of ordered, non-empty file bytes.
///
/// An empty file produces no items. Concatenating successful chunks reconstructs
/// the file from byte zero, but chunk counts, sizes, and split positions are not
/// stable. A later filesystem or service-cancellation failure is the final item,
/// followed by stream completion. Dropping a stream releases that read without
/// cancelling sibling operations.
pub type ReadStream = BoxStream<'static, Result<Bytes, FsError>>;

pub(crate) async fn read_file(
    path: &str,
    cancellation_token: CancellationToken,
) -> Result<ReadStream, FsError> {
    let path = resolve_path(path)?;
    let open_file = async {
        let file = File::open(&path).await.map_err(|error| {
            FsError::new(
                FsError::from_io(&error).kind(),
                format!("cannot open `{}`: {error}", path.display()),
            )
        })?;
        let metadata = file.metadata().await.map_err(|error| {
            FsError::new(
                FsError::from_io(&error).kind(),
                format!("cannot inspect `{}`: {error}", path.display()),
            )
        })?;
        if !metadata.is_file() {
            return Err(FsError::new(
                FsErrorKind::NotFile,
                format!("cannot read `{}`: not a regular file", path.display()),
            ));
        }
        Ok(file)
    };
    let file = tokio::select! {
        biased;
        () = cancellation_token.cancelled() => return Err(service_cancelled_error()),
        result = open_file => result?,
    };
    let reader = ReaderStream::new(file);
    Ok(
        stream::unfold(Some((reader, cancellation_token)), |state| async move {
            let (mut reader, cancellation_token) = state?;
            tokio::select! {
                biased;
                () = cancellation_token.cancelled() => {
                    Some((Err(service_cancelled_error()), None))
                }
                item = reader.next() => match item {
                    Some(Ok(bytes)) => {
                        debug_assert!(!bytes.is_empty());
                        Some((Ok(bytes), Some((reader, cancellation_token))))
                    }
                    Some(Err(error)) => Some((Err(FsError::from_io(&error)), None)),
                    None => None,
                }
            }
        })
        .boxed(),
    )
}

#[cfg(test)]
mod tests {
    use futures::{StreamExt as _, TryStreamExt as _};
    use tokio_util::sync::CancellationToken;

    use super::read_file;
    use crate::FsErrorKind;

    #[tokio::test]
    async fn reads_file_contents_incrementally() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("payload.bin");
        let expected = (0_usize..128 * 1024 + 17)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect::<Vec<_>>();
        tokio::fs::write(&path, &expected).await.unwrap();

        let chunks = read_file(path.to_str().unwrap(), CancellationToken::new())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(chunks.iter().all(|chunk| !chunk.is_empty()));
        let actual = chunks.into_iter().flatten().collect::<Vec<_>>();

        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn empty_files_emit_no_items() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("empty.bin");
        tokio::fs::write(&path, b"").await.unwrap();

        let items = read_file(path.to_str().unwrap(), CancellationToken::new())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();

        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn missing_files_fail_before_returning_a_stream() {
        let error = read_file(
            "/definitely/missing/file-peeker-read-fixture",
            CancellationToken::new(),
        )
        .await
        .err()
        .expect("missing file should fail");

        assert_eq!(error.kind(), FsErrorKind::NotFound);
    }

    #[tokio::test]
    async fn directories_fail_before_returning_a_stream() {
        let fixture = tempfile::tempdir().unwrap();

        let error = read_file(fixture.path().to_str().unwrap(), CancellationToken::new())
            .await
            .err()
            .expect("directory target should fail");

        assert_eq!(error.kind(), FsErrorKind::NotFile);
        assert!(error.message().contains(fixture.path().to_str().unwrap()));
    }

    #[tokio::test]
    async fn service_cancellation_terminates_a_partially_consumed_stream() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("payload.bin");
        tokio::fs::write(&path, vec![0x5a; 1024]).await.unwrap();
        let cancellation_token = CancellationToken::new();
        let mut stream = read_file(path.to_str().unwrap(), cancellation_token.clone())
            .await
            .unwrap();

        let prefix = stream.next().await.unwrap().unwrap();
        assert!(!prefix.is_empty());

        cancellation_token.cancel();
        let error = stream.next().await.unwrap().unwrap_err();

        assert_eq!(error.kind(), FsErrorKind::Cancelled);
        assert!(stream.next().await.is_none());
    }
}
