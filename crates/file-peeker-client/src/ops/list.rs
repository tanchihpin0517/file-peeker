use std::io;

use file_peeker_protocol::v1::{ListRequest, ListingEntry, file_peeker_client::FilePeekerClient};
use futures::{
    StreamExt, TryStreamExt,
    stream::{self, BoxStream},
};
use tonic::{Request, transport::Channel};

use crate::connection::status_error;

pub type ListStream = BoxStream<'static, io::Result<ListingEntry>>;
pub(crate) type ListBatchStream = BoxStream<'static, io::Result<Vec<ListingEntry>>>;

pub(crate) async fn list_batches(
    channel: Channel,
    request: Request<ListRequest>,
) -> io::Result<ListBatchStream> {
    let stream = FilePeekerClient::new(channel)
        .list(request)
        .await
        .map_err(status_error)?
        .into_inner()
        .map(|result| {
            let batch = result.map_err(status_error)?;
            if batch.entries.is_empty() {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "server returned an empty listing batch",
                ))
            } else {
                Ok(batch.entries)
            }
        })
        .boxed();
    Ok(stream)
}

pub(crate) fn flatten_batches(batches: ListBatchStream) -> ListStream {
    batches
        .map_ok(|entries| stream::iter(entries.into_iter().map(Ok)))
        .try_flatten()
        .boxed()
}

#[cfg(test)]
mod tests {
    use std::io;

    use file_peeker_protocol::v1::{EntryKind, ListingEntry};
    use futures::{StreamExt, TryStreamExt, stream};

    use super::flatten_batches;

    fn entry(name: &str) -> ListingEntry {
        ListingEntry {
            name: name.into(),
            kind: EntryKind::File.into(),
            navigable: false,
        }
    }

    #[tokio::test]
    async fn flattens_batches_in_order() {
        let batches = stream::iter([
            Ok(vec![entry("one"), entry("two")]),
            Ok(vec![entry("three")]),
        ])
        .boxed();
        let names = flatten_batches(batches)
            .map_ok(|entry| entry.name)
            .try_collect::<Vec<_>>()
            .await
            .unwrap();

        assert_eq!(names, ["one", "two", "three"]);
    }

    #[tokio::test]
    async fn preserves_terminal_errors_after_entries() {
        let batches = stream::iter([
            Ok(vec![entry("one")]),
            Err(io::Error::new(io::ErrorKind::ConnectionAborted, "closed")),
        ])
        .boxed();
        let mut entries = flatten_batches(batches);

        assert_eq!(entries.next().await.unwrap().unwrap().name, "one");
        assert_eq!(
            entries.next().await.unwrap().unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
    }
}
