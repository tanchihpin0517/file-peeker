use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use crate::{
    Client, ConnectionId, EntryStream, ServerLocation, cli::write_path, host_path::HostPath,
};
use futures::TryStreamExt as _;

const OUTPUT_FLUSH_INTERVAL: u64 = 1024;

pub(crate) async fn run(path: &str, remote: Option<&str>) -> io::Result<()> {
    tracing::debug!("---------------- list ----------------");
    let location = match remote {
        Some(destination) => ServerLocation::Ssh {
            destination: destination.to_owned(),
            force_install: false,
        },
        None => ServerLocation::Embedded,
    };
    let client = Client::new();
    let connection = client.open(location).await?;
    let result = run_with_connection(&client, &connection, path).await;
    let shutdown = client.close_all().await;
    result?;
    shutdown
}

async fn run_with_connection(
    client: &Client,
    connection: &ConnectionId,
    path: &str,
) -> io::Result<()> {
    let started = Instant::now();
    let mut entries = client
        .op_list_dir(connection, &HostPath::from_literal(path))
        .await?;
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
        write_path(output, &path)?;
        stats.entries += 1;
        if stats.entries.is_multiple_of(OUTPUT_FLUSH_INTERVAL) {
            output.flush()?;
        }
    }
    Ok(stats)
}

/// Where a listed child sits, checking the server sent a single component and not a
/// path. Checked before decoding, which is sound: the escape leaves `.` and `..`
/// alone and cannot introduce a separator.
fn child_path(parent: &Path, name: &HostPath) -> io::Result<PathBuf> {
    if !name.is_one_component() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "server returned an invalid child name: {:?}",
                name.as_escaped()
            ),
        ));
    }
    let name = name
        .to_path_buf()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    Ok(parent.join(name))
}

#[cfg(test)]
mod tests {
    use std::{io, path::Path};

    use crate::{DirectoryEntry, EntryKind, EntryStream};
    use futures::{StreamExt as _, stream};

    use super::{ListStats, OUTPUT_FLUSH_INTERVAL, child_path, write_listing};

    fn entry(name: &str) -> DirectoryEntry {
        DirectoryEntry::readable(name.into(), EntryKind::File, false)
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

        assert!(output.is_empty(), "{output:?}");
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
        assert!(!output.bytes.is_empty(), "nothing was written");
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
            child_path(Path::new("/fixture"), &"notes.txt".into()).unwrap(),
            Path::new("/fixture/notes.txt")
        );
        assert!(child_path(Path::new("/fixture"), &"../escape".into()).is_err());
        assert!(child_path(Path::new("/fixture"), &"nested/name".into()).is_err());
    }
}
