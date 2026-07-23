use std::{
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use file_peeker_client::{Client, Session, SessionTarget, WalkStream};
use futures::TryStreamExt as _;

const OUTPUT_FLUSH_INTERVAL: u64 = 1024;

pub async fn run(path: &str, remote: Option<&str>) -> io::Result<()> {
    tracing::debug!("---------------- walk ----------------");
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
    let result = match client.get_session(session_id.clone()).await {
        Some(session) => run_with_session(&session, path).await,
        None => Err(io::Error::other("started Session was not retained")),
    };
    let shutdown = client
        .close_session(session_id)
        .await
        .map_err(|error| io::Error::other(error.to_string()));
    result?;
    shutdown
}

async fn run_with_session(session: &Session, path: &str) -> io::Result<()> {
    let started = Instant::now();
    let mut entries = session.op_walk_dir(path).await?;
    let mut output = io::stdout().lock();
    let stats = write_walk(Path::new(path), &mut entries, &mut output).await?;
    output.flush()?;

    let elapsed = started.elapsed();
    tracing::debug!(
        entries = stats.entries,
        elapsed_ms = elapsed.as_secs_f64() * 1_000.0,
        entries_per_second = %entries_per_second(stats.entries, elapsed),
        "walk performance"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WalkStats {
    entries: u64,
}

async fn write_walk(
    root: &Path,
    entries: &mut WalkStream,
    output: &mut impl Write,
) -> io::Result<WalkStats> {
    let mut stats = WalkStats { entries: 0 };
    while let Some(entry) = entries.try_next().await? {
        let path = descendant_path(root, &entry.relative_path)?;
        writeln!(output, "{}", path.display())?;
        stats.entries += 1;
        if stats.entries.is_multiple_of(OUTPUT_FLUSH_INTERVAL) {
            output.flush()?;
        }
    }
    Ok(stats)
}

fn descendant_path(root: &Path, relative_path: &str) -> io::Result<PathBuf> {
    let relative_path = Path::new(relative_path);
    let valid = !relative_path.as_os_str().is_empty()
        && !relative_path.is_absolute()
        && relative_path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "walk returned an invalid relative path",
        ));
    }
    Ok(root.join(relative_path))
}

fn entries_per_second(entries: u64, elapsed: Duration) -> u128 {
    if elapsed.is_zero() {
        0
    } else {
        u128::from(entries) * 1_000_000_000 / elapsed.as_nanos()
    }
}

#[cfg(test)]
mod tests {
    use std::{io, path::Path, time::Duration};

    use file_peeker_client::{DirectoryEntry, EntryKind, WalkEntry, WalkStream};
    use futures::{StreamExt as _, stream};

    use super::{
        OUTPUT_FLUSH_INTERVAL, WalkStats, descendant_path, entries_per_second, write_walk,
    };

    fn entry(relative_path: &str) -> WalkEntry {
        WalkEntry {
            relative_path: relative_path.into(),
            entry: DirectoryEntry {
                name: relative_path.rsplit('/').next().unwrap().into(),
                kind: EntryKind::File,
                navigable: false,
            },
            depth: relative_path.split('/').count(),
        }
    }

    fn walk(entries: Vec<io::Result<WalkEntry>>) -> WalkStream {
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
    async fn writes_preorder_descendant_paths() {
        let mut entries = walk(vec![
            Ok(entry("docs")),
            Ok(entry("docs/guide.md")),
            Ok(entry("notes.txt")),
        ]);
        let mut output = Vec::new();

        let stats = write_walk(Path::new("/fixture"), &mut entries, &mut output)
            .await
            .unwrap();

        assert_eq!(
            output,
            b"/fixture/docs\n/fixture/docs/guide.md\n/fixture/notes.txt\n"
        );
        assert_eq!(stats, WalkStats { entries: 3 });
    }

    #[tokio::test]
    async fn empty_walk_writes_nothing() {
        let mut entries = walk(Vec::new());
        let mut output = Vec::new();

        let stats = write_walk(Path::new("/fixture"), &mut entries, &mut output)
            .await
            .unwrap();

        assert!(output.is_empty());
        assert_eq!(stats, WalkStats { entries: 0 });
    }

    #[tokio::test]
    async fn flushes_output_at_the_entry_interval() {
        let entries = (0..OUTPUT_FLUSH_INTERVAL)
            .map(|index| Ok(entry(&format!("file-{index}"))))
            .collect();
        let mut entries = walk(entries);
        let mut output = FlushCountingWriter::default();

        let stats = write_walk(Path::new("/fixture"), &mut entries, &mut output)
            .await
            .unwrap();

        assert_eq!(
            stats,
            WalkStats {
                entries: OUTPUT_FLUSH_INTERVAL
            }
        );
        assert_eq!(output.flushes, 1);
        assert!(!output.bytes.is_empty());
    }

    #[tokio::test]
    async fn stream_errors_preserve_prior_output() {
        let mut entries = walk(vec![
            Ok(entry("kept.txt")),
            Err(io::Error::other("walk failed")),
        ]);
        let mut output = Vec::new();

        let error = write_walk(Path::new("/fixture"), &mut entries, &mut output)
            .await
            .expect_err("stream error should fail the walk");

        assert_eq!(error.to_string(), "walk failed");
        assert_eq!(output, b"/fixture/kept.txt\n");
    }

    #[test]
    fn relative_paths_stay_below_the_requested_root() {
        assert_eq!(
            descendant_path(Path::new("~/fixture"), "docs/guide.md").unwrap(),
            Path::new("~/fixture/docs/guide.md")
        );
        for invalid in ["", "/escape", "../escape", "nested/../../escape"] {
            assert_eq!(
                descendant_path(Path::new("/fixture"), invalid)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn performance_rate_handles_zero_and_nonzero_durations() {
        assert_eq!(entries_per_second(10, Duration::ZERO), 0);
        assert_eq!(entries_per_second(6, Duration::from_secs(2)), 3);
    }
}
