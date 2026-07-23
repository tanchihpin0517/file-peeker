use std::path::PathBuf;

use futures::{StreamExt as _, stream, stream::BoxStream};
use tokio::fs;
use tokio_util::sync::CancellationToken;

use super::entry::inspect_entry;
use crate::{DirectoryEntry, FsError, error::service_cancelled_error, resolve_path::resolve_path};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WalkEntry {
    pub relative_path: String,
    pub entry: DirectoryEntry,
    pub depth: usize,
}

pub type WalkStream = BoxStream<'static, Result<WalkEntry, FsError>>;

struct WalkState {
    stack: Vec<DirectoryFrame>,
    pending_descent: Option<PendingDirectory>,
    cancellation: CancellationToken,
}

struct DirectoryFrame {
    entries: fs::ReadDir,
    relative_dir: PathBuf,
    child_depth: usize,
}

struct PendingDirectory {
    path: PathBuf,
    relative_dir: PathBuf,
    child_depth: usize,
}

pub(crate) async fn walk_dir(
    path: &str,
    cancellation: CancellationToken,
) -> Result<WalkStream, FsError> {
    let root = resolve_path(path)?;
    let entries = open_directory(&root, &cancellation).await?;
    let initial = WalkState {
        stack: vec![DirectoryFrame {
            entries,
            relative_dir: PathBuf::new(),
            child_depth: 1,
        }],
        pending_descent: None,
        cancellation,
    };

    Ok(stream::unfold(Some(initial), |state| async move {
        let mut state = state?;
        loop {
            if let Some(pending) = state.pending_descent.take() {
                let entries = match open_directory(&pending.path, &state.cancellation).await {
                    Ok(entries) => entries,
                    Err(error) => return Some((Err(error), None)),
                };
                state.stack.push(DirectoryFrame {
                    entries,
                    relative_dir: pending.relative_dir,
                    child_depth: pending.child_depth,
                });
            }

            let frame = state.stack.last_mut()?;
            let next = tokio::select! {
                biased;
                () = state.cancellation.cancelled() => {
                    return Some((Err(service_cancelled_error()), None));
                }
                result = frame.entries.next_entry() => result,
            };
            let directory_entry = match next {
                Ok(Some(entry)) => entry,
                Ok(None) => {
                    state.stack.pop();
                    continue;
                }
                Err(error) => return Some((Err(FsError::from_io(&error)), None)),
            };
            let inspected = tokio::select! {
                biased;
                () = state.cancellation.cancelled() => {
                    return Some((Err(service_cancelled_error()), None));
                }
                result = inspect_entry(&directory_entry) => match result {
                    Ok(entry) => entry,
                    Err(error) => return Some((Err(error), None)),
                },
            };
            let relative_path = frame.relative_dir.join(&inspected.entry.name);
            let relative_path_string = match relative_path.to_str() {
                Some(path) if !path.is_empty() => path.to_owned(),
                _ => {
                    return Some((
                        Err(FsError::new(
                            crate::FsErrorKind::InvalidArgument,
                            "Encountered a non-UTF-8 relative path",
                        )),
                        None,
                    ));
                }
            };
            let depth = frame.child_depth;
            if inspected.descend {
                state.pending_descent = Some(PendingDirectory {
                    path: inspected.path,
                    relative_dir: relative_path,
                    child_depth: depth + 1,
                });
            }
            return Some((
                Ok(WalkEntry {
                    relative_path: relative_path_string,
                    entry: inspected.entry,
                    depth,
                }),
                Some(state),
            ));
        }
    })
    .boxed())
}

