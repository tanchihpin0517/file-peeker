use file_peeker_core::{DirectoryEntry, EntryStream, FsError};
use file_peeker_server::protocol::v1::ListBatch;
use futures::{StreamExt as _, stream, stream::BoxStream};
use prost::Message as _;
use tonic::Status;

use super::{
    GRPC_BATCH_MAX_BYTES, GRPC_BATCH_MAX_ENTRIES, entry::convert_entry, status::fs_status,
};

pub(super) fn list_batches(stream: EntryStream) -> BoxStream<'static, Result<ListBatch, Status>> {
    stream
        .chunks(GRPC_BATCH_MAX_ENTRIES)
        .flat_map(|entries| stream::iter(convert_chunk(entries)))
        .boxed()
}

fn convert_chunk(entries: Vec<Result<DirectoryEntry, FsError>>) -> Vec<Result<ListBatch, Status>> {
    let mut results = Vec::new();
    let mut batch = Vec::new();
    let mut batch_bytes = 0;

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                if !batch.is_empty() {
                    results.push(Ok(ListBatch { entries: batch }));
                }
                results.push(Err(fs_status(&error)));
                return results;
            }
        };
        let entry = convert_entry(entry);
        let entry_bytes = prost::encoding::key_len(1)
            + prost::encoding::encoded_len_varint(entry.encoded_len() as u64)
            + entry.encoded_len();
        if entry_bytes > GRPC_BATCH_MAX_BYTES {
            if !batch.is_empty() {
                results.push(Ok(ListBatch { entries: batch }));
            }
            results.push(Err(Status::resource_exhausted(
                "directory entry exceeds the gRPC listing batch limit",
            )));
            return results;
        }
        if !batch.is_empty() && batch_bytes + entry_bytes > GRPC_BATCH_MAX_BYTES {
            results.push(Ok(ListBatch {
                entries: std::mem::take(&mut batch),
            }));
            batch_bytes = 0;
        }
        batch.push(entry);
        batch_bytes += entry_bytes;
    }

    if !batch.is_empty() {
        results.push(Ok(ListBatch { entries: batch }));
    }

    results
}

#[cfg(test)]
mod tests {
    use file_peeker_core::{DirectoryEntry, EntryKind, FsError, FsErrorKind};
    use prost::Message as _;
    use tonic::Code;

    use super::{GRPC_BATCH_MAX_BYTES, convert_chunk};

    #[test]
    fn keeps_small_chunks_together() {
        let batches = convert_chunk(vec![Ok(DirectoryEntry {
            name: "reports".into(),
            kind: EntryKind::Directory,
            navigable: true,
        })]);
        assert_eq!(batches.len(), 1);
        let batch = batches[0].as_ref().unwrap();
        assert_eq!(batch.entries.len(), 1);
        assert_eq!(batch.entries[0].name, "reports");
        assert!(batch.entries[0].navigable);
    }

    #[test]
    fn splits_large_chunks_without_reordering() {
        let entries = (0..3)
            .map(|index| DirectoryEntry {
                name: format!("{index}-{}", "x".repeat(GRPC_BATCH_MAX_BYTES / 2)),
                kind: EntryKind::File,
                navigable: false,
            })
            .map(Ok)
            .collect();

        let batches = convert_chunk(entries)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(batches.len(), 3);
        assert!(
            batches
                .iter()
                .all(|batch| batch.encoded_len() <= GRPC_BATCH_MAX_BYTES)
        );
        assert_eq!(
            batches
                .iter()
                .flat_map(|batch| batch.entries.iter())
                .map(|entry| &entry.name[..1])
                .collect::<Vec<_>>(),
            ["0", "1", "2"]
        );
    }

    #[test]
    fn rejects_single_entries_larger_than_the_grpc_limit() {
        let error = convert_chunk(vec![Ok(DirectoryEntry {
            name: "x".repeat(GRPC_BATCH_MAX_BYTES),
            kind: EntryKind::File,
            navigable: false,
        })])
        .pop()
        .unwrap()
        .unwrap_err();

        assert_eq!(error.code(), Code::ResourceExhausted);
    }

    #[test]
    fn empty_chunks_emit_no_grpc_messages() {
        assert!(convert_chunk(Vec::new()).is_empty());
    }

    #[test]
    fn flushes_entries_before_a_terminal_core_error() {
        let results = convert_chunk(vec![
            Ok(DirectoryEntry {
                name: "kept".into(),
                kind: EntryKind::File,
                navigable: false,
            }),
            Err(FsError::new(FsErrorKind::PermissionDenied, "denied")),
        ]);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].as_ref().unwrap().entries[0].name, "kept");
        assert_eq!(
            results[1].as_ref().unwrap_err().code(),
            Code::PermissionDenied
        );
    }
}
