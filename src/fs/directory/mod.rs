mod entry;
mod list;
mod walk;

pub(crate) use list::EntryStream;
pub(crate) use list::list_dir;
pub(crate) use walk::walk_dir;
pub(crate) use walk::{WalkEntry, WalkStream};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryEntry {
    pub name: String,
    pub kind: EntryKind,
    pub navigable: bool,
}
