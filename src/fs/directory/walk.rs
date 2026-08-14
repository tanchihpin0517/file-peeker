use std::path::PathBuf;

use futures::{StreamExt as _, stream, stream::BoxStream};
use tokio::fs;
use tokio_util::sync::CancellationToken;

use super::entry::inspect_entry;
use crate::{
    fs::{DirectoryEntry, FsError, error::service_cancelled_error, resolve_path::resolve_path},
    host_path::HostPath,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WalkEntry {
    /// Escaped, for the same reason [`DirectoryEntry::name`] is: a component of it
    /// may be bytes no `String` can hold.
    pub relative_path: HostPath,
    pub entry: DirectoryEntry,
    pub depth: usize,
}

pub(crate) type WalkStream = BoxStream<'static, Result<WalkEntry, FsError>>;

struct WalkState {
    stack: Vec<DirectoryFrame>,
    pending_descent: Option<PendingDirectory>,
    cancellation: CancellationToken,
}

struct DirectoryFrame {
    entries: fs::ReadDir,
    /// Escaped and root-relative, which is what the entries report. Not a real path
    /// — see [`PendingDirectory::path`] for that.
    relative_dir: HostPath,
    child_depth: usize,
}

struct PendingDirectory {
    /// The real path on this host, which is what gets opened.
    path: PathBuf,
    /// The escaped root-relative path, which is what gets reported.
    relative_dir: HostPath,
    child_depth: usize,
}

pub(crate) async fn walk_dir(
    path: &HostPath,
    cancellation: CancellationToken,
) -> Result<WalkStream, FsError> {
    let root = resolve_path(path)?;
    let entries = open_directory(&root, &cancellation).await?;
    let initial = WalkState {
        stack: vec![DirectoryFrame {
            entries,
            relative_dir: HostPath::from_literal(""),
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
            // Infallible: an entry it could not describe is emitted degraded rather
            // than ending the traversal, and is not descended into.
            let inspected = tokio::select! {
                biased;
                () = state.cancellation.cancelled() => {
                    return Some((Err(service_cancelled_error()), None));
                }
                inspected = inspect_entry(&directory_entry) => inspected,
            };
            // Escaped throughout: joining escaped halves is the escape of the joined
            // path, so a descendant whose name is not UTF-8 no longer ends the walk.
            let relative_path = frame.relative_dir.join(&inspected.entry.name);
            let depth = frame.child_depth;
            if inspected.descend {
                state.pending_descent = Some(PendingDirectory {
                    path: inspected.path,
                    relative_dir: relative_path.clone(),
                    child_depth: depth + 1,
                });
            }
            return Some((
                Ok(WalkEntry {
                    relative_path,
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
    use crate::{
        fs::{EntryKind, FsErrorKind},
        host_path::HostPath,
    };

    #[tokio::test]
    async fn empty_roots_yield_no_entries() {
        let fixture = tempfile::tempdir().unwrap();
        let entries = walk_dir(
            &HostPath::from_path(fixture.path()),
            CancellationToken::new(),
        )
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

        let entries = walk_dir(
            &HostPath::from_path(fixture.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
        let by_path = entries
            .iter()
            .map(|entry| (entry.relative_path.as_escaped(), entry))
            .collect::<HashMap<_, _>>();

        assert_eq!(by_path.len(), 5);
        assert_eq!(by_path["root.txt"].depth, 1);
        assert_eq!(by_path["one"].depth, 1);
        assert_eq!(by_path["one/child.txt"].depth, 2);
        assert_eq!(by_path["one/two"].depth, 2);
        assert_eq!(by_path["one/two/deep.txt"].depth, 3);
        assert!(entries.iter().all(|entry| {
            std::path::Path::new(entry.relative_path.as_escaped())
                .file_name()
                .is_some_and(|name| name == entry.entry.name.as_escaped())
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

        let entries = walk_dir(
            &HostPath::from_path(fixture.path()),
            CancellationToken::new(),
        )
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
        let error = walk_dir(
            &HostPath::from_path(fixture.path()),
            CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind(), FsErrorKind::NotDirectory);
    }

    #[tokio::test]
    async fn missing_roots_fail_before_returning_a_stream() {
        let error = walk_dir(
            &"/definitely/missing/file-peeker-walk-fixture".into(),
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
        let mut stream = walk_dir(&HostPath::from_path(fixture.path()), cancellation.clone())
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
        let mut stream = walk_dir(
            &HostPath::from_path(fixture.path()),
            CancellationToken::new(),
        )
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
        let service = crate::fs::FsService::new();
        let walk = service
            .walk_dir(&HostPath::from_path(fixture.path()))
            .await
            .unwrap();
        drop(walk);

        assert_eq!(
            service
                .list_dir(&HostPath::from_path(fixture.path()))
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
                .walk_dir(&HostPath::from_path(fixture.path()))
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// A descendant whose name is not UTF-8 used to end the walk on the spot. It is
    /// escaped instead, so the traversal carries on and the entry is still reported
    /// by a relative path that addresses it.
    ///
    /// Skips itself where the filesystem refuses to create such a name at all, which
    /// is what APFS does.
    #[cfg(unix)]
    #[tokio::test]
    async fn non_utf8_descendants_are_escaped_rather_than_terminal() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt as _};

        let fixture = tempfile::tempdir().unwrap();
        let directory = OsString::from_vec(vec![b'd', 0xff]);
        if tokio::fs::create_dir(fixture.path().join(&directory))
            .await
            .is_err()
        {
            return;
        }
        tokio::fs::write(fixture.path().join(&directory).join("child.txt"), b"")
            .await
            .unwrap();

        let entries = walk_dir(
            &HostPath::from_path(fixture.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .expect("an unusual name should no longer end the walk");

        // Both the directory and the child beneath it: the walk descended through a
        // name it cannot spell.
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| entry.relative_path == "d%FF"));
        assert!(
            entries
                .iter()
                .any(|entry| entry.relative_path == "d%FF/child.txt" && entry.depth == 2)
        );
    }
}
