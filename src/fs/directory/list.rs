use futures::{StreamExt, stream, stream::BoxStream};
use tokio::fs;
use tokio_util::sync::CancellationToken;

use super::{DirectoryEntry, entry::inspect_entry};
use crate::{
    fs::{FsError, error::service_cancelled_error, resolve_path::resolve_path},
    host_path::HostPath,
};

pub(crate) type EntryStream = BoxStream<'static, Result<DirectoryEntry, FsError>>;

pub(crate) async fn list_dir(
    path: &HostPath,
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
    use crate::{
        fs::{EntryKind, FsErrorKind},
        host_path::HostPath,
    };

    #[tokio::test]
    async fn lists_entry_kinds_and_directory_symlinks() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("file"), b"").unwrap();
        std::fs::create_dir(fixture.path().join("directory")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("directory", fixture.path().join("link")).unwrap();

        let entries = list_dir(
            &HostPath::from_path(fixture.path()),
            CancellationToken::new(),
        )
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

        let entries = list_dir(
            &HostPath::from_path(fixture.path()),
            CancellationToken::new(),
        )
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
        let entries = list_dir(
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
    async fn rejects_file_targets_before_returning_a_stream() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("file.txt");
        tokio::fs::write(&path, b"payload").await.unwrap();

        let error = list_dir(&HostPath::from_path(&path), CancellationToken::new())
            .await
            .err()
            .expect("file target should fail");

        assert_eq!(error.kind(), FsErrorKind::NotDirectory);
        assert!(error.message().contains(&path.display().to_string()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn broken_symlinks_are_non_navigable_entries() {
        let fixture = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("missing", fixture.path().join("broken")).unwrap();

        let entries = list_dir(
            &HostPath::from_path(fixture.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, EntryKind::Symlink);
        assert!(!entries[0].navigable);
    }

    /// A name that is not UTF-8 used to end the listing on its first entry, which
    /// meant one unusual filename hid every sibling. It is escaped instead, so the
    /// rest of the directory still arrives.
    ///
    /// Skips itself where the filesystem refuses to create such a name at all, which
    /// is what APFS does — so on macOS only the escape's own tests cover this.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_utf8_name_is_escaped_and_hides_no_siblings() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt as _};

        let fixture = tempfile::tempdir().unwrap();
        let unusual = OsString::from_vec(vec![b'f', 0x80]);
        if tokio::fs::write(fixture.path().join(&unusual), b"payload")
            .await
            .is_err()
        {
            return;
        }
        for sibling in ["a.txt", "z.txt"] {
            tokio::fs::write(fixture.path().join(sibling), b"")
                .await
                .unwrap();
        }

        let entries = list_dir(
            &HostPath::from_path(fixture.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .expect("an unusual name should no longer end the listing");

        let mut names: Vec<_> = entries
            .iter()
            .map(|entry| entry.name.as_escaped())
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["a.txt", "f%80", "z.txt"]);
    }

    /// A literal marker is the other half of what makes the escape invertible, and
    /// unlike a name that is not UTF-8 every filesystem will hold one — so this is
    /// what covers the round trip on macOS, where APFS refuses the other case.
    ///
    /// `100%.txt` is reported as `100%%.txt`, reads back as one marker, and still
    /// opens the file. Escaping only the invalid bytes would have made this name
    /// collide with the one holding byte `0x0B`.
    #[tokio::test]
    async fn a_literal_marker_round_trips_and_still_addresses_its_file() {
        use crate::fs::read::read_file;

        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::write(fixture.path().join("100%.txt"), b"payload")
            .await
            .unwrap();

        let entries = list_dir(
            &HostPath::from_path(fixture.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
        let [entry] = entries.as_slice() else {
            panic!("one entry expected");
        };

        assert_eq!(entry.name.as_escaped(), "100%%.txt");
        assert_eq!(entry.name.display(), "100%.txt");

        let child = HostPath::from_path(fixture.path()).join(&entry.name);
        let contents = read_file(&child, CancellationToken::new())
            .await
            .expect("the reported name should open the file it names")
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .concat();

        assert_eq!(contents, b"payload");
    }

    /// The reason the escape is invertible rather than lossy: the name a listing
    /// reports still addresses the file. A replacement character would not.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_utf8_name_still_addresses_its_file() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt as _};

        use crate::fs::read::read_file;

        let fixture = tempfile::tempdir().unwrap();
        let unusual = OsString::from_vec(vec![b'f', 0x80]);
        if tokio::fs::write(fixture.path().join(&unusual), b"payload")
            .await
            .is_err()
        {
            return;
        }

        let entries = list_dir(
            &HostPath::from_path(fixture.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
        let [entry] = entries.as_slice() else {
            panic!("one entry expected");
        };

        // Straight back out of the listing and into a read, with no decoding by hand.
        let child = HostPath::from_path(fixture.path()).join(&entry.name);
        let contents = read_file(&child, CancellationToken::new())
            .await
            .expect("the reported name should open the file it names")
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .concat();

        assert_eq!(contents, b"payload");
        assert_eq!(entry.name.display(), "f\u{fffd}");
    }

    #[tokio::test]
    async fn service_cancellation_errors_a_partially_consumed_stream() {
        let fixture = tempfile::tempdir().unwrap();
        for index in 0..=2048 {
            std::fs::write(fixture.path().join(format!("file-{index}")), b"").unwrap();
        }
        let cancellation_token = CancellationToken::new();
        let mut stream = list_dir(
            &HostPath::from_path(fixture.path()),
            cancellation_token.clone(),
        )
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
