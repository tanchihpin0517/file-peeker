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
    /// Why this one entry could not be fully described, and `None` when it could.
    ///
    /// This used to be impossible to express: anything wrong with an entry ended the
    /// listing, so one bad entry reported failure for the directory and hid every
    /// sibling behind it.
    pub defect: Option<String>,
}

impl DirectoryEntry {
    /// Whether anything about this entry could not be read.
    ///
    /// Read by the front ends, which the slim server build leaves out.
    #[cfg_attr(not(feature = "client"), allow(dead_code))]
    pub(crate) fn is_degraded(&self) -> bool {
        self.defect.is_some()
    }

    /// An entry described without trouble, for the many fixtures that are about
    /// names, kinds and placement rather than about what went wrong.
    #[cfg(test)]
    pub(crate) fn readable(name: HostPath, kind: EntryKind, navigable: bool) -> Self {
        Self {
            name,
            kind,
            navigable,
            defect: None,
        }
    }
}
