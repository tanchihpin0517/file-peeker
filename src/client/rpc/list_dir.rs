use std::io;

use crate::host_path::HostPath;
use crate::protocol::v1::{ListRequest, ListingEntry, file_peeker_client::FilePeekerClient};
use futures::{StreamExt as _, stream};

use super::{entry::convert_entry, error::operation_status_error};
use crate::{DirectoryEntry, EntryStream, client::connection::ServerConnection};

pub(crate) async fn list_dir(
    connection: &ServerConnection,
    path: &HostPath,
) -> io::Result<EntryStream> {
    let channel = connection.channel();
    let request = connection.request(ListRequest {
        path: path.as_escaped().to_owned(),
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

#[cfg(test)]
mod tests {
    use crate::protocol::v1::{EntryKind as ProtocolEntryKind, ListingEntry};

    use super::convert_batch;
    use crate::{DirectoryEntry, EntryKind};

    fn entry(name: &str, kind: ProtocolEntryKind) -> ListingEntry {
        ListingEntry {
            name: name.into(),
            kind: kind.into(),
            navigable: kind == ProtocolEntryKind::Directory,
            defect: String::new(),
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
                DirectoryEntry::readable("one".into(), EntryKind::File, false),
                DirectoryEntry::readable("two".into(), EntryKind::Directory, true),
                DirectoryEntry::readable("three".into(), EntryKind::Symlink, false),
                DirectoryEntry::readable("four".into(), EntryKind::Other, false),
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
                defect: String::new(),
            }])
            .unwrap_err()
            .kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}
