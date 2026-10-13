use std::{fs::FileType, path::PathBuf};

use tokio::fs;

use super::{DirectoryEntry, EntryKind, EntryMetadata};
use crate::{fs::FsError, host_path::HostPath};

pub(super) struct InspectedEntry {
    pub(super) path: PathBuf,
    pub(super) entry: DirectoryEntry,
    pub(super) descend: bool,
}

/// Describes one directory entry, and never fails.
///
/// Whatever goes wrong is a property of the entry it went wrong for, so the listing
/// carries on and its only terminal conditions stay the two that really are:
/// iterating the directory failing, and cancellation.
///
/// This is what pays for the metadata. Reading a size, a time and a mode needs a real
/// `lstat`, and an `lstat` can be denied for one entry while its siblings are fine —
/// so the per-entry failure below is not defensive, it is the routine case.
pub(super) async fn inspect_entry(entry: &fs::DirEntry) -> InspectedEntry {
    let path = entry.path();
    // A name the host holds as bytes is escaped rather than refused.
    let name = HostPath::escape_os(&entry.file_name());

    // One `lstat` for the file type *and* the size, time and mode, replacing the
    // `DirEntry::file_type` that used to answer from what `read_dir` already carried.
    // That was often free and reported nothing else; this costs a syscall per entry
    // and is the only way to have the rest.
    let metadata = match fs::symlink_metadata(&path).await {
        Ok(metadata) => metadata,
        Err(error) => {
            // Refused, not missing: a directory that is readable but not searchable
            // lists its entries and then refuses to stat any of them. The kind may
            // still be known, because `read_dir` often carries it and it is the stat
            // that was denied rather than the entry itself.
            let kind = entry.file_type().await.map_or(EntryKind::Other, kind_of);
            return InspectedEntry {
                path,
                entry: DirectoryEntry {
                    name,
                    kind,
                    // Nothing here can be opened: the usual cause is a parent this
                    // process may not search, so its children are unreachable.
                    navigable: false,
                    metadata: None,
                    defect: Some(FsError::from_io(&error).message().to_owned()),
                },
                descend: false,
            };
        }
    };

    let file_type = metadata.file_type();
    let descend = file_type.is_dir();
    let kind = kind_of(file_type);
    // A symlink is navigable when what it points at is a directory, which is the one
    // place the target is consulted -- and it stays that way, so navigation behaves
    // exactly as it did before metadata existed.
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
            .map(|target| HostPath::from_path(&target))
    } else {
        None
    };

    InspectedEntry {
        path,
        entry: DirectoryEntry {
            name,
            kind,
            navigable,
            metadata: Some(EntryMetadata {
                size: metadata.len(),
                modified: metadata.modified().ok(),
                unix_mode: unix_mode(&metadata),
                symlink_target,
            }),
            defect: None,
        },
        descend,
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

// The `Option` is the shape both arms share, not a needless wrapper: off Unix there
// is genuinely no mode to report.
#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)]
fn unix_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;

    Some(metadata.permissions().mode())
}

#[cfg(not(unix))]
fn unix_mode(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}
