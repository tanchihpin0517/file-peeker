use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use futures::StreamExt as _;
use tokio::{
    fs::{self, File, OpenOptions},
    io::AsyncWriteExt as _,
};
use tokio_util::sync::CancellationToken;

use crate::{
    FsError, FsErrorKind, ReadStream, error::service_cancelled_error, resolve_path::resolve_path,
};

static NEXT_PARTIAL_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteConflict {
    FailIfExists,
    Replace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteResult {
    pub path: String,
    pub bytes_written: u64,
}

struct PartialFileGuard {
    path: Option<PathBuf>,
}

impl PartialFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    async fn remove(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path).await;
        }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for PartialFileGuard {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = fs::remove_file(path).await;
            });
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(crate) async fn write_file(
    directory: &str,
    name: &str,
    conflict: WriteConflict,
    mut source: ReadStream,
    cancellation_token: CancellationToken,
) -> Result<WriteResult, FsError> {
    validate_name(name)?;
    let directory = resolve_path(directory)?;
    let destination = directory.join(name);

    let metadata = cancellable(&cancellation_token, fs::metadata(&directory))
        .await
        .map_err(|error| {
            path_io_error("cannot inspect destination directory", &directory, &error)
        })?;
    if !metadata.is_dir() {
        return Err(FsError::new(
            FsErrorKind::NotDirectory,
            format!("destination `{}` is not a directory", directory.display()),
        ));
    }

    if conflict == WriteConflict::FailIfExists {
        match fs::symlink_metadata(&destination).await {
            Ok(_) => return Err(already_exists(&destination)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(path_io_error(
                    "cannot inspect destination",
                    &destination,
                    &error,
                ));
            }
        }
    }

    let (mut file, partial_path) = create_partial(&directory, name, &cancellation_token).await?;
    let mut guard = PartialFileGuard::new(partial_path.clone());
    let result = async {
        let bytes_written =
            write_source(&mut file, &mut source, &partial_path, &cancellation_token).await?;
        cancellable(&cancellation_token, file.flush())
            .await
            .map_err(|error| path_io_error("cannot flush partial file", &partial_path, &error))?;
        drop(file);

        publish(
            &partial_path,
            &destination,
            conflict,
            &cancellation_token,
            &mut guard,
        )
        .await?;

        let path = destination.into_os_string().into_string().map_err(|_| {
            FsError::new(
                FsErrorKind::InvalidArgument,
                "destination path is not UTF-8",
            )
        })?;
        Ok(WriteResult {
            path,
            bytes_written,
        })
    }
    .await;

    if result.is_err() {
        guard.remove().await;
    }
    result
}

async fn write_source(
    file: &mut File,
    source: &mut ReadStream,
    partial_path: &Path,
    cancellation_token: &CancellationToken,
) -> Result<u64, FsError> {
    let mut bytes_written = 0_u64;
    loop {
        let item = tokio::select! {
            biased;
            () = cancellation_token.cancelled() => return Err(service_cancelled_error()),
            item = source.next() => item,
        };
        let Some(chunk) = item else {
            return Ok(bytes_written);
        };
        let chunk = chunk?;
        if chunk.is_empty() {
            return Err(FsError::new(
                FsErrorKind::InvalidArgument,
                "source emitted an empty data chunk",
            ));
        }
        bytes_written = bytes_written
            .checked_add(u64::try_from(chunk.len()).map_err(|_| {
                FsError::new(FsErrorKind::Internal, "source chunk length exceeds u64")
            })?)
            .ok_or_else(|| FsError::new(FsErrorKind::Internal, "written byte count overflowed"))?;
        cancellable(cancellation_token, file.write_all(&chunk))
            .await
            .map_err(|error| path_io_error("cannot write partial file", partial_path, &error))?;
    }
}

async fn publish(
    partial_path: &Path,
    destination: &Path,
    conflict: WriteConflict,
    cancellation_token: &CancellationToken,
    guard: &mut PartialFileGuard,
) -> Result<(), FsError> {
    if cancellation_token.is_cancelled() {
        return Err(service_cancelled_error());
    }
    match conflict {
        WriteConflict::Replace => {
            fs::rename(partial_path, destination)
                .await
                .map_err(|error| {
                    path_io_error("cannot publish destination", destination, &error)
                })?;
            guard.disarm();
        }
        WriteConflict::FailIfExists => {
            fs::hard_link(partial_path, destination)
                .await
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        already_exists(destination)
                    } else {
                        path_io_error("cannot publish destination", destination, &error)
                    }
                })?;
            guard.remove().await;
        }
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), FsError> {
    let path = Path::new(name);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if component == OsStr::new(name) => Ok(()),
        _ => Err(FsError::new(
            FsErrorKind::InvalidArgument,
            "destination name must be one non-empty normal path component",
        )),
    }
}

