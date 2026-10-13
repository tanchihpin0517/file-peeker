use std::{fs::FileType, os::unix::fs::PermissionsExt as _, path::PathBuf, time::SystemTime};

use tokio::fs;

use super::FsError;
use crate::path_string::PathString;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// What one `lstat` of an entry reports.
///
/// A symlink describes **itself**, as `ls -l` does, and names its target
/// separately. Reporting the target's instead would leave a broken link with no
/// metadata at all, indistinguishable from one this process may not stat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EntryMetadata {
    pub size: u64,
    /// `None` where the filesystem keeps no modification time.
    pub modified: Option<SystemTime>,
    /// The mode bits, type and permissions both.
    pub mode: u32,
    /// Where a symlink points, and `None` for anything else. For display only: a
    /// target is never followed or addressed.
    pub symlink_target: Option<PathString>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryEntry {
    pub name: PathString,
    pub kind: EntryKind,
    /// Whether opening it lists a directory: a directory, or a symlink to one.
    pub navigable: bool,
    /// `None` when this one entry could not be inspected, which [`defect`]
    /// explains.
    ///
    /// The common way in is a directory that is readable but not searchable: it
    /// lists its entries and then refuses to stat any of them. A stat is a
    /// syscall per entry that can be denied on its own, which is why the failure
    /// belongs to the entry rather than to the listing.
    ///
    /// [`defect`]: DirectoryEntry::defect
    pub metadata: Option<EntryMetadata>,
    /// Why this one entry could not be fully described, and `None` when it could.
    pub defect: Option<String>,
}

impl DirectoryEntry {
    /// Whether anything about this entry could not be read.
    #[must_use]
    pub(crate) fn is_degraded(&self) -> bool {
        self.metadata.is_none()
    }
}

pub(super) struct Inspected {
    pub path: PathBuf,
    pub entry: DirectoryEntry,
    /// A real directory, which a walk goes into. Never a symlink.
    pub descend: bool,
}

/// Describes one directory entry, and never fails.
///
/// Whatever goes wrong is a property of the entry it went wrong for, so a listing
/// carries on, and its only end other than the last entry is iterating the
/// directory failing.
pub(super) async fn inspect(entry: &fs::DirEntry) -> Inspected {
    let path = entry.path();
    // A name held as bytes is escaped rather than refused.
    let name = PathString::from_os_str(&entry.file_name());

    let metadata = match fs::symlink_metadata(&path).await {
        Ok(metadata) => metadata,
        Err(error) => {
            // Refused, not missing: the kind may still be known, because
            // `read_dir` often carries it and it is the stat that was denied.
            let kind = entry.file_type().await.map_or(EntryKind::Other, kind_of);
            return Inspected {
                path,
                entry: DirectoryEntry {
                    name,
                    kind,
                    // The usual cause is a parent this process may not search, so
                    // nothing under it can be opened either.
                    navigable: false,
                    metadata: None,
                    defect: Some(FsError::from_io(&error).message().to_owned()),
                },
                descend: false,
            };
        }
    };

    let file_type = metadata.file_type();
    let kind = kind_of(file_type);
    // The one place a symlink's target is consulted.
    let navigable = match kind {
        EntryKind::Directory => true,
        EntryKind::Symlink => fs::metadata(&path)
            .await
            .is_ok_and(|target| target.is_dir()),
        EntryKind::File | EntryKind::Other => false,
    };
    let symlink_target = if kind == EntryKind::Symlink {
        fs::read_link(&path)
            .await
            .ok()
            .map(|target| PathString::from_path(&target))
    } else {
        None
    };

    Inspected {
        path,
        entry: DirectoryEntry {
            name,
            kind,
            navigable,
            metadata: Some(EntryMetadata {
                size: metadata.len(),
                modified: metadata.modified().ok(),
                mode: metadata.permissions().mode(),
                symlink_target,
            }),
            defect: None,
        },
        descend: file_type.is_dir(),
    }
}

fn kind_of(file_type: FileType) -> EntryKind {
    if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    }
}
