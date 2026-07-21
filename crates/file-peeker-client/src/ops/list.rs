use std::io;

use file_peeker_protocol::v1::{
    EntryKind as ProtocolEntryKind, ListRequest, ListingEntry, file_peeker_client::FilePeekerClient,
};
use futures::{StreamExt, stream::BoxStream};
use tonic::{Request, transport::Channel};

use crate::connection::status_error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: EntryKind,
    pub navigable: bool,
}

pub type ListStream = BoxStream<'static, io::Result<Vec<DirectoryEntry>>>;

pub(crate) async fn list(
    channel: Channel,
    request: Request<ListRequest>,
) -> io::Result<ListStream> {
    let stream = FilePeekerClient::new(channel)
        .list(request)
        .await
        .map_err(status_error)?
        .into_inner()
        .map(|result| {
            let batch = result.map_err(status_error)?;
            convert_batch(batch.entries)
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
    entries.into_iter().map(DirectoryEntry::try_from).collect()
}

impl TryFrom<ListingEntry> for DirectoryEntry {
    type Error = io::Error;

    fn try_from(entry: ListingEntry) -> Result<Self, Self::Error> {
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
        Ok(Self {
            name: entry.name,
            kind,
            navigable: entry.navigable,
        })
    }
}

#[cfg(test)]
mod tests {
    use file_peeker_protocol::v1::{EntryKind as ProtocolEntryKind, ListingEntry};

    use super::{DirectoryEntry, EntryKind, convert_batch};

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
            ]
        );
    }

    #[test]
    fn rejects_empty_batches_and_unspecified_kinds() {
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
    }
}
