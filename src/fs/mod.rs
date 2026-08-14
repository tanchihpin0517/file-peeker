//! Transport-independent filesystem operations for File Peeker.

mod directory;
mod error;
mod fs_service;
mod read;
mod resolve_path;
mod write;

pub(crate) use directory::{
    DirectoryEntry, EntryKind, EntryMetadata, EntryStream, WalkEntry, WalkStream,
};
pub(crate) use error::{FsError, FsErrorKind};
pub(crate) use fs_service::FsService;
pub(crate) use read::ReadStream;
pub(crate) use write::WriteConflict;
