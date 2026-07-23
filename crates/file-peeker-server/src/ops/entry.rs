use file_peeker_core::{DirectoryEntry, EntryKind};
use file_peeker_server::protocol::v1::{EntryKind as ProtocolEntryKind, ListingEntry};

pub(super) fn convert_entry(entry: DirectoryEntry) -> ListingEntry {
    let kind = match entry.kind {
        EntryKind::File => ProtocolEntryKind::File,
        EntryKind::Directory => ProtocolEntryKind::Directory,
        EntryKind::Symlink => ProtocolEntryKind::Symlink,
        EntryKind::Other => ProtocolEntryKind::Other,
    };
    ListingEntry {
        name: entry.name,
        kind: kind.into(),
        navigable: entry.navigable,
    }
}
