//! The headless model of one browsed root: a flat pre-order list of rows,
//! expanded in place as listings stream in.
//!
//! Both front ends drive this. Neither the terminal's viewport nor the desktop's
//! scroll area appears here, and nor does the client: a [`Tree`] is fed groups of
//! entries by whoever owns the listing task, and answers questions about the rows
//! it has placed. That is what lets the same model back a `ratatui` frame and an
//! `egui` table, and what lets its own tests place a thousand rows without a
//! server or a runtime.
//!
//! The invariant worth restating, because getting it wrong is what made a large
//! expansion stall: **the insertion point of an in-flight expansion is carried
//! forward, never recomputed.** Walking to the end of a subtree once per arriving
//! group is quadratic in the size of the directory.

mod tree;
mod window;

use std::path::Path;

use crate::DirectoryEntry;

pub(crate) use tree::Tree;
pub(crate) use window::{VisibleRows, in_scope, matches_needle};

/// How far a row is from the root. `u16` rather than `usize` so the indent
/// arithmetic converts to `f32` without a lossy cast.
pub(crate) type Depth = u16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Status {
    Loading,
    Complete,
    Failed(String),
}

/// Whether a row is open, and the request that opened it.
///
/// A row that is not a directory is `Collapsed` and stays there: activation
/// checks [`DirectoryEntry::navigable`] before it ever gets here, so there is no
/// separate "cannot expand" state to keep in step with the entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Expansion {
    Collapsed,
    Loading { request_id: u64 },
    Expanded { request_id: u64 },
    Failed { request_id: u64, error: String },
}

impl Expansion {
    pub(crate) fn request_id(&self) -> Option<u64> {
        match self {
            Self::Collapsed => None,
            Self::Loading { request_id }
            | Self::Expanded { request_id }
            | Self::Failed { request_id, .. } => Some(*request_id),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Row {
    path: String,
    depth: Depth,
    entry: DirectoryEntry,
    expansion: Expansion,
}

impl Row {
    fn new(path: String, depth: Depth, entry: DirectoryEntry) -> Self {
        Self {
            path,
            depth,
            entry,
            expansion: Expansion::Collapsed,
        }
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn name(&self) -> &str {
        &self.entry.name
    }

    pub(crate) fn depth(&self) -> Depth {
        self.depth
    }

    pub(crate) fn entry(&self) -> &DirectoryEntry {
        &self.entry
    }

    pub(crate) fn expansion(&self) -> &Expansion {
        &self.expansion
    }
}

pub(crate) fn child_path(parent: &str, name: &str) -> String {
    Path::new(parent).join(name).to_string_lossy().into_owned()
}

/// Entries to build a tree out of in tests, here rather than in each test module
/// because both front ends drive the same model and want the same fixtures.
#[cfg(test)]
pub(crate) mod fixture {
    use crate::{DirectoryEntry, EntryKind};

    /// A named entry, a directory when it is navigable and a plain file when it is
    /// not.
    pub(crate) fn entry(name: &str, navigable: bool) -> DirectoryEntry {
        DirectoryEntry {
            name: name.into(),
            kind: if navigable {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            navigable,
        }
    }
}
