use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use crate::{DirectoryEntry, FilePeekerError, StateRow, StateSnapshot, ops, session::Session};

#[derive(Debug)]
pub(crate) struct State {
    session: Arc<Session>,
    data: Mutex<StateData>,
}

impl State {
    /// Returns the current fixed-root state snapshot.
    #[must_use]
    pub(crate) fn snapshot(&self) -> StateSnapshot {
        lock_state(&self.data).snapshot()
    }

    /// Freshly loads and expands one visible directory.
    ///
    /// # Errors
    ///
    /// Returns an invalid-path error for an unknown or non-navigable row, or a
    /// connection, protocol, or filesystem error when listing the directory fails.
    pub(crate) async fn expand(&self, path: String) -> Result<StateSnapshot, FilePeekerError> {
        self.session.ensure_open()?;
        let revision = match lock_state(&self.data).prepare_expand(&path)? {
            ExpandAction::Ready(snapshot) => return Ok(snapshot),
            ExpandAction::Load { revision } => revision,
        };
        match ops::collect(self.session.socket_path().to_path_buf(), path.clone()).await {
            Ok(entries) => Ok(lock_state(&self.data).finish_expand(revision, &path, entries)),
            Err(error) => {
                lock_state(&self.data).fail_expand(revision, &path, &error);
                Err(error)
            }
        }
    }

    /// Collapses a visible directory and discards all of its descendants.
    ///
    /// # Errors
    ///
    /// Returns an invalid-path error for an unknown or non-navigable row.
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn collapse(&self, path: String) -> Result<StateSnapshot, FilePeekerError> {
        self.session.ensure_open()?;
        lock_state(&self.data).collapse(&path)
    }
}

impl State {
    pub(super) async fn load(
        session: Arc<Session>,
        path: String,
    ) -> Result<Arc<Self>, FilePeekerError> {
        session.ensure_open()?;
        let path = ops::absolute_utf8_path(&path)?;
        let entries = ops::collect(session.socket_path().to_path_buf(), path.clone()).await?;
        Ok(Arc::new(Self {
            session,
            data: Mutex::new(StateData::new(path, entries)),
        }))
    }
}

fn lock_state(state: &Mutex<StateData>) -> MutexGuard<'_, StateData> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
pub(super) struct StateData {
    path: String,
    rows: Vec<StateRow>,
    revisions: HashMap<String, u64>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ExpandAction {
    Ready(StateSnapshot),
    Load { revision: u64 },
}

impl StateData {
    pub(super) fn new(path: String, entries: Vec<DirectoryEntry>) -> Self {
        let rows: Vec<_> = entries
            .into_iter()
            .map(|entry| StateRow {
                entry,
                parent_path: None,
                depth: 0,
                expanded: false,
                error_message: None,
            })
            .collect();
        let revisions = rows.iter().map(|row| (row.entry.path.clone(), 0)).collect();
        Self {
            path,
            rows,
            revisions,
        }
    }

    pub(super) fn prepare_expand(&mut self, path: &str) -> Result<ExpandAction, FilePeekerError> {
        let Some(row) = self.rows.iter_mut().find(|row| row.entry.path == path) else {
            return Err(unknown_path(path));
        };
        if !row.entry.navigable {
            return Err(not_directory(path));
        }
        if row.expanded {
            return Ok(ExpandAction::Ready(self.snapshot()));
        }

        row.error_message = None;
        let revision = self.revisions.entry(path.to_owned()).or_default();
        *revision = revision.wrapping_add(1);
        Ok(ExpandAction::Load {
            revision: *revision,
        })
    }

    pub(super) fn finish_expand(
        &mut self,
        revision: u64,
        parent_path: &str,
        entries: Vec<DirectoryEntry>,
    ) -> StateSnapshot {
        if self.revisions.get(parent_path) != Some(&revision) {
            return self.snapshot();
        }
        let Some(parent_index) = self
            .rows
            .iter()
            .position(|row| row.entry.path == parent_path && !row.expanded)
        else {
            return self.snapshot();
        };

        let depth = self.rows[parent_index].depth + 1;
        self.rows[parent_index].expanded = true;
        self.rows[parent_index].error_message = None;
        for (offset, entry) in entries.into_iter().enumerate() {
            self.revisions.insert(entry.path.clone(), 0);
            self.rows.insert(
                parent_index + 1 + offset,
                StateRow {
                    entry,
                    parent_path: Some(parent_path.to_owned()),
                    depth,
                    expanded: false,
                    error_message: None,
                },
            );
        }
        self.snapshot()
    }

    pub(super) fn fail_expand(&mut self, revision: u64, path: &str, error: &FilePeekerError) {
        if self.revisions.get(path) != Some(&revision) {
            return;
        }
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| row.entry.path == path && !row.expanded)
        {
            row.error_message = Some(error.to_string());
        }
    }

