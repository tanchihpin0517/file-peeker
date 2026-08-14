use std::io;

use crate::protocol::v1::{EntryKind as ProtocolEntryKind, ListingEntry};

use crate::{DirectoryEntry, EntryKind, host_path::HostPath};

/// Takes a selected-host name or path off the wire.
///
/// Names cross as an escaped form, so one that does not decode means the server is
/// not speaking this protocol — the same class of fault as an unknown entry kind, and
/// reported the same way.
pub(super) fn wire_path(escaped: String, what: &str) -> io::Result<HostPath> {
    HostPath::from_escaped(escaped).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("server returned an incorrectly escaped {what}: {error}"),
        )
    })
}

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
        name: wire_path(entry.name, "entry name")?,
        kind,
        navigable: entry.navigable,
    })
}
