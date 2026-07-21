//! Streaming directory-listing operation.

use file_peeker_protocol::{ErrorCode, ListingEntry, ServerMessage};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{Duration, Instant, timeout_at},
};

use super::write_error;
use crate::{ServerError, utils::resolve_path, write_server_message};

const BATCH_TARGET_BYTES: usize = 128 * 1024;
const BATCH_MAX_ENTRIES: usize = 512;
const BATCH_MAX_DELAY: Duration = Duration::from_millis(25);

pub(super) async fn handle<S>(stream: &mut S, path: &str) -> Result<(), ServerError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let path = match resolve_path(path) {
        Ok(path) => path,
        Err(message) => return write_error(stream, ErrorCode::InvalidPath, message).await,
    };

    let mut directory_entries = match tokio::fs::read_dir(&path).await {
        Ok(entries) => entries,
        Err(error) => return write_io_error(stream, error).await,
    };
    let mut batch = Vec::new();
    let mut batch_bytes: usize = 0;
    let mut deadline = None;

    loop {
        // Read the next directory entry while respecting the current batch's
        // delivery deadline.
        let next = if let Some(batch_deadline) = deadline {
            // Keep waiting only until the oldest entry in this batch has waited
            // long enough.
            if let Ok(result) = timeout_at(batch_deadline, directory_entries.next_entry()).await {
                result
            } else {
                // The deadline expired before another entry arrived. Deliver the
                // partial batch now so a slow directory still updates the client.
                flush_batch(stream, &mut batch).await?;
                batch_bytes = 0;
                deadline = None;
                continue;
            }
        } else {
            // No batch is buffered yet, so there is no delivery deadline to meet.
            directory_entries.next_entry().await
        };

        // Separate a usable entry, normal end-of-directory, and a read failure.
        let entry = match next {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(error) => {
                // Entries already delivered by the filesystem remain useful even
                // when walking the rest of the directory fails.
                flush_batch(stream, &mut batch).await?;
                return write_io_error(stream, error).await;
            }
        };

        // Read the entry metadata needed by the protocol. Metadata lookup shares
        // the same deadline as reading entries so it cannot stall a ready batch.
        let converted = if let Some(batch_deadline) = deadline {
            if let Ok(result) = timeout_at(batch_deadline, convert_entry(&entry)).await {
                result
            } else {
                // Send older entries first, then finish converting this entry for
                // the next batch without carrying over an expired deadline.
                flush_batch(stream, &mut batch).await?;
                batch_bytes = 0;
                deadline = None;
                convert_entry(&entry).await
            }
        } else {
            convert_entry(&entry).await
        };

        // Turn conversion failures into a terminal protocol error after preserving
        // any entries that were already converted successfully.
        let listing_entry = match converted {
            Ok(entry) => entry,
            Err(failure) => {
                // Preserve the successfully converted prefix before terminating
                // the stream with an explicit error.
                flush_batch(stream, &mut batch).await?;
                return write_error(stream, failure.code, &failure.message).await;
            }
        };

        // Measure the encoded entry before adding it so the byte threshold tracks
        // transport size instead of an in-memory Rust representation.
        let entry_bytes = serde_json::to_vec(&listing_entry)
            .map_err(|error| ServerError::Protocol {
                message: format!("cannot encode listing entry: {error}"),
            })?
            .len();

        // The size and count limits bound transport overhead and frame size. The
        // deadline bounds time-to-first-result when entries arrive slowly.
        if !batch.is_empty() && batch_bytes.saturating_add(entry_bytes + 1) > BATCH_TARGET_BYTES {
            // Keep the new entry for the next batch rather than exceeding the
            // target size of the current batch.
            flush_batch(stream, &mut batch).await?;
            batch_bytes = 0;
            deadline = None;
        }

        if batch.is_empty() {
            // Start one deadline for the whole batch. Resetting it per entry could
            // postpone delivery forever on a continuously busy directory.
            deadline = Some(Instant::now() + BATCH_MAX_DELAY);
        }

        // Account for the entry and its JSON separator, then retain it until one
        // of the batch limits requests a flush.
        batch_bytes = batch_bytes.saturating_add(entry_bytes + usize::from(!batch.is_empty()));
        batch.push(listing_entry);

        // Bound memory and per-message parsing work even when entries are tiny.
        if batch.len() == BATCH_MAX_ENTRIES {
            flush_batch(stream, &mut batch).await?;
            batch_bytes = 0;
            deadline = None;
        }
    }

    // Deliver the final partial batch before marking the stream complete.
    flush_batch(stream, &mut batch).await?;
    // An explicit terminator distinguishes a complete listing from a dropped
    // operation connection.
    write_server_message(stream, &ServerMessage::ListEnd).await
}

async fn flush_batch<S>(stream: &mut S, entries: &mut Vec<ListingEntry>) -> Result<(), ServerError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if entries.is_empty() {
        return Ok(());
    }
    let entries = std::mem::take(entries);
    write_server_message(stream, &ServerMessage::ListBatch { entries }).await
}

