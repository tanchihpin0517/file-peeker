//! Each operation against a real temporary directory.

use std::{
    collections::HashMap,
    ffi::OsString,
    fs::Permissions,
    os::unix::{ffi::OsStringExt as _, fs::PermissionsExt as _},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use futures_util::{StreamExt as _, TryStreamExt as _, stream};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::{
    EntryKind, FsError, FsErrorKind, ReadStream, WriteConflict, list_dir, read_file, walk_dir,
    write_file,
};
use crate::path_string::PathString;

fn at(path: &Path) -> PathString {
    PathString::from_path(path)
}

async fn list(path: &Path) -> Vec<super::DirectoryEntry> {
    list_dir(&at(path))
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap()
}

async fn read(path: &PathString) -> Vec<u8> {
    read_file(path)
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap()
        .concat()
}

/// Creates a file whose name is `name`'s bytes, or `None` where the filesystem
/// refuses names outside UTF-8, as APFS does — so on macOS only the escape's own
/// tests cover them.
fn unusual_file(directory: &Path, name: &[u8]) -> Option<PathBuf> {
    let path = directory.join(OsString::from_vec(name.to_vec()));
    std::fs::write(&path, b"payload").ok().map(|()| path)
}

// Listing.

#[tokio::test]
async fn lists_each_kind_and_a_link_to_a_directory() {
    let fixture = TempDir::new().unwrap();
    std::fs::write(fixture.path().join("file"), b"").unwrap();
    std::fs::create_dir(fixture.path().join("directory")).unwrap();
    std::os::unix::fs::symlink("directory", fixture.path().join("link")).unwrap();

    let entries = list(fixture.path()).await;
    let kind = |name: &str| {
        let entry = entries.iter().find(|entry| entry.name == name).unwrap();
        (entry.kind, entry.navigable)
    };
    assert_eq!(kind("file"), (EntryKind::File, false));
    assert_eq!(kind("directory"), (EntryKind::Directory, true));
    assert_eq!(kind("link"), (EntryKind::Symlink, true));
}

#[tokio::test]
async fn lists_every_entry_once() {
    let fixture = TempDir::new().unwrap();
    for index in 0..=1024 {
        std::fs::write(fixture.path().join(format!("file-{index}")), b"").unwrap();
    }
    assert_eq!(list(fixture.path()).await.len(), 1025);
    let empty = TempDir::new().unwrap();
    assert!(list(empty.path()).await.is_empty());
}

#[tokio::test]
async fn listing_a_file_fails_before_the_stream() {
    let fixture = TempDir::new().unwrap();
    let path = fixture.path().join("file.txt");
    std::fs::write(&path, b"payload").unwrap();
    let error = list_dir(&at(&path)).await.err().unwrap();
    assert_eq!(error.kind(), FsErrorKind::NotDirectory);
    assert!(
        error.message().contains(&path.display().to_string()),
        "{error}"
    );
}

#[tokio::test]
async fn a_relative_path_is_refused_before_anything_is_touched() {
    for path in ["notes", "."] {
        let path = PathString::from_literal(path);
        assert_eq!(
            list_dir(&path).await.err().unwrap().kind(),
            FsErrorKind::InvalidArgument
        );
        assert_eq!(
            read_file(&path).await.err().unwrap().kind(),
            FsErrorKind::InvalidArgument
        );
    }
}

#[tokio::test]
async fn an_entry_reports_its_size_time_and_mode() {
    let fixture = TempDir::new().unwrap();
    let path = fixture.path().join("payload.bin");
    std::fs::write(&path, b"seven..").unwrap();
    std::fs::set_permissions(&path, Permissions::from_mode(0o640)).unwrap();
    let before = SystemTime::now() - Duration::from_secs(60);

    let entries = list(fixture.path()).await;
    let [entry] = entries.as_slice() else {
        panic!("one entry expected: {entries:?}");
    };
    let metadata = entry.metadata.as_ref().unwrap();
    assert_eq!(metadata.size, 7);
    assert!(metadata.modified.unwrap() >= before);
    assert_eq!(metadata.mode & 0o777, 0o640);
    assert_eq!(metadata.symlink_target, None);
    assert!(!entry.is_degraded());
}

/// A symlink describes itself, as `ls -l` does, and names its target. A broken
/// one still has metadata, which tells it apart from an entry that could not be
/// stat'd at all.
#[tokio::test]
async fn a_symlink_describes_itself_and_names_its_target() {
    let fixture = TempDir::new().unwrap();
    std::fs::create_dir(fixture.path().join("target-directory")).unwrap();
    std::os::unix::fs::symlink("target-directory", fixture.path().join("to-directory")).unwrap();
    std::os::unix::fs::symlink("nowhere", fixture.path().join("broken")).unwrap();

    let entries = list(fixture.path()).await;
    let by_name = |name: &str| entries.iter().find(|entry| entry.name == name).unwrap();

    let link = by_name("to-directory");
    let metadata = link.metadata.as_ref().unwrap();
    assert_eq!(
        metadata.symlink_target.as_ref().map(PathString::as_str),
        Some("target-directory")
    );
    // Its own size, the length of what it stores, not the directory's.
    assert_eq!(metadata.size, "target-directory".len() as u64);
    assert!(link.navigable);

    let broken = by_name("broken");
    assert!(!broken.is_degraded());
    assert!(!broken.navigable);
    assert_eq!(
        broken
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.symlink_target.as_ref())
            .map(PathString::as_str),
        Some("nowhere")
    );
}