async fn create_partial(
    directory: &Path,
    name: &str,
    cancellation_token: &CancellationToken,
) -> Result<(File, PathBuf), FsError> {
    for _ in 0..128 {
        let id = NEXT_PARTIAL_ID.fetch_add(1, Ordering::Relaxed);
        let partial_path = directory.join(format!(
            ".{name}.file-peeker-{}-{id}.partial",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        if cancellation_token.is_cancelled() {
            return Err(service_cancelled_error());
        }
        match options.open(&partial_path).await {
            Ok(file) => return Ok((file, partial_path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(path_io_error(
                    "cannot create partial file",
                    &partial_path,
                    &error,
                ));
            }
        }
    }
    Err(FsError::new(
        FsErrorKind::AlreadyExists,
        "cannot allocate a unique partial filename",
    ))
}

async fn cancellable<T>(
    cancellation_token: &CancellationToken,
    operation: impl Future<Output = std::io::Result<T>>,
) -> std::io::Result<T> {
    tokio::select! {
        biased;
        () = cancellation_token.cancelled() => {
            Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "filesystem service is cancelled"))
        }
        result = operation => result,
    }
}

fn path_io_error(action: &str, path: &Path, error: &std::io::Error) -> FsError {
    FsError::new(
        FsError::from_io(error).kind(),
        format!("{action} `{}`: {error}", path.display()),
    )
}

fn already_exists(path: &Path) -> FsError {
    FsError::new(
        FsErrorKind::AlreadyExists,
        format!("destination `{}` already exists", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures::{StreamExt as _, stream};

    use super::{WriteConflict, write_file};
    use crate::{FsError, FsErrorKind, ReadStream};
    use tokio_util::sync::CancellationToken;

    fn source(items: Vec<Result<&'static [u8], FsError>>) -> ReadStream {
        stream::iter(items.into_iter().map(|item| item.map(Bytes::from_static))).boxed()
    }

    fn partials(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(directory)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.to_string_lossy().ends_with(".partial"))
            .collect()
    }

    #[tokio::test]
    async fn writes_empty_and_multi_chunk_files() {
        let fixture = tempfile::tempdir().unwrap();
        for (name, input, expected) in [
            ("empty", vec![], b"".as_slice()),
            (
                "multi",
                vec![Ok(b"one".as_slice()), Ok(b"two".as_slice())],
                b"onetwo".as_slice(),
            ),
        ] {
            let result = write_file(
                fixture.path().to_str().unwrap(),
                name,
                WriteConflict::FailIfExists,
                source(input),
                CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(result.bytes_written, u64::try_from(expected.len()).unwrap());
            assert_eq!(tokio::fs::read(result.path).await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn rejects_non_component_names() {
        let fixture = tempfile::tempdir().unwrap();
        for name in ["", ".", "..", "/absolute", "nested/name"] {
            let error = write_file(
                fixture.path().to_str().unwrap(),
                name,
                WriteConflict::FailIfExists,
                source(vec![]),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind(), FsErrorKind::InvalidArgument);
        }
    }

    #[tokio::test]
    async fn conflict_does_not_change_existing_file_and_replace_does() {
        let fixture = tempfile::tempdir().unwrap();
        let destination = fixture.path().join("target");
        tokio::fs::write(&destination, b"old").await.unwrap();
        let error = write_file(
            fixture.path().to_str().unwrap(),
            "target",
            WriteConflict::FailIfExists,
            source(vec![Ok(b"new")]),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), FsErrorKind::AlreadyExists);
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"old");

        write_file(
            fixture.path().to_str().unwrap(),
            "target",
            WriteConflict::Replace,
            source(vec![Ok(b"new")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"new");
    }

    #[tokio::test]
    async fn source_error_removes_partial_and_publishes_nothing() {
        let fixture = tempfile::tempdir().unwrap();
        let error = write_file(
            fixture.path().to_str().unwrap(),
            "target",
            WriteConflict::FailIfExists,
            source(vec![
                Ok(b"prefix"),
                Err(FsError::new(FsErrorKind::Internal, "source failed")),
            ]),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), FsErrorKind::Internal);
        assert!(!fixture.path().join("target").exists());
        assert!(partials(fixture.path()).is_empty());
    }

    #[tokio::test]
    async fn aborting_pending_source_cleans_up_partial() {
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().to_str().unwrap().to_owned();
        let pending: ReadStream =
            stream::once(futures::future::pending::<Result<Bytes, FsError>>()).boxed();
        let task = tokio::spawn(async move {
            write_file(
                &directory,
                "target",
                WriteConflict::FailIfExists,
                pending,
                CancellationToken::new(),
            )
            .await
        });
        tokio::task::yield_now().await;
        task.abort();
        let _ = task.await;
        tokio::task::yield_now().await;
        assert!(!fixture.path().join("target").exists());
        assert!(partials(fixture.path()).is_empty());
    }

    #[tokio::test]
    async fn cancellation_cleans_up_a_pending_write() {
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().to_str().unwrap().to_owned();
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let pending: ReadStream =
            stream::once(futures::future::pending::<Result<Bytes, FsError>>()).boxed();
        let task = tokio::spawn(async move {
            write_file(
                &directory,
                "target",
                WriteConflict::FailIfExists,
                pending,
                task_cancellation,
            )
            .await
        });
        while partials(fixture.path()).is_empty() {
            tokio::task::yield_now().await;
        }
        cancellation.cancel();

        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.kind(), FsErrorKind::Cancelled);
        assert!(!fixture.path().join("target").exists());
        assert!(partials(fixture.path()).is_empty());
    }

    #[tokio::test]
    async fn publication_collision_during_stream_does_not_overwrite() {
        use futures::SinkExt as _;

        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().to_str().unwrap().to_owned();
        let (mut sender, receiver) = futures::channel::mpsc::channel(1);
        sender
            .send(Ok(Bytes::from_static(b"prefix")))
            .await
            .unwrap();
        let task = tokio::spawn(async move {
            write_file(
                &directory,
                "target",
                WriteConflict::FailIfExists,
                receiver.boxed(),
                CancellationToken::new(),
            )
            .await
        });
        while partials(fixture.path()).is_empty() {
            tokio::task::yield_now().await;
        }
        tokio::fs::write(fixture.path().join("target"), b"winner")
            .await
            .unwrap();
        drop(sender);

        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.kind(), FsErrorKind::AlreadyExists);
        assert_eq!(
            tokio::fs::read(fixture.path().join("target"))
                .await
                .unwrap(),
            b"winner"
        );
        assert!(partials(fixture.path()).is_empty());
    }
}
