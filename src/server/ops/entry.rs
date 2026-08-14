use crate::fs::{DirectoryEntry, EntryKind};
use crate::protocol::v1::{EntryKind as ProtocolEntryKind, ListingEntry};

pub(crate) fn convert_entry(entry: DirectoryEntry) -> ListingEntry {
    let kind = match entry.kind {
        EntryKind::File => ProtocolEntryKind::File,
        EntryKind::Directory => ProtocolEntryKind::Directory,
        EntryKind::Symlink => ProtocolEntryKind::Symlink,
        EntryKind::Other => ProtocolEntryKind::Other,
    };
    ListingEntry {
        name: entry.name.into_escaped(),
        kind: kind.into(),
        navigable: entry.navigable,
        defect: entry.defect.unwrap_or_default(),
    }
}