/// A directory that is readable but not searchable lists its entries and then
/// refuses to stat any of them. Each entry comes back degraded, and the listing
/// completes.
#[tokio::test]
async fn an_entry_whose_stat_is_refused_does_not_end_the_listing() {
    let fixture = TempDir::new().unwrap();
    let refused = fixture.path().join("refused");
    std::fs::create_dir(&refused).unwrap();
    for name in ["one.txt", "two.txt"] {
        std::fs::write(refused.join(name), b"payload").unwrap();
    }
    std::fs::set_permissions(&refused, Permissions::from_mode(0o444)).unwrap();
    // A privileged process ignores the missing bit, and there is nothing to see.
    let privileged = std::fs::metadata(refused.join("one.txt")).is_ok();
    let listed = list_dir(&at(&refused))
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await;
    std::fs::set_permissions(&refused, Permissions::from_mode(0o755)).unwrap();
    if privileged {
        return;
    }

    let entries = listed.expect("a refused stat ends nothing");
    let mut names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["one.txt", "two.txt"]);
    assert!(
        entries
            .iter()
            .all(|entry| entry.is_degraded() && entry.defect.is_some() && !entry.navigable)
    );
}

#[tokio::test]
async fn a_name_outside_utf8_is_escaped_and_still_opens_its_file() {
    let fixture = TempDir::new().unwrap();
    if unusual_file(fixture.path(), b"f\x80").is_none() {
        return;
    }
    std::fs::write(fixture.path().join("a.txt"), b"").unwrap();

    let entries = list(fixture.path()).await;
    let mut names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["a.txt", "f%80"]);

    let unusual = entries.iter().find(|entry| entry.name == "f%80").unwrap();
    assert_eq!(unusual.name.display(), "f\u{fffd}");
    // Straight from the listing into a read, decoded by nobody.
    let child = at(fixture.path()).join(&unusual.name);
    assert_eq!(read(&child).await, b"payload");
}

/// What covers the round trip where the filesystem refuses the other case: every
/// filesystem holds a literal marker.
#[tokio::test]
async fn a_literal_marker_round_trips_and_still_opens_its_file() {
    let fixture = TempDir::new().unwrap();
    std::fs::write(fixture.path().join("100%.txt"), b"payload").unwrap();

    let entries = list(fixture.path()).await;
    let [entry] = entries.as_slice() else {
        panic!("one entry expected: {entries:?}");
    };
    assert_eq!(entry.name.as_str(), "100%%.txt");
    assert_eq!(entry.name.display(), "100%.txt");
    assert_eq!(
        read(&at(fixture.path()).join(&entry.name)).await,
        b"payload"
    );
}

#[tokio::test]
async fn a_tilde_lists_home() {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let listed = list_dir(&PathString::from_literal("~")).await;
    // Only that it resolved to the same place; what is in a home is not ours.
    assert_eq!(
        listed.is_ok(),
        std::fs::read_dir(&home).is_ok(),
        "{:?}",
        listed.err()
    );
}

// Walking.

#[tokio::test]
async fn walks_depth_first_with_paths_from_the_root() {
    let fixture = TempDir::new().unwrap();
    std::fs::create_dir_all(fixture.path().join("one/two")).unwrap();
    for file in ["root.txt", "one/child.txt", "one/two/deep.txt"] {
        std::fs::write(fixture.path().join(file), b"").unwrap();
    }

    let entries: Vec<_> = walk_dir(&at(fixture.path()))
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let depths: HashMap<_, _> = entries
        .iter()
        .map(|entry| (entry.relative_path.as_str(), entry.depth))
        .collect();
    assert_eq!(
        depths,
        HashMap::from([
            ("root.txt", 1),
            ("one", 1),
            ("one/child.txt", 2),
            ("one/two", 2),
            ("one/two/deep.txt", 3),
        ])
    );
    let position = |path: &str| {
        entries
            .iter()
            .position(|entry| entry.relative_path == path)
            .unwrap()
    };
    assert!(position("one") < position("one/child.txt"));
    assert!(position("one") < position("one/two"));
    assert!(position("one/two") < position("one/two/deep.txt"));
}

