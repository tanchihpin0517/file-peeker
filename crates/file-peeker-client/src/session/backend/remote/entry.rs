use std::io;

use file_peeker_server::protocol::v1::{EntryKind as ProtocolEntryKind, ListingEntry};

use crate::{DirectoryEntry, EntryKind};

pub(super) fn convert_entry(entry: ListingEntry) -> io::Result<DirectoryEntry> {
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
