use std::{io, time::Duration};

use file_peeker_protocol::v1::{EntryKind, ListBatch, ListingEntry};
use futures::{StreamExt, stream::BoxStream};
use prost::Message;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::Status;

use crate::utils::resolve_path;

const BATCH_TARGET_BYTES: usize = 1024 * 1024;
const BATCH_MAX_ENTRIES: usize = 1024;
const BATCH_MAX_DELAY: Duration = Duration::from_millis(25);
const BATCH_CHANNEL_CAPACITY: usize = 2;

pub(super) async fn list(
    path: String,
    cancellation: CancellationToken,
) -> Result<BoxStream<'static, Result<ListBatch, Status>>, Status> {
    let path = resolve_path(&path).map_err(Status::invalid_argument)?;
    let directory_entries = tokio::task::spawn_blocking(move || std::fs::read_dir(path))
        .await
        .map_err(|error| Status::internal(format!("directory worker failed: {error}")))?
        .map_err(|error| io_status(&error))?;
    let (sender, receiver) = mpsc::channel(BATCH_CHANNEL_CAPACITY);
    let runtime = tokio::runtime::Handle::current();

    tokio::task::spawn_blocking(move || {
        produce_batches(directory_entries, &cancellation, &sender, &runtime);
    });

    Ok(ReceiverStream::new(receiver).boxed())
}

fn produce_batches(
    directory_entries: std::fs::ReadDir,
    cancellation: &CancellationToken,
    sender: &mpsc::Sender<Result<ListBatch, Status>>,
    runtime: &tokio::runtime::Handle,
) {
    let mut batch = Vec::new();
    let mut batch_bytes = 0_usize;
    let mut batch_started = None;

    for result in directory_entries {
        if cancellation.is_cancelled() {
            return;
        }

        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                if !send_batch(sender, &mut batch, cancellation, runtime) {
                    return;
                }
                let _ = send_result(sender, Err(io_status(&error)), cancellation, runtime);
                return;
            }
        };
        let listing_entry = match convert_entry(&entry) {
            Ok(entry) => entry,
            Err(status) => {
                if !send_batch(sender, &mut batch, cancellation, runtime) {
                    return;
                }
                let _ = send_result(sender, Err(status), cancellation, runtime);
                return;
            }
        };
        let entry_bytes = repeated_message_bytes(&listing_entry);

        if !batch.is_empty() && batch_bytes.saturating_add(entry_bytes) > BATCH_TARGET_BYTES {
            if !send_batch(sender, &mut batch, cancellation, runtime) {
                return;
            }
            batch_bytes = 0;
            batch_started = None;
        }
        if batch.is_empty() {
            batch_started = Some(std::time::Instant::now());
        }
        batch_bytes = batch_bytes.saturating_add(entry_bytes);
        batch.push(listing_entry);

        let deadline_reached =
            batch_started.is_some_and(|started| started.elapsed() >= BATCH_MAX_DELAY);
        if batch.len() == BATCH_MAX_ENTRIES || deadline_reached {
            if !send_batch(sender, &mut batch, cancellation, runtime) {
                return;
            }
            batch_bytes = 0;
            batch_started = None;
        }
    }

    let _ = send_batch(sender, &mut batch, cancellation, runtime);
}

fn send_batch(
    sender: &mpsc::Sender<Result<ListBatch, Status>>,
    batch: &mut Vec<ListingEntry>,
    cancellation: &CancellationToken,
    runtime: &tokio::runtime::Handle,
) -> bool {
    if batch.is_empty() {
        return true;
    }
    send_result(
        sender,
        Ok(ListBatch {
            entries: std::mem::take(batch),
        }),
        cancellation,
        runtime,
    )
}

fn send_result(
    sender: &mpsc::Sender<Result<ListBatch, Status>>,
    result: Result<ListBatch, Status>,
    cancellation: &CancellationToken,
    runtime: &tokio::runtime::Handle,
) -> bool {
    runtime.block_on(async {
        tokio::select! {
            () = cancellation.cancelled() => false,
            result = sender.send(result) => result.is_ok(),
        }
    })
}

fn repeated_message_bytes(entry: &ListingEntry) -> usize {
    let encoded = entry.encoded_len();
    1 + varint_len(encoded) + encoded
}

fn varint_len(mut value: usize) -> usize {
    let mut bytes = 1;
    while value >= 0x80 {
        value >>= 7;
        bytes += 1;
    }
    bytes
}

fn convert_entry(entry: &std::fs::DirEntry) -> Result<ListingEntry, Status> {
    let entry_path = entry.path();
    if entry_path.to_str().is_none() {
        return Err(Status::invalid_argument("Encountered a non-UTF-8 path"));
    }
    let name = entry
        .file_name()
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| Status::invalid_argument("Encountered a non-UTF-8 filename"))?;
    let file_type = entry.file_type().map_err(|error| io_status(&error))?;
    let (kind, navigable) = if file_type.is_dir() {
        (EntryKind::Directory, true)
    } else if file_type.is_file() {
        (EntryKind::File, false)
    } else if file_type.is_symlink() {
        (
            EntryKind::Symlink,
            std::fs::metadata(&entry_path).is_ok_and(|metadata| metadata.is_dir()),
        )
    } else {
        (EntryKind::Other, false)
    };

    Ok(ListingEntry {
        name,
        kind: kind.into(),
        navigable,
    })
}

fn io_status(error: &io::Error) -> Status {
    match error.kind() {
        io::ErrorKind::NotFound => Status::not_found(error.to_string()),
        io::ErrorKind::PermissionDenied => Status::permission_denied(error.to_string()),
        io::ErrorKind::NotADirectory => Status::failed_precondition(error.to_string()),
        _ => Status::internal(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::TryStreamExt;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::{BATCH_MAX_ENTRIES, list, produce_batches};

    #[tokio::test]
    async fn lists_a_directory_in_non_empty_bounded_batches() {
        let fixture = tempfile::tempdir().unwrap();
        for index in 0..=BATCH_MAX_ENTRIES {
            tokio::fs::write(fixture.path().join(format!("file-{index}")), b"")
                .await
                .unwrap();
        }

        let batches = list(
            fixture.path().to_string_lossy().into_owned(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();

        assert_eq!(
            batches
                .iter()
                .map(|batch| batch.entries.len())
                .sum::<usize>(),
            BATCH_MAX_ENTRIES + 1
        );
        assert!(batches.iter().all(|batch| !batch.entries.is_empty()));
        assert!(
            batches
                .iter()
                .all(|batch| batch.entries.len() <= BATCH_MAX_ENTRIES)
        );
    }

    #[tokio::test]
    async fn empty_directory_completes_without_batches() {
        let fixture = tempfile::tempdir().unwrap();
        let batches = list(
            fixture.path().to_string_lossy().into_owned(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();

        assert!(batches.is_empty());
    }

    #[tokio::test]
    async fn cancellation_unblocks_a_backpressured_directory_worker() {
        let fixture = tempfile::tempdir().unwrap();
        for index in 0..=(BATCH_MAX_ENTRIES * 3) {
            std::fs::write(fixture.path().join(format!("file-{index}")), b"").unwrap();
        }
        let directory_entries = std::fs::read_dir(fixture.path()).unwrap();
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (sender, _receiver) = mpsc::channel(1);
        let runtime = tokio::runtime::Handle::current();
        let worker = tokio::task::spawn_blocking(move || {
            produce_batches(directory_entries, &worker_cancellation, &sender, &runtime);
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        cancellation.cancel();

        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .expect("cancelled worker should not remain blocked")
            .unwrap();
    }
}
