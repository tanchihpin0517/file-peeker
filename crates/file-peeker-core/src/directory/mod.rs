mod list;

pub use list::EntryStream;
pub(crate) use list::list_dir;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: EntryKind,
    pub navigable: bool,
}
