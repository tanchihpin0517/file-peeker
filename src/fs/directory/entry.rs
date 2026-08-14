use std::{fs::FileType, path::PathBuf};

use tokio::fs;

use super::{DirectoryEntry, EntryKind};
use crate::{fs::FsError, host_path::HostPath};

pub(super) struct InspectedEntry {
    pub(super) path: PathBuf,
    pub(super) entry: DirectoryEntry,
    pub(super) descend: bool,
}

/// Describes one directory entry, and never fails.
///
/// It used to return a `Result`, and an error here ended the whole listing: the
/// stream yielded it as its final item and dropped the `ReadDir` behind it. So one
/// entry the host would not describe hid every sibling that had not arrived yet, and
/// the directory reported failure instead of reporting the entries it did have.
///
/// Whatever goes wrong now travels with the entry it went wrong for, which leaves
/// only two conditions terminal for a listing — iterating the directory failing, and
/// cancellation — and those two really are.
pub(super) async fn inspect_entry(entry: &fs::DirEntry) -> InspectedEntry {
    let path = entry.path();
    // A name the host holds as bytes is escaped rather than refused.
    let name = HostPath::escape_os(&entry.file_name());

    let file_type = match entry.file_type().await {
        Ok(file_type) => file_type,
        Err(error) => {
            return InspectedEntry {
                path,
                entry: DirectoryEntry {
                    name,
                    // Unknowable without the type. `Other` is the kind that claims
                    // the least: not a file, directory or link this listing can
                    // vouch for.
                    kind: EntryKind::Other,
                    // What cannot be described cannot be opened either.
                    navigable: false,
                    defect: Some(FsError::from_io(&error).message().to_owned()),
                },
                descend: false,
            };
        }
    };

    let descend = file_type.is_dir();
    let kind = kind_of(file_type);
    // A symlink is navigable when what it points at is a directory, which is the one
    // place the target is consulted.
    let navigable = match kind {
        EntryKind::Directory => true,
        EntryKind::Symlink => fs::metadata(&path)
            .await
            .is_ok_and(|target| target.is_dir()),
        EntryKind::File | EntryKind::Other => false,
    };

    InspectedEntry {
        path,
        entry: DirectoryEntry {
            name,
            kind,
            navigable,
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
