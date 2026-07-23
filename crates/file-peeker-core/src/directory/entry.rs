use std::path::PathBuf;

use tokio::fs;

use super::{DirectoryEntry, EntryKind};
use crate::{FsError, FsErrorKind};

pub(super) struct InspectedEntry {
    pub(super) path: PathBuf,
    pub(super) entry: DirectoryEntry,
    pub(super) descend: bool,
}

pub(super) async fn inspect_entry(entry: &fs::DirEntry) -> Result<InspectedEntry, FsError> {
    let path = entry.path();
    if path.to_str().is_none() {
        return Err(FsError::new(
            FsErrorKind::InvalidArgument,
            "Encountered a non-UTF-8 path",
        ));
    }
    let name = entry
        .file_name()
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| {
            FsError::new(
                FsErrorKind::InvalidArgument,
                "Encountered a non-UTF-8 filename",
            )
        })?;
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
