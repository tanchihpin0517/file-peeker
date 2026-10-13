//! This host's filesystem: the only module that touches it.
//!
//! Task-agnostic. Every operation here runs inside a task, and the task owns
//! cancelling it, as it owns progress and state: cancelling is **dropping** the
//! future or stream, which every caller does by selecting on its context's
//! `cancelled()`. Nothing here holds a token. Each `.await` is a point it can
//! stop at, and a stream does no work until it is pulled, so a dropped operation
//! stops where it stood; a write takes its partial file with it.
//!
//! Paths arrive as [`PathString`], absolute, and are opened exactly as given.
//! Nothing is interpreted here — not `~`, not `$VARIABLE`, not `.` or `..` — so
//! a path names one file and only the kernel reads it. What a person typed is
//! read by whatever took it in, which knows the tab it was typed in; a relative
//! path is refused, since the server's own working directory is wherever the
//! client that started it happened to be.
//!
//! | Operation | Does |
//! | --- | --- |
//! | [`list_dir`] | One directory's entries, streamed |
//! | [`walk_dir`] | Everything below a directory, depth first, links not followed |
//! | [`read_file`] | A regular file's bytes, streamed |
//! | [`write_file`] | A new file from a stream, published whole or not at all |

mod absolute;
mod entry;
mod error;
mod list;
mod read;
mod walk;
mod write;

#[cfg(test)]
mod tests;

pub(crate) use entry::{DirectoryEntry, EntryKind, EntryMetadata};
pub(crate) use error::{FsError, FsErrorKind};
pub(crate) use list::{EntryStream, list_dir};
pub(crate) use read::{ReadStream, read_file};
pub(crate) use walk::{WalkEntry, WalkStream, walk_dir};
pub(crate) use write::{WriteConflict, Written, write_file};

#[cfg(doc)]
use crate::path_string::PathString;
