mod list;
mod walk;

pub use list::EntryStream;
pub use walk::{WalkEntry, WalkStream};

pub type EntryKind = file_peeker_core::EntryKind;

#[uniffi::remote(Enum)]
enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

pub type DirectoryEntry = file_peeker_core::DirectoryEntry;

#[uniffi::remote(Record)]
struct DirectoryEntry {
    pub name: String,
    pub kind: EntryKind,
    pub navigable: bool,
}
