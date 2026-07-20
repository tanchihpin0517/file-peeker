use std::{
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::Instant,
};

use file_peeker_client::server::Server;
use file_peeker_protocol::{
    ClientMessage, ServerMessage,
    io::{read_message, send_message},
};

use super::connect::connect_local_server;

pub async fn run(path: &str) -> io::Result<()> {
    tracing::debug!("---------------- list ----------------");
    let path = absolute_path(path)?;
    let request_path = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path must be valid UTF-8"))?;

    let mut server = connect_local_server().await?;
    let mut stream = server.operate().await?;
    let started = Instant::now();
    send_message(
        &mut stream,
        &ClientMessage::List {
            path: request_path.to_owned(),
        },
    )
    .await?;

    let mut output = io::stdout().lock();
    let listed = write_listing(&path, &mut stream, &mut output).await;
    drop(stream);
    let shutdown = server.shutdown().await;
    let stats = listed?;
    shutdown?;
    output.flush()?;

    let elapsed = started.elapsed();
    let entries_per_second = if elapsed.is_zero() {
        0
    } else {
        u128::from(stats.entries) * 1_000_000_000 / elapsed.as_nanos()
    };
    tracing::debug!(
        entries = stats.entries,
        batches = stats.batches,
        elapsed_ms = elapsed.as_secs_f64() * 1_000.0,
        entries_per_second = %entries_per_second,
        "list performance"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ListStats {
    entries: u64,
    batches: u64,
}

fn absolute_path(path: &str) -> io::Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

async fn write_listing<R>(
    parent: &Path,
    reader: &mut R,
    output: &mut impl Write,
) -> io::Result<ListStats>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut stats = ListStats {
        entries: 0,
        batches: 0,
    };
    loop {
        match read_message::<ServerMessage, _>(reader).await? {
            ServerMessage::ListBatch { entries } if entries.is_empty() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "server returned an empty listing batch",
                ));
            }
            ServerMessage::ListBatch { entries } => {
                stats.batches += 1;
                stats.entries += entries.len() as u64;
                for entry in entries {
                    let path = child_path(parent, &entry.name)?;
                    writeln!(output, "{}", path.display())?;
                }
            }
            ServerMessage::ListEnd => return Ok(stats),
            ServerMessage::Error { code, message } => {
                return Err(io::Error::other(format!(
                    "server rejected list ({code:?}): {message}"
                )));
            }
            response => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("server returned unexpected list response: {response:?}"),
                ));
            }
        }
    }
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
    use std::path::Path;

    use file_peeker_protocol::{EntryKind, ErrorCode, ListingEntry, ServerMessage};

    use super::{ListStats, absolute_path, child_path, write_listing};

    fn responses(messages: &[ServerMessage]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for message in messages {
            serde_json::to_writer(&mut bytes, message).unwrap();
            bytes.push(b'\n');
        }
        bytes
    }

    #[tokio::test]
    async fn writes_child_paths_from_multiple_batches() {
        let reader = responses(&[
            ServerMessage::ListBatch {
                entries: vec![ListingEntry {
                    name: "notes.txt".into(),
                    kind: EntryKind::File,
                    navigable: false,
                }],
            },
            ServerMessage::ListBatch {
                entries: vec![ListingEntry {
                    name: "docs".into(),
                    kind: EntryKind::Directory,
                    navigable: true,
                }],
            },
            ServerMessage::ListEnd,
        ]);
        let mut output = Vec::new();

        let mut reader = reader.as_slice();
        let stats = write_listing(Path::new("/fixture"), &mut reader, &mut output)
            .await
            .unwrap();

        assert_eq!(output, b"/fixture/notes.txt\n/fixture/docs\n");
        assert_eq!(
            stats,
            ListStats {
                entries: 2,
                batches: 2
            }
        );
    }

    #[tokio::test]
    async fn list_end_without_batches_writes_nothing() {
        let reader = responses(&[ServerMessage::ListEnd]);
        let mut output = Vec::new();

        let mut reader = reader.as_slice();
        let stats = write_listing(Path::new("/fixture"), &mut reader, &mut output)
            .await
            .unwrap();

        assert!(output.is_empty());
        assert_eq!(
            stats,
            ListStats {
                entries: 0,
                batches: 0
            }
        );
    }

    #[tokio::test]
    async fn server_errors_are_returned() {
        let reader = responses(&[ServerMessage::Error {
            code: ErrorCode::NotFound,
            message: "missing".into(),
        }]);
        let mut reader = reader.as_slice();
        let error = write_listing(Path::new("/fixture"), &mut reader, &mut Vec::new())
            .await
            .expect_err("server error should fail the listing");

        assert!(error.to_string().contains("NotFound"));
        assert!(error.to_string().contains("missing"));
    }

    #[tokio::test]
    async fn invalid_listing_responses_are_rejected() {
        let empty_batch = responses(&[ServerMessage::ListBatch {
            entries: Vec::new(),
        }]);
        let mut empty_batch = empty_batch.as_slice();
        assert_eq!(
            write_listing(Path::new("/fixture"), &mut empty_batch, &mut Vec::new())
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );

        let unexpected = responses(&[ServerMessage::HeartbeatOk]);
        let mut unexpected = unexpected.as_slice();
        assert_eq!(
            write_listing(Path::new("/fixture"), &mut unexpected, &mut Vec::new())
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );

        let truncated = br#"{"type":"list_end""#.to_vec();
        let mut truncated = truncated.as_slice();
        assert!(
            write_listing(Path::new("/fixture"), &mut truncated, &mut Vec::new())
                .await
                .is_err()
        );
    }

    #[test]
    fn paths_are_absolute_and_child_names_are_single_components() {
        assert!(absolute_path(".").unwrap().is_absolute());
        assert_eq!(
            child_path(Path::new("/fixture"), "notes.txt").unwrap(),
            Path::new("/fixture/notes.txt")
        );
        assert!(child_path(Path::new("/fixture"), "../escape").is_err());
        assert!(child_path(Path::new("/fixture"), "nested/name").is_err());
    }
}
