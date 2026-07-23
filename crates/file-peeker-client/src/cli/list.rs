use std::{
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::Instant,
};

use file_peeker_client::{Client, EntryStream, Session, SessionTarget};
use futures::TryStreamExt as _;

const OUTPUT_FLUSH_INTERVAL: u64 = 1024;

pub async fn run(path: &str, remote: Option<&str>) -> io::Result<()> {
    tracing::debug!("---------------- list ----------------");
    let target = match remote {
        Some(destination) => SessionTarget::Remote {
            destination: destination.to_owned(),
        },
        None => SessionTarget::Local,
    };
    let client = Client::new();
    let session_id = client
        .start_session(target)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    let session = client
        .get_session(session_id.clone())
        .await
        .ok_or_else(|| io::Error::other("started Session was not retained"))?;
    let result = run_with_session(&session, path).await;
    let shutdown = client
        .close_session(session_id)
        .await
        .map_err(|error| io::Error::other(error.to_string()));
    result?;
    shutdown
}

async fn run_with_session(session: &Session, path: &str) -> io::Result<()> {
    let started = Instant::now();
    let mut entries = session.op_list_dir(path).await?;
    let mut output = io::stdout().lock();
    let stats = write_listing(Path::new(path), &mut entries, &mut output).await?;
    output.flush()?;

    let elapsed = started.elapsed();
    let entries_per_second = if elapsed.is_zero() {
        0
    } else {
        u128::from(stats.entries) * 1_000_000_000 / elapsed.as_nanos()
    };
    tracing::debug!(
        entries = stats.entries,
        elapsed_ms = elapsed.as_secs_f64() * 1_000.0,
        entries_per_second = %entries_per_second,
        "list performance"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ListStats {
    entries: u64,
}

async fn write_listing(
    parent: &Path,
    entries: &mut EntryStream,
    output: &mut impl Write,
) -> io::Result<ListStats> {
    let mut stats = ListStats { entries: 0 };
    while let Some(entry) = entries.try_next().await? {
        let path = child_path(parent, &entry.name)?;
        writeln!(output, "{}", path.display())?;
        stats.entries += 1;
        if stats.entries.is_multiple_of(OUTPUT_FLUSH_INTERVAL) {
            output.flush()?;
        }
    }
    Ok(stats)
}

fn child_path(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let mut components = Path::new(name).components();
    let valid =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("server returned an invalid child name: {name:?}"),
        ));
    }
    Ok(parent.join(name))
}

#[cfg(test)]
mod tests {
    use std::{io, path::Path};

    use file_peeker_client::{DirectoryEntry, EntryKind, EntryStream};
    use futures::{StreamExt as _, stream};

    use super::{ListStats, OUTPUT_FLUSH_INTERVAL, child_path, write_listing};

    fn entry(name: &str) -> DirectoryEntry {
        DirectoryEntry {
            name: name.into(),
            kind: EntryKind::File,
            navigable: false,
        }
    }

    fn listing(entries: Vec<io::Result<DirectoryEntry>>) -> EntryStream {
        stream::iter(entries).boxed()
    }

    #[derive(Default)]
    struct FlushCountingWriter {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl io::Write for FlushCountingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[tokio::test]
    async fn writes_child_paths_from_the_listing_stream() {
        let mut entries = listing(vec![Ok(entry("notes.txt")), Ok(entry("docs"))]);
        let mut output = Vec::new();

        let stats = write_listing(Path::new("/fixture"), &mut entries, &mut output)
            .await
            .unwrap();

        assert_eq!(output, b"/fixture/notes.txt\n/fixture/docs\n");
        assert_eq!(stats, ListStats { entries: 2 });
    }

    #[tokio::test]
    async fn empty_listing_writes_nothing() {
        let mut entries = listing(Vec::new());
        let mut output = Vec::new();

        let stats = write_listing(Path::new("/fixture"), &mut entries, &mut output)
            .await
            .unwrap();

        assert!(output.is_empty());
        assert_eq!(stats, ListStats { entries: 0 });
    }

    #[tokio::test]
    async fn flushes_output_at_the_entry_interval() {
        let entries = (0..OUTPUT_FLUSH_INTERVAL)
            .map(|index| entry(&format!("file-{index}")))
            .map(Ok)
            .collect();
        let mut entries = listing(entries);
        let mut output = FlushCountingWriter::default();

        let stats = write_listing(Path::new("/fixture"), &mut entries, &mut output)
            .await
            .unwrap();

        assert_eq!(
            stats,
            ListStats {
                entries: OUTPUT_FLUSH_INTERVAL
            }
        );
        assert_eq!(output.flushes, 1);
        assert!(!output.bytes.is_empty());
    }

    #[tokio::test]
    async fn listing_stream_errors_are_returned() {
        let mut entries = listing(vec![Err(io::Error::other("listing failed"))]);

        let error = write_listing(Path::new("/fixture"), &mut entries, &mut Vec::new())
            .await
            .expect_err("stream error should fail the listing");

        assert_eq!(error.to_string(), "listing failed");
    }

    #[tokio::test]
    async fn invalid_child_names_are_rejected() {
        let mut entries = listing(vec![Ok(entry("../escape"))]);

        let error = write_listing(Path::new("/fixture"), &mut entries, &mut Vec::new())
            .await
            .expect_err("invalid child name should fail the listing");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn child_names_are_single_components() {
        assert_eq!(
            child_path(Path::new("/fixture"), "notes.txt").unwrap(),
            Path::new("/fixture/notes.txt")
        );
        assert!(child_path(Path::new("/fixture"), "../escape").is_err());
        assert!(child_path(Path::new("/fixture"), "nested/name").is_err());
    }
}
