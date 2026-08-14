mod entry;
mod list;
mod walk;

use std::time::SystemTime;

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

/// What one `lstat` of an entry reports.
///
/// A symlink describes **itself** here, the way `ls -l` does, and names its target
/// separately. Reporting the target's instead would leave a broken link with no
/// metadata at all, which would make a dangling link indistinguishable from one this
/// process may not stat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EntryMetadata {
    pub size: u64,
    /// `None` where the platform has no modification time to give.
    pub modified: Option<SystemTime>,
    /// `None` off Unix, where there is no mode.
    pub unix_mode: Option<u32>,
    /// Where a symlink points, escaped like any other name, and `None` for anything
    /// that is not one. Display only: a target is never followed or addressed.
    pub symlink_target: Option<HostPath>,
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
    /// `None` when this one entry could not be inspected, which is what [`defect`]
    /// then explains.
    ///
    /// The common way in is a directory that is readable but not searchable: it lists
    /// its entries and then refuses to stat any of them. Reading metadata is a
    /// per-entry syscall that can be denied on its own, which is why it belongs to the
    /// entry rather than to the listing.
    ///
    /// [`defect`]: DirectoryEntry::defect
    pub metadata: Option<EntryMetadata>,
    /// Why this one entry could not be fully described, and `None` when it could.
    pub defect: Option<String>,
}

impl DirectoryEntry {
    /// Whether anything about this entry could not be read.
    ///
    /// Read by the front ends, which the slim server build leaves out.
    #[cfg_attr(not(feature = "client"), allow(dead_code))]
    pub(crate) fn is_degraded(&self) -> bool {
        self.metadata.is_none()
    }

    /// An entry that was inspected without trouble, for the many fixtures that are
    /// about names, kinds and placement rather than about metadata.
    #[cfg(test)]
    pub(crate) fn readable(name: HostPath, kind: EntryKind, navigable: bool) -> Self {
        Self {
            name,
            kind,
            navigable,
            metadata: Some(EntryMetadata::readable()),
            defect: None,
        }
    }
}

#[cfg(test)]
impl EntryMetadata {
    /// Plain metadata with nothing interesting in it.
    pub(crate) fn readable() -> Self {
        Self {
            size: 0,
            modified: Some(SystemTime::UNIX_EPOCH),
            unix_mode: None,
            symlink_target: None,
        }
    }
}
