use std::io;

use file_peeker_server::protocol::v1::{
    EntryKind as ProtocolEntryKind, ListRequest, ListingEntry, file_peeker_client::FilePeekerClient,
};
use futures::{StreamExt as _, stream};

use super::error::operation_status_error;
use crate::{
    DirectoryEntry, EntryKind, EntryStream, session::backend::connection::RemoteConnection,
};

pub(super) async fn list_dir(connection: &RemoteConnection, path: &str) -> io::Result<EntryStream> {
    let channel = connection.channel()?;
    let request = connection.request(ListRequest {
        path: path.to_owned(),
    })?;

    let stream: EntryStream = FilePeekerClient::new(channel)
        .list(request)
        .await
        .map_err(|status| operation_status_error(&status))?
        .into_inner()
        .flat_map(|result| {
            let entries = result
                .map_err(|status| operation_status_error(&status))
                .and_then(|batch| convert_batch(batch.entries));
            let entries = match entries {
                Ok(entries) => entries.into_iter().map(Ok).collect(),
                Err(error) => vec![Err(error)],
            };
            stream::iter(entries)
        })
        .boxed();
    Ok(stream)
}

fn convert_batch(entries: Vec<ListingEntry>) -> io::Result<Vec<DirectoryEntry>> {
    if entries.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "server returned an empty listing batch",
        ));
    }
    entries.into_iter().map(convert_entry).collect()
}

fn convert_entry(entry: ListingEntry) -> io::Result<DirectoryEntry> {
    let kind = ProtocolEntryKind::try_from(entry.kind).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("server returned unknown entry kind {}", entry.kind),
        )
    })?;
    let kind = match kind {
        ProtocolEntryKind::Unspecified => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "server returned an unspecified entry kind",
            ));
        }
        ProtocolEntryKind::File => EntryKind::File,
        ProtocolEntryKind::Directory => EntryKind::Directory,
        ProtocolEntryKind::Symlink => EntryKind::Symlink,
        ProtocolEntryKind::Other => EntryKind::Other,
    };
    Ok(DirectoryEntry {
        name: entry.name,
        kind,
        navigable: entry.navigable,
    })
}

#[cfg(test)]
mod tests {
    use file_peeker_server::protocol::v1::{EntryKind as ProtocolEntryKind, ListingEntry};

    use super::convert_batch;
    use crate::{DirectoryEntry, EntryKind};

    fn entry(name: &str, kind: ProtocolEntryKind) -> ListingEntry {
        ListingEntry {
            name: name.into(),
            kind: kind.into(),
            navigable: kind == ProtocolEntryKind::Directory,
        }
    }

    #[test]
    fn converts_complete_batches_in_order() {
        assert_eq!(
            convert_batch(vec![
                entry("one", ProtocolEntryKind::File),
                entry("two", ProtocolEntryKind::Directory),
                entry("three", ProtocolEntryKind::Symlink),
                entry("four", ProtocolEntryKind::Other),
            ])
            .unwrap(),
            [
                DirectoryEntry {
                    name: "one".into(),
                    kind: EntryKind::File,
                    navigable: false,
                },
                DirectoryEntry {
                    name: "two".into(),
                    kind: EntryKind::Directory,
                    navigable: true,
                },
                DirectoryEntry {
                    name: "three".into(),
                    kind: EntryKind::Symlink,
                    navigable: false,
                },
                DirectoryEntry {
                    name: "four".into(),
                    kind: EntryKind::Other,
                    navigable: false,
                },
            ]
        );
    }

    #[test]
    fn rejects_empty_batches_and_invalid_kinds() {
        assert_eq!(
            convert_batch(vec![]).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(
            convert_batch(vec![entry("unknown", ProtocolEntryKind::Unspecified)])
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(
            convert_batch(vec![ListingEntry {
                name: "unknown".into(),
                kind: i32::MAX,
                navigable: false,
            }])
            .unwrap_err()
            .kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}