async fn open_directory(
    path: &std::path::Path,
    cancellation: &CancellationToken,
) -> Result<fs::ReadDir, FsError> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(service_cancelled_error()),
        result = fs::read_dir(path) => result.map_err(|error| {
            FsError::new(
                FsError::from_io(&error).kind(),
                format!("cannot walk `{}`: {error}", path.display()),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use futures::{StreamExt as _, TryStreamExt as _};
    use tokio_util::sync::CancellationToken;

    use super::walk_dir;
    use crate::{EntryKind, FsErrorKind};

    #[tokio::test]
    async fn empty_roots_yield_no_entries() {
        let fixture = tempfile::tempdir().unwrap();
        let entries = walk_dir(fixture.path().to_str().unwrap(), CancellationToken::new())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn walks_nested_trees_depth_first_with_relative_paths() {
        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(fixture.path().join("one/two"))
            .await
            .unwrap();
        tokio::fs::write(fixture.path().join("root.txt"), b"")
            .await
            .unwrap();
        tokio::fs::write(fixture.path().join("one/child.txt"), b"")
            .await
            .unwrap();
        tokio::fs::write(fixture.path().join("one/two/deep.txt"), b"")
            .await
            .unwrap();

        let entries = walk_dir(fixture.path().to_str().unwrap(), CancellationToken::new())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        let by_path = entries
            .iter()
            .map(|entry| (entry.relative_path.as_str(), entry))
            .collect::<HashMap<_, _>>();

        assert_eq!(by_path.len(), 5);
        assert_eq!(by_path["root.txt"].depth, 1);
        assert_eq!(by_path["one"].depth, 1);
        assert_eq!(by_path["one/child.txt"].depth, 2);
        assert_eq!(by_path["one/two"].depth, 2);
        assert_eq!(by_path["one/two/deep.txt"].depth, 3);
        assert!(entries.iter().all(|entry| {
            std::path::Path::new(&entry.relative_path)
                .file_name()
                .is_some_and(|name| name == entry.entry.name.as_str())
        }));
        let one = entries
            .iter()
            .position(|entry| entry.relative_path == "one")
            .unwrap();
        let child = entries
            .iter()
            .position(|entry| entry.relative_path == "one/child.txt")
            .unwrap();
        let two = entries
            .iter()
            .position(|entry| entry.relative_path == "one/two")
            .unwrap();
        let deep = entries
            .iter()
            .position(|entry| entry.relative_path == "one/two/deep.txt")
            .unwrap();
        assert!(one < child);
        assert!(one < two && two < deep);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn emits_directory_symlinks_without_following_them() {
        use std::os::unix::fs::symlink;

        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(fixture.path().join("target"))
            .await
            .unwrap();
        tokio::fs::write(fixture.path().join("target/inside.txt"), b"")
            .await
            .unwrap();
        symlink("target", fixture.path().join("linked")).unwrap();

        let entries = walk_dir(fixture.path().to_str().unwrap(), CancellationToken::new())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        let linked = entries
            .iter()
            .find(|entry| entry.relative_path == "linked")
            .unwrap();
        assert_eq!(linked.entry.kind, EntryKind::Symlink);
        assert!(linked.entry.navigable);
        assert!(
            !entries
                .iter()
                .any(|entry| entry.relative_path == "linked/inside.txt")
        );
    }

    #[tokio::test]
    async fn rejects_non_directory_roots_before_returning_a_stream() {
        let fixture = tempfile::NamedTempFile::new().unwrap();
        let error = walk_dir(fixture.path().to_str().unwrap(), CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind(), FsErrorKind::NotDirectory);
    }

    #[tokio::test]
    async fn missing_roots_fail_before_returning_a_stream() {
        let error = walk_dir(
            "/definitely/missing/file-peeker-walk-fixture",
            CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind(), FsErrorKind::NotFound);
    }

    #[tokio::test]
    async fn cancellation_is_terminal_after_partial_consumption() {
        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::write(fixture.path().join("one"), b"")
            .await
            .unwrap();
        tokio::fs::write(fixture.path().join("two"), b"")
            .await
            .unwrap();
        let cancellation = CancellationToken::new();
        let mut stream = walk_dir(fixture.path().to_str().unwrap(), cancellation.clone())
            .await
            .unwrap();
        assert!(stream.next().await.unwrap().is_ok());
        cancellation.cancel();
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.kind(), FsErrorKind::Cancelled);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn descendant_open_failures_follow_the_emitted_directory() {
        let fixture = tempfile::tempdir().unwrap();
        let child = fixture.path().join("child");
        tokio::fs::create_dir(&child).await.unwrap();
        let mut stream = walk_dir(fixture.path().to_str().unwrap(), CancellationToken::new())
            .await
            .unwrap();
        let emitted = stream.next().await.unwrap().unwrap();
        assert_eq!(emitted.relative_path, "child");
        tokio::fs::remove_dir(&child).await.unwrap();

        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.kind(), FsErrorKind::NotFound);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn dropping_one_walk_does_not_cancel_sibling_operations() {
        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::write(fixture.path().join("entry"), b"payload")
            .await
            .unwrap();
        let service = crate::FsService::new();
        let walk = service
            .walk_dir(fixture.path().to_str().unwrap())
            .await
            .unwrap();
        drop(walk);

        assert_eq!(
            service
                .list_dir(fixture.path().to_str().unwrap())
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            service
                .walk_dir(fixture.path().to_str().unwrap())
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_utf8_descendants_fail_terminally() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt as _};

        let fixture = tempfile::tempdir().unwrap();
        let write_result = tokio::fs::write(
            fixture.path().join(OsString::from_vec(vec![0xff, 0xfe])),
            b"",
        )
        .await;
        if write_result.is_err() {
            return;
        }
        let error = walk_dir(fixture.path().to_str().unwrap(), CancellationToken::new())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), FsErrorKind::InvalidArgument);
    }
}