#[tokio::test]
async fn a_walk_emits_a_link_to_a_directory_without_following_it() {
    let fixture = TempDir::new().unwrap();
    std::fs::create_dir(fixture.path().join("target")).unwrap();
    std::fs::write(fixture.path().join("target/inside.txt"), b"").unwrap();
    std::os::unix::fs::symlink("target", fixture.path().join("linked")).unwrap();

    let entries: Vec<_> = walk_dir(&at(fixture.path()))
        .await
        .unwrap()
        .try_collect()
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
async fn a_walk_root_must_be_a_directory_that_exists() {
    let fixture = TempDir::new().unwrap();
    let file = fixture.path().join("file");
    std::fs::write(&file, b"").unwrap();
    assert_eq!(
        walk_dir(&at(&file)).await.err().unwrap().kind(),
        FsErrorKind::NotDirectory
    );
    assert_eq!(
        walk_dir(&at(&fixture.path().join("missing")))
            .await
            .err()
            .unwrap()
            .kind(),
        FsErrorKind::NotFound
    );
}

/// A directory that cannot be opened ends the walk after the entry naming it.
#[tokio::test]
async fn a_descendant_that_fails_to_open_follows_its_entry() {
    let fixture = TempDir::new().unwrap();
    let child = fixture.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let mut walk = walk_dir(&at(fixture.path())).await.unwrap();
    assert_eq!(walk.next().await.unwrap().unwrap().relative_path, "child");
    std::fs::remove_dir(&child).unwrap();

    assert_eq!(
        walk.next().await.unwrap().unwrap_err().kind(),
        FsErrorKind::NotFound
    );
    assert!(walk.next().await.is_none());
}

#[tokio::test]
async fn a_walk_goes_through_a_directory_named_outside_utf8() {
    let fixture = TempDir::new().unwrap();
    let directory = fixture.path().join(OsString::from_vec(b"d\xff".to_vec()));
    if std::fs::create_dir(&directory).is_err() {
        return;
    }
    std::fs::write(directory.join("child.txt"), b"").unwrap();

    let entries: Vec<_> = walk_dir(&at(fixture.path()))
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let paths: Vec<_> = entries
        .iter()
        .map(|entry| (entry.relative_path.as_str(), entry.depth))
        .collect();
    assert_eq!(paths, [("d%FF", 1), ("d%FF/child.txt", 2)]);
}

// Reading.

#[tokio::test]
async fn reads_a_file_in_non_empty_chunks() {
    let fixture = TempDir::new().unwrap();
    let path = fixture.path().join("payload.bin");
    let expected: Vec<u8> = (0..128 * 1024 + 17)
        .map(|index: usize| u8::try_from(index % 251).unwrap())
        .collect();
    std::fs::write(&path, &expected).unwrap();

    let chunks: Vec<_> = read_file(&at(&path))
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert!(chunks.len() > 1);
    assert!(chunks.iter().all(|chunk| !chunk.is_empty()));
    assert_eq!(chunks.concat(), expected);

    let empty = fixture.path().join("empty");
    std::fs::write(&empty, b"").unwrap();
    assert!(read(&at(&empty)).await.is_empty());
}

#[tokio::test]
async fn only_a_regular_file_that_exists_is_read() {
    let fixture = TempDir::new().unwrap();
    let error = read_file(&at(fixture.path())).await.err().unwrap();
    assert_eq!(error.kind(), FsErrorKind::NotFile);
    assert!(
        error
            .message()
            .contains(&fixture.path().display().to_string())
    );
    assert_eq!(
        read_file(&at(&fixture.path().join("missing")))
            .await
            .err()
            .unwrap()
            .kind(),
        FsErrorKind::NotFound
    );
}

// Writing.

fn source(chunks: Vec<Result<&'static [u8], FsError>>) -> ReadStream {
    stream::iter(
        chunks
            .into_iter()
            .map(|chunk| chunk.map(Bytes::from_static)),
    )
    .boxed()
}

/// A source that sends what the test sends, and waits for more until dropped.
fn channel_source() -> (mpsc::Sender<Result<Bytes, FsError>>, ReadStream) {
    let (sender, receiver) = mpsc::channel(1);
    (sender, ReceiverStream::new(receiver).boxed())
}

fn partials(directory: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.to_string_lossy().ends_with(".partial"))
        .collect()
}

