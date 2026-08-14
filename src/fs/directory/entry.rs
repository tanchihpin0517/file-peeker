use std::path::PathBuf;

use tokio::fs;

use super::{DirectoryEntry, EntryKind};
use crate::{fs::FsError, host_path::HostPath};

pub(super) struct InspectedEntry {
    pub(super) path: PathBuf,
    pub(super) entry: DirectoryEntry,
    pub(super) descend: bool,
}

pub(super) async fn inspect_entry(entry: &fs::DirEntry) -> Result<InspectedEntry, FsError> {
    let path = entry.path();
    // A name the host holds as bytes is escaped rather than refused. It used to end
    // the listing, which meant one unusual filename hid every entry behind it; the
    // escape is invertible, so the entry stays addressable.
    let name = HostPath::escape_os(&entry.file_name());
    let file_type = entry
        .file_type()
        .await
        .map_err(|error| FsError::from_io(&error))?;
    let descend = file_type.is_dir();
    let (kind, navigable) = if descend {
        (EntryKind::Directory, true)
    } else if file_type.is_file() {
        (EntryKind::File, false)
    } else if file_type.is_symlink() {
        (
            EntryKind::Symlink,
            fs::metadata(&path)
                .await
                .is_ok_and(|metadata| metadata.is_dir()),
        )
    } else {
        (EntryKind::Other, false)
    };

    Ok(InspectedEntry {
        path,
        entry: DirectoryEntry {
            name,
            kind,
            navigable,
        },
        descend,
    })
}
