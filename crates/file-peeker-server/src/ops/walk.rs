use file_peeker_core::{FsError, WalkEntry as CoreWalkEntry, WalkStream};
use file_peeker_server::protocol::v1::{WalkBatch, WalkEntry};
use futures::{StreamExt as _, stream, stream::BoxStream};
use prost::Message as _;
use tonic::Status;

use super::{
    GRPC_BATCH_MAX_BYTES, GRPC_BATCH_MAX_ENTRIES, entry::convert_entry, status::fs_status,
};

pub(super) fn walk_batches(stream: WalkStream) -> BoxStream<'static, Result<WalkBatch, Status>> {
    stream
        .chunks(GRPC_BATCH_MAX_ENTRIES)
        .flat_map(|entries| stream::iter(convert_chunk(entries)))
        .boxed()
}

fn convert_chunk(entries: Vec<Result<CoreWalkEntry, FsError>>) -> Vec<Result<WalkBatch, Status>> {
    let mut results = Vec::new();
    let mut batch = Vec::new();
    let mut batch_bytes = 0;

    for entry in entries {
        let entry = match entry {
            Ok(entry) => match convert_walk_entry(entry) {
                Ok(entry) => entry,
                Err(error) => {
                    if !batch.is_empty() {
                        results.push(Ok(WalkBatch { entries: batch }));
                    }
                    results.push(Err(error));
                    return results;
                }
            },
            Err(error) => {
                if !batch.is_empty() {
                    results.push(Ok(WalkBatch { entries: batch }));
                }
                results.push(Err(fs_status(&error)));
                return results;
            }
        };
        let entry_bytes = prost::encoding::key_len(1)
            + prost::encoding::encoded_len_varint(entry.encoded_len() as u64)
            + entry.encoded_len();
        if entry_bytes > GRPC_BATCH_MAX_BYTES {
            if !batch.is_empty() {
                results.push(Ok(WalkBatch { entries: batch }));
            }
            results.push(Err(Status::resource_exhausted(
                "walk entry exceeds the gRPC walk batch limit",
            )));
            return results;
        }
        if !batch.is_empty() && batch_bytes + entry_bytes > GRPC_BATCH_MAX_BYTES {
            results.push(Ok(WalkBatch {
                entries: std::mem::take(&mut batch),
            }));
            batch_bytes = 0;
        }
        batch.push(entry);
        batch_bytes += entry_bytes;
    }

    if !batch.is_empty() {
        results.push(Ok(WalkBatch { entries: batch }));
    }
    results
}

fn convert_walk_entry(entry: CoreWalkEntry) -> Result<WalkEntry, Status> {
    Ok(WalkEntry {
        relative_path: entry.relative_path,
        entry: Some(convert_entry(entry.entry)),
        depth: u64::try_from(entry.depth)
            .map_err(|_| Status::out_of_range("walk depth exceeds protocol range"))?,
    })
}

#[cfg(test)]
mod tests {
    use file_peeker_core::{
        DirectoryEntry, EntryKind, FsError, FsErrorKind, WalkEntry as CoreWalkEntry,
    };
    use futures::{StreamExt as _, TryStreamExt as _, stream};
    use prost::Message as _;
    use tonic::Code;

    use super::{GRPC_BATCH_MAX_BYTES, GRPC_BATCH_MAX_ENTRIES, convert_chunk, walk_batches};

    fn entry(relative_path: String, depth: usize) -> CoreWalkEntry {
        CoreWalkEntry {
            entry: DirectoryEntry {
                name: relative_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&relative_path)
                    .into(),
                kind: EntryKind::File,
                navigable: false,
            },
            relative_path,
            depth,
        }
    }

    #[test]
    fn converts_paths_entries_and_depths() {
        let batches = convert_chunk(vec![Ok(entry("nested/file.txt".into(), 2))]);
        let converted = &batches[0].as_ref().unwrap().entries[0];
        assert_eq!(converted.relative_path, "nested/file.txt");
        assert_eq!(converted.depth, 2);
        assert_eq!(converted.entry.as_ref().unwrap().name, "file.txt");
    }

    #[tokio::test]
    async fn splits_batches_at_the_entry_count_limit() {
        let entries = (0..=GRPC_BATCH_MAX_ENTRIES).map(|index| Ok(entry(index.to_string(), 1)));
        let batches = walk_batches(stream::iter(entries).boxed())
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].entries.len(), GRPC_BATCH_MAX_ENTRIES);
        assert_eq!(batches[1].entries.len(), 1);
    }

    #[test]
    fn splits_batches_by_encoded_size_without_reordering() {
        let entries = (0..3)
            .map(|index| {
                Ok(entry(
                    format!("{index}-{}", "x".repeat(GRPC_BATCH_MAX_BYTES / 3)),
                    1,
                ))
            })
            .collect();
        let batches = convert_chunk(entries)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(batches.len(), 3);
        assert!(batch_encoded_sizes_fit(&batches));
        assert_eq!(
            batches
                .iter()
                .flat_map(|batch| batch.entries.iter())
                .map(|entry| &entry.relative_path[..1])
                .collect::<Vec<_>>(),
            ["0", "1", "2"]
        );
    }

    fn batch_encoded_sizes_fit(batches: &[file_peeker_server::protocol::v1::WalkBatch]) -> bool {
        batches
            .iter()
            .all(|batch| batch.encoded_len() <= GRPC_BATCH_MAX_BYTES)
    }

    #[test]
    fn rejects_oversized_entries_and_emits_no_empty_batches() {
        let error = convert_chunk(vec![Ok(entry("x".repeat(GRPC_BATCH_MAX_BYTES), 1))])
            .pop()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code(), Code::ResourceExhausted);
        assert!(convert_chunk(Vec::new()).is_empty());
    }

    #[test]
    fn flushes_entries_before_terminal_core_errors() {
        let results = convert_chunk(vec![
            Ok(entry("kept".into(), 1)),
            Err(FsError::new(FsErrorKind::PermissionDenied, "denied")),
        ]);
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].as_ref().unwrap().entries[0].relative_path,
            "kept"
        );
        assert_eq!(
            results[1].as_ref().unwrap_err().code(),
            Code::PermissionDenied
        );
    }
}