#[tokio::test]
async fn writes_an_empty_file_and_one_of_several_chunks() {
    let fixture = TempDir::new().unwrap();
    for (name, chunks, expected) in [
        ("empty", vec![], b"".as_slice()),
        ("multi", vec![Ok(b"one".as_slice()), Ok(b"two")], b"onetwo"),
    ] {
        let written = write_file(
            &at(fixture.path()),
            &name.into(),
            WriteConflict::FailIfExists,
            source(chunks),
        )
        .await
        .unwrap();
        assert_eq!(written.bytes, expected.len() as u64);
        assert_eq!(written.path, at(&fixture.path().join(name)));
        assert_eq!(read(&written.path).await, expected);
    }
    assert!(partials(fixture.path()).is_empty());
}

#[tokio::test]
async fn a_name_must_be_one_name() {
    let fixture = TempDir::new().unwrap();
    for name in ["", ".", "..", "/absolute", "nested/name"] {
        let error = write_file(
            &at(fixture.path()),
            &name.into(),
            WriteConflict::Replace,
            source(vec![]),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), FsErrorKind::InvalidArgument, "{name:?}");
    }
}

#[tokio::test]
async fn an_existing_file_stays_unless_replace_is_asked_for() {
    let fixture = TempDir::new().unwrap();
    let destination = fixture.path().join("target");
    std::fs::write(&destination, b"old").unwrap();
    let directory = at(fixture.path());
    let name = PathString::from_literal("target");
    let write = |conflict| write_file(&directory, &name, conflict, source(vec![Ok(b"new")]));

    let error = write(WriteConflict::FailIfExists).await.unwrap_err();
    assert_eq!(error.kind(), FsErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&destination).unwrap(), b"old");

    write(WriteConflict::Replace).await.unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), b"new");
    assert!(partials(fixture.path()).is_empty());
}

#[tokio::test]
async fn a_failing_source_publishes_nothing() {
    let fixture = TempDir::new().unwrap();
    let error = write_file(
        &at(fixture.path()),
        &"target".into(),
        WriteConflict::FailIfExists,
        source(vec![
            Ok(b"prefix"),
            Err(FsError::new(FsErrorKind::Internal, "source failed")),
        ]),
    )
    .await
    .unwrap_err();
    assert_eq!(error.message(), "source failed");
    assert!(!fixture.path().join("target").exists());
    assert!(partials(fixture.path()).is_empty());
}

#[tokio::test]
async fn a_file_that_appears_mid_write_is_not_overwritten() {
    let fixture = TempDir::new().unwrap();
    let (sender, source) = channel_source();
    sender
        .send(Ok(Bytes::from_static(b"prefix")))
        .await
        .unwrap();
    let directory = at(fixture.path());
    let write = tokio::spawn(async move {
        write_file(
            &directory,
            &"target".into(),
            WriteConflict::FailIfExists,
            source,
        )
        .await
    });
    while partials(fixture.path()).is_empty() {
        tokio::task::yield_now().await;
    }
    std::fs::write(fixture.path().join("target"), b"winner").unwrap();
    drop(sender);

    let error = write.await.unwrap().unwrap_err();
    assert_eq!(error.kind(), FsErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(fixture.path().join("target")).unwrap(),
        b"winner"
    );
    assert!(partials(fixture.path()).is_empty());
}

/// Cancelling is dropping. The partial file is gone the moment the write is,
/// with nothing left to run afterwards.
#[tokio::test]
async fn dropping_a_write_removes_its_partial_file_at_once() {
    let fixture = TempDir::new().unwrap();
    let (sender, source) = channel_source();
    sender
        .send(Ok(Bytes::from_static(b"prefix")))
        .await
        .unwrap();
    let directory = at(fixture.path());
    let name = PathString::from_literal("target");
    let mut write = Box::pin(write_file(
        &directory,
        &name,
        WriteConflict::Replace,
        source,
    ));
    // Under way: the prefix is written and it waits for more.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut write)
            .await
            .is_err()
    );
    assert_eq!(partials(fixture.path()).len(), 1);

    drop(write);
    assert!(partials(fixture.path()).is_empty());
    assert!(!fixture.path().join("target").exists());
    drop(sender);
}

/// A listing or a walk dropped part-way leaves nothing behind for the next one.
#[tokio::test]
async fn dropping_a_stream_part_way_leaves_the_next_operation_whole() {
    let fixture = TempDir::new().unwrap();
    for index in 0..10 {
        std::fs::write(fixture.path().join(format!("file-{index}")), b"").unwrap();
    }
    let mut listing = list_dir(&at(fixture.path())).await.unwrap();
    listing.next().await.unwrap().unwrap();
    drop(listing);
    let mut walk = walk_dir(&at(fixture.path())).await.unwrap();
    walk.next().await.unwrap().unwrap();
    drop(walk);
    assert_eq!(list(fixture.path()).await.len(), 10);
}
