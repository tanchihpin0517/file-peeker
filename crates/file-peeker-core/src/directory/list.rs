use futures::{StreamExt, stream, stream::BoxStream};
use tokio::fs;
use tokio_util::sync::CancellationToken;

use super::{DirectoryEntry, entry::inspect_entry};
use crate::{FsError, error::service_cancelled_error, resolve_path::resolve_path};

pub type EntryStream = BoxStream<'static, Result<DirectoryEntry, FsError>>;

pub(crate) async fn list_dir(
    path: &str,
    cancellation_token: CancellationToken,
) -> Result<EntryStream, FsError> {
    let path = resolve_path(path)?;
    let directory_entries = tokio::select! {
        biased;
        () = cancellation_token.cancelled() => return Err(service_cancelled_error()),
        result = fs::read_dir(&path) => result.map_err(|error| {
            FsError::new(
                FsError::from_io(&error).kind(),
                format!("cannot list `{}`: {error}", path.display()),
            )
        })?,
    };
    let stream = stream::unfold(Some(directory_entries), move |state| {
        let cancellation_token = cancellation_token.clone();
        async move {
            let mut directory_entries = state?;
            let next_entry = tokio::select! {
                biased;
                () = cancellation_token.cancelled() => {
                    return Some((Err(service_cancelled_error()), None));
                }
                result = directory_entries.next_entry() => result,
            };
            match next_entry {
                Ok(Some(entry)) => {
                    let result = tokio::select! {
                        biased;
                        () = cancellation_token.cancelled() => {
                            return Some((Err(service_cancelled_error()), None));
                        }
                        result = inspect_entry(&entry) => result.map(|inspected| inspected.entry),
                    };
                    let next_state = result.is_ok().then_some(directory_entries);
                    Some((result, next_state))
                }
                Ok(None) => None,
                Err(error) => Some((Err(FsError::from_io(&error)), None)),
            }
        }
    });

    Ok(stream.boxed())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::{StreamExt as _, TryStreamExt};
    use tokio_util::sync::CancellationToken;

    use super::list_dir;
    use crate::{EntryKind, FsErrorKind};

    #[tokio::test]
    async fn lists_entry_kinds_and_directory_symlinks() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("file"), b"").unwrap();
        std::fs::create_dir(fixture.path().join("directory")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("directory", fixture.path().join("link")).unwrap();

        let entries = list_dir(fixture.path().to_str().unwrap(), CancellationToken::new())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(entries.iter().any(|entry| entry.name == "file"
            && entry.kind == EntryKind::File
            && !entry.navigable));
        assert!(entries.iter().any(|entry| entry.name == "directory"
            && entry.kind == EntryKind::Directory
            && entry.navigable));
        #[cfg(unix)]
        assert!(entries.iter().any(|entry| entry.name == "link"
            && entry.kind == EntryKind::Symlink
            && entry.navigable));
    }

    #[tokio::test]
    async fn emits_each_directory_entry_once() {
        let fixture = tempfile::tempdir().unwrap();
        for index in 0..=1024 {
            tokio::fs::write(fixture.path().join(format!("file-{index}")), b"")
                .await
                .unwrap();
        }

        let entries = list_dir(fixture.path().to_str().unwrap(), CancellationToken::new())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();

        assert_eq!(entries.len(), 1025);
    }

    #[tokio::test]
    async fn empty_directory_completes_without_entries() {
        let fixture = tempfile::tempdir().unwrap();
        let entries = list_dir(fixture.path().to_str().unwrap(), CancellationToken::new())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();

        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn rejects_file_targets_before_returning_a_stream() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("file.txt");
        tokio::fs::write(&path, b"payload").await.unwrap();

        let error = list_dir(path.to_str().unwrap(), CancellationToken::new())
            .await
            .err()
            .expect("file target should fail");

        assert_eq!(error.kind(), FsErrorKind::NotDirectory);
        assert!(error.message().contains(path.to_str().unwrap()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn broken_symlinks_are_non_navigable_entries() {
        let fixture = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("missing", fixture.path().join("broken")).unwrap();

        let entries = list_dir(fixture.path().to_str().unwrap(), CancellationToken::new())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, EntryKind::Symlink);
        assert!(!entries[0].navigable);
    }

    #[cfg(all(unix, not(target_vendor = "apple")))]
    #[tokio::test]
    async fn non_utf8_filenames_emit_a_terminal_error() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt as _};

        let fixture = tempfile::tempdir().unwrap();
        let name = OsString::from_vec(vec![b'f', 0x80]);
        tokio::fs::write(fixture.path().join(name), b"")
            .await
            .unwrap();
        let mut entries = list_dir(fixture.path().to_str().unwrap(), CancellationToken::new())
            .await
            .unwrap();

        let error = entries.next().await.unwrap().unwrap_err();

        assert_eq!(error.kind(), FsErrorKind::InvalidArgument);
        assert!(entries.next().await.is_none());
    }

    #[tokio::test]
    async fn service_cancellation_errors_a_partially_consumed_stream() {
        let fixture = tempfile::tempdir().unwrap();
        for index in 0..=2048 {
            std::fs::write(fixture.path().join(format!("file-{index}")), b"").unwrap();
        }
        let cancellation_token = CancellationToken::new();
        let mut stream = list_dir(fixture.path().to_str().unwrap(), cancellation_token.clone())
            .await
            .unwrap();

        assert!(stream.next().await.is_some());
        cancellation_token.cancel();

        let error = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("cancelled stream should respond promptly")
            .expect("cancelled stream should emit an error")
            .unwrap_err();
        assert_eq!(error.kind(), FsErrorKind::Cancelled);
        assert!(stream.next().await.is_none());
    }
}
