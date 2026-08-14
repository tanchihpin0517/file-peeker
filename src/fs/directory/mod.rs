mod entry;
mod list;
mod walk;

use crate::host_path::HostPath;

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
    /// Escaped, so a name the host holds as bytes survives a `String` and a
    /// protobuf `string` without losing one of them. Front ends show
    /// [`HostPath::display`]; anything addressing the entry again uses
    /// [`HostPath::as_escaped`].
    pub name: HostPath,
    pub kind: EntryKind,
    pub navigable: bool,
}