    pub(super) fn collapse(&mut self, path: &str) -> Result<StateSnapshot, FilePeekerError> {
        let Some(index) = self.rows.iter().position(|row| row.entry.path == path) else {
            return Err(unknown_path(path));
        };
        if !self.rows[index].entry.navigable {
            return Err(not_directory(path));
        }

        let depth = self.rows[index].depth;
        let descendants_start = index + 1;
        let descendants_end = self.rows[descendants_start..]
            .iter()
            .position(|row| row.depth <= depth)
            .map_or(self.rows.len(), |offset| descendants_start + offset);
        for row in &self.rows[descendants_start..descendants_end] {
            self.revisions.remove(&row.entry.path);
        }
        self.rows.drain(descendants_start..descendants_end);
        let revision = self.revisions.entry(path.to_owned()).or_default();
        *revision = revision.wrapping_add(1);
        self.rows[index].expanded = false;
        self.rows[index].error_message = None;
        Ok(self.snapshot())
    }

    pub(super) fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            path: self.path.clone(),
            rows: self.rows.clone(),
        }
    }
}

fn unknown_path(path: &str) -> FilePeekerError {
    FilePeekerError::InvalidPath {
        message: format!("state entry does not exist: {path}"),
    }
}

fn not_directory(path: &str) -> FilePeekerError {
    FilePeekerError::InvalidPath {
        message: format!("state entry is not a directory: {path}"),
    }
}

#[cfg(test)]
mod tests {
    use crate::{DirectoryEntry, EntryKind, FilePeekerError};

    use super::{ExpandAction, StateData};

    fn entry(path: &str, navigable: bool) -> DirectoryEntry {
        DirectoryEntry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            kind: if navigable {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            navigable,
        }
    }

    fn begin_expand(state: &mut StateData, path: &str) -> u64 {
        match state.prepare_expand(path).unwrap() {
            ExpandAction::Load { revision } => revision,
            ExpandAction::Ready(_) => panic!("expected a fresh directory load"),
        }
    }

    #[test]
    fn expands_nested_rows_and_reloads_after_collapse() {
        let mut state = StateData::new("/root".into(), vec![entry("/root/a", true)]);
        let revision = begin_expand(&mut state, "/root/a");
        state.finish_expand(revision, "/root/a", vec![entry("/root/a/b", true)]);
        let revision = begin_expand(&mut state, "/root/a/b");
        state.finish_expand(revision, "/root/a/b", vec![entry("/root/a/b/file", false)]);

        assert_eq!(state.snapshot().rows.len(), 3);
        assert_eq!(state.snapshot().rows[2].depth, 2);
        assert_eq!(state.collapse("/root/a").unwrap().rows.len(), 1);

        let revision = begin_expand(&mut state, "/root/a");
        let snapshot = state.finish_expand(revision, "/root/a", vec![entry("/root/a/new", false)]);
        assert_eq!(snapshot.path, "/root");
        assert_eq!(snapshot.rows[1].entry.path, "/root/a/new");
    }

    #[test]
    fn collapsed_expansion_results_are_ignored() {
        let mut state = StateData::new("/root".into(), vec![entry("/root/a", true)]);
        let revision = begin_expand(&mut state, "/root/a");
        state.collapse("/root/a").unwrap();
        let snapshot = state.finish_expand(revision, "/root/a", vec![entry("/root/a/file", false)]);
        assert_eq!(snapshot.rows.len(), 1);
        assert!(!snapshot.rows[0].expanded);
    }

    #[test]
    fn failed_expansion_stays_collapsed_and_can_retry() {
        let mut state = StateData::new("/root".into(), vec![entry("/root/private", true)]);
        let revision = begin_expand(&mut state, "/root/private");
        state.fail_expand(
            revision,
            "/root/private",
            &FilePeekerError::Io {
                message: "permission denied".into(),
            },
        );

        let snapshot = state.snapshot();
        assert!(!snapshot.rows[0].expanded);
        assert_eq!(
            snapshot.rows[0].error_message.as_deref(),
            Some("filesystem I/O error: permission denied")
        );
        assert!(matches!(
            state.prepare_expand("/root/private").unwrap(),
            ExpandAction::Load { .. }
        ));
    }

    #[test]
    fn concurrent_sibling_expansions_merge_by_current_position() {
        let mut state = StateData::new(
            "/root".into(),
            vec![entry("/root/a", true), entry("/root/b", true)],
        );
        let revision_a = begin_expand(&mut state, "/root/a");
        let revision_b = begin_expand(&mut state, "/root/b");

        state.finish_expand(revision_b, "/root/b", vec![entry("/root/b/file", false)]);
        let snapshot =
            state.finish_expand(revision_a, "/root/a", vec![entry("/root/a/file", false)]);

        assert_eq!(snapshot.rows.len(), 4);
        assert!(
            snapshot
                .rows
                .iter()
                .find(|row| row.entry.path == "/root/a")
                .unwrap()
                .expanded
        );
        assert!(
            snapshot
                .rows
                .iter()
                .find(|row| row.entry.path == "/root/b")
                .unwrap()
                .expanded
        );
    }

    #[test]
    fn rejects_unknown_and_non_navigable_entries() {
        let mut state = StateData::new("/root".into(), vec![entry("/root/file", false)]);
        assert!(matches!(
            state.prepare_expand("/missing"),
            Err(FilePeekerError::InvalidPath { .. })
        ));
        assert!(matches!(
            state.collapse("/root/file"),
            Err(FilePeekerError::InvalidPath { .. })
        ));
    }
}
