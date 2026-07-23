//! Transport-independent filesystem operations for File Peeker.

mod directory;
mod error;
mod fs_service;
mod read;
mod resolve_path;

pub use directory::{DirectoryEntry, EntryKind, EntryStream};
pub use error::{FsError, FsErrorKind};
pub use fs_service::FsService;
pub use read::ReadStream;
