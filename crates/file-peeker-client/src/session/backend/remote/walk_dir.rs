use std::{
    collections::VecDeque,
    io,
    path::{Component, Path},
    pin::Pin,
};

use file_peeker_server::protocol::v1::{
    WalkBatch, WalkEntry as ProtocolWalkEntry, WalkRequest, file_peeker_client::FilePeekerClient,
};
use futures::{Stream, StreamExt as _, stream};

use super::{entry::convert_entry, error::operation_status_error};
use crate::{WalkEntry, WalkStream, session::backend::connection::RemoteConnection};

pub(super) async fn walk_dir(connection: &RemoteConnection, path: &str) -> io::Result<WalkStream> {
    let channel = connection.channel()?;
    let request = connection.request(WalkRequest {
        path: path.to_owned(),
    })?;
    let stream = FilePeekerClient::new(channel)
        .walk(request)
        .await
        .map_err(|status| operation_status_error(&status))?
        .into_inner();
    Ok(network_walk_stream(stream))
}

struct NetworkWalkState<S> {
    stream: Pin<Box<S>>,
    pending: VecDeque<WalkEntry>,
}

fn network_walk_stream<S>(stream: S) -> WalkStream
where
    S: Stream<Item = Result<WalkBatch, tonic::Status>> + Send + 'static,
{
    stream::unfold(
        Some(NetworkWalkState {
            stream: Box::pin(stream),
            pending: VecDeque::new(),
        }),
        |state| async move {
            let mut state = state?;
            loop {
                if let Some(entry) = state.pending.pop_front() {
                    return Some((Ok(entry), Some(state)));
                }
                match state.stream.next().await {
                    Some(Ok(batch)) => match convert_batch(batch.entries) {
                        Ok(entries) => state.pending = entries.into(),
                        Err(error) => return Some((Err(error), None)),
                    },
                    Some(Err(status)) => {
                        return Some((Err(operation_status_error(&status)), None));
                    }
                    None => return None,
                }
            }
        },
    )
    .boxed()
}

fn convert_batch(entries: Vec<ProtocolWalkEntry>) -> io::Result<Vec<WalkEntry>> {
    if entries.is_empty() {
        return Err(invalid_data("server returned an empty walk batch"));
    }
    entries.into_iter().map(convert_walk_entry).collect()
}

fn convert_walk_entry(entry: ProtocolWalkEntry) -> io::Result<WalkEntry> {
    validate_relative_path(&entry.relative_path)?;
    if entry.depth == 0 {
        return Err(invalid_data("server returned walk depth 0"));
    }
    let depth = usize::try_from(entry.depth)
        .map_err(|_| invalid_data("server returned walk depth outside the client range"))?;
    let nested = entry
        .entry
        .ok_or_else(|| invalid_data("server returned a walk entry without listing metadata"))?;
    Ok(WalkEntry {
        relative_path: entry.relative_path,
        entry: convert_entry(nested)?,
        depth,
    })
}

fn validate_relative_path(path: &str) -> io::Result<()> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(invalid_data(
            "server returned an invalid walk relative path",
        ));
    }
    Ok(())
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use file_peeker_server::protocol::v1::{
        EntryKind as ProtocolEntryKind, ListingEntry, WalkBatch, WalkEntry as ProtocolWalkEntry,
    };
    use futures::{StreamExt as _, stream};
    use tonic::Status;

    use super::network_walk_stream;

    fn entry(path: &str, depth: u64) -> ProtocolWalkEntry {
        ProtocolWalkEntry {
            relative_path: path.into(),
            entry: Some(ListingEntry {
                name: path.rsplit('/').next().unwrap().into(),
                kind: ProtocolEntryKind::File.into(),
                navigable: false,
            }),
            depth,
        }
    }

    #[tokio::test]
    async fn preserves_order_across_batches() {
        let mut stream = network_walk_stream(stream::iter([
            Ok(WalkBatch {
                entries: vec![entry("one", 1), entry("dir/two", 2)],
            }),
            Ok(WalkBatch {
                entries: vec![entry("three", 1)],
            }),
        ]));
        let mut paths = Vec::new();
        while let Some(item) = stream.next().await {
            paths.push(item.unwrap().relative_path);
        }
        assert_eq!(paths, ["one", "dir/two", "three"]);
    }

    #[tokio::test]
    async fn rejects_malformed_batches_terminally() {
        let invalid_entries = [
            Vec::new(),
            vec![entry("", 1)],
            vec![entry("/absolute", 1)],
            vec![entry("../parent", 2)],
            vec![entry("zero", 0)],
            vec![ProtocolWalkEntry {
                relative_path: "missing".into(),
                entry: None,
                depth: 1,
            }],
            vec![ProtocolWalkEntry {
                relative_path: "unknown".into(),
                entry: Some(ListingEntry {
                    name: "unknown".into(),
                    kind: i32::MAX,
                    navigable: false,
                }),
                depth: 1,
            }],
        ];
        for entries in invalid_entries {
            let mut stream = network_walk_stream(stream::iter([
                Ok(WalkBatch { entries }),
                Ok(WalkBatch {
                    entries: vec![entry("discarded", 1)],
                }),
            ]));
            assert_eq!(
                stream.next().await.unwrap().unwrap_err().kind(),
                std::io::ErrorKind::InvalidData
            );
            assert!(stream.next().await.is_none());
        }
    }

    #[tokio::test]
    async fn preserves_prior_entries_before_transport_errors() {
        let mut stream = network_walk_stream(stream::iter([
            Ok(WalkBatch {
                entries: vec![entry("kept", 1)],
            }),
            Err(Status::unavailable("lost")),
            Ok(WalkBatch {
                entries: vec![entry("discarded", 1)],
            }),
        ]));
        assert_eq!(stream.next().await.unwrap().unwrap().relative_path, "kept");
        assert!(stream.next().await.unwrap().is_err());
        assert!(stream.next().await.is_none());
    }
}