struct ListingFailure {
    code: ErrorCode,
    message: String,
}

async fn convert_entry(entry: &tokio::fs::DirEntry) -> Result<ListingEntry, ListingFailure> {
    let entry_path = entry.path();
    // Protocol paths and names are JSON strings. Report unrepresentable names
    // instead of silently replacing bytes and pointing the client at another path.
    if entry_path.to_str().is_none() {
        return Err(ListingFailure {
            code: ErrorCode::InvalidPath,
            message: "Encountered a non-UTF-8 path".into(),
        });
    }
    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
        return Err(ListingFailure {
            code: ErrorCode::InvalidPath,
            message: "Encountered a non-UTF-8 filename".into(),
        });
    };
    let file_type = entry
        .file_type()
        .await
        .map_err(|error| listing_io_failure(&error))?;
    let (kind, navigable) = if file_type.is_dir() {
        (file_peeker_protocol::EntryKind::Directory, true)
    } else if file_type.is_file() {
        (file_peeker_protocol::EntryKind::File, false)
    } else if file_type.is_symlink() {
        // A broken, inaccessible, or non-directory symlink is still displayed,
        // but following it cannot produce another directory listing.
        (
            file_peeker_protocol::EntryKind::Symlink,
            tokio::fs::metadata(&entry_path)
                .await
                .is_ok_and(|metadata| metadata.is_dir()),
        )
    } else {
        (file_peeker_protocol::EntryKind::Other, false)
    };

    Ok(ListingEntry {
        name,
        kind,
        navigable,
    })
}

fn listing_io_failure(error: &std::io::Error) -> ListingFailure {
    ListingFailure {
        code: io_error_code(error),
        message: error.to_string(),
    }
}

async fn write_io_error<S>(stream: &mut S, error: std::io::Error) -> Result<(), ServerError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let code = io_error_code(&error);
    write_error(stream, code, &error.to_string()).await
}

fn io_error_code(error: &std::io::Error) -> ErrorCode {
    match error.kind() {
        std::io::ErrorKind::NotFound => ErrorCode::NotFound,
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        std::io::ErrorKind::NotADirectory => ErrorCode::NotDirectory,
        _ => ErrorCode::Io,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use file_peeker_protocol::{ErrorCode, ServerMessage};
    use tokio::io::{AsyncReadExt, duplex};

    use super::handle;

    #[tokio::test]
    async fn listing_sends_a_batch_and_end_marker() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("notes.txt"), "hello").unwrap();
        std::fs::create_dir(directory.path().join("docs")).unwrap();
        let listing_path = directory.path().to_string_lossy().into_owned();
        let (mut server_stream, mut client_stream) = duplex(1024 * 1024);

        let server = tokio::spawn(async move { handle(&mut server_stream, &listing_path).await });
        let mut response = String::new();
        client_stream.read_to_string(&mut response).await.unwrap();
        server.await.unwrap().unwrap();

        let messages: Vec<ServerMessage> = response
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(messages.len(), 2);
        let ServerMessage::ListBatch { entries } = &messages[0] else {
            panic!("listing should begin with a list_batch message");
        };
        let names: HashSet<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, HashSet::from(["docs", "notes.txt"]));
        assert_eq!(messages[1], ServerMessage::ListEnd);
    }

    #[tokio::test]
    async fn listing_error_sends_only_an_error_result() {
        let directory = tempfile::tempdir().unwrap();
        let listing_path = directory
            .path()
            .join("missing")
            .to_string_lossy()
            .into_owned();
        let (mut server_stream, mut client_stream) = duplex(1024 * 1024);

        let server = tokio::spawn(async move { handle(&mut server_stream, &listing_path).await });
        let mut response = String::new();
        client_stream.read_to_string(&mut response).await.unwrap();
        server.await.unwrap().unwrap();

        assert_eq!(response.lines().count(), 1);
        let message: ServerMessage = serde_json::from_str(response.trim_end()).unwrap();
        assert!(matches!(
            message,
            ServerMessage::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn listing_splits_more_than_the_entry_cap_into_multiple_batches() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..513 {
            std::fs::write(directory.path().join(format!("entry-{index:04}")), "").unwrap();
        }
        let listing_path = directory.path().to_string_lossy().into_owned();
        let (mut server_stream, mut client_stream) = duplex(1024 * 1024);

        let server = tokio::spawn(async move { handle(&mut server_stream, &listing_path).await });
        let mut response = String::new();
        client_stream.read_to_string(&mut response).await.unwrap();
        server.await.unwrap().unwrap();

        let messages: Vec<ServerMessage> = response
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(messages.last(), Some(&ServerMessage::ListEnd));
        let batches: Vec<_> = messages
            .iter()
            .filter_map(|message| match message {
                ServerMessage::ListBatch { entries } => Some(entries),
                _ => None,
            })
            .collect();
        assert!(batches.len() >= 2);
        assert_eq!(
            batches.iter().map(|entries| entries.len()).sum::<usize>(),
            513
        );
        assert!(batches.iter().all(|entries| entries.len() <= 512));
    }
}
