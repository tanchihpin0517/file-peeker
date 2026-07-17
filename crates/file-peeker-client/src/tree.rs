use std::collections::HashMap;

use crate::{ClientError, DirectoryEntry, DirectoryTreeRow};

#[derive(Debug, Default)]
pub(super) struct DirectoryTree {
    generation: u64,
    root: Option<String>,
    rows: Vec<DirectoryTreeRow>,
    revisions: HashMap<String, u64>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ExpandAction {
    Ready(Vec<DirectoryTreeRow>),
    Load { generation: u64, revision: u64 },
}

impl DirectoryTree {
    pub(super) fn begin_root(&mut self, root: String) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.root = Some(root);
        self.rows.clear();
        self.revisions.clear();
        self.generation
    }

    pub(super) fn finish_root(
        &mut self,
        generation: u64,
        entries: Vec<DirectoryEntry>,
    ) -> Vec<DirectoryTreeRow> {
        if generation == self.generation {
            self.rows = entries
                .into_iter()
                .map(|entry| DirectoryTreeRow {
                    entry,
                    parent_path: None,
                    depth: 0,
                    expanded: false,
                    error_message: None,
                })
                .collect();
            self.revisions = self
                .rows
                .iter()
                .map(|row| (row.entry.path.clone(), 0))
                .collect();
        }
        self.rows.clone()
    }

    pub(super) fn prepare_expand(&mut self, path: &str) -> Result<ExpandAction, ClientError> {
        let Some(row) = self.rows.iter_mut().find(|row| row.entry.path == path) else {
            return Err(unknown_path(path));
        };
        if !row.entry.navigable {
            return Err(not_directory(path));
        }
        if row.expanded {
            return Ok(ExpandAction::Ready(self.rows.clone()));
        }

        row.error_message = None;
        let revision = self.revisions.entry(path.to_owned()).or_default();
        *revision = revision.wrapping_add(1);
        Ok(ExpandAction::Load {
            generation: self.generation,
            revision: *revision,
        })
    }

    pub(super) fn finish_expand(
        &mut self,
        generation: u64,
        revision: u64,
        parent_path: &str,
        entries: Vec<DirectoryEntry>,
    ) -> Vec<DirectoryTreeRow> {
        if generation != self.generation || self.revisions.get(parent_path) != Some(&revision) {
            return self.rows.clone();
        }
        let Some(parent_index) = self
            .rows
            .iter()
            .position(|row| row.entry.path == parent_path && !row.expanded)
        else {
            return self.rows.clone();
        };

        let depth = self.rows[parent_index].depth + 1;
        self.rows[parent_index].expanded = true;
        self.rows[parent_index].error_message = None;
        for (offset, entry) in entries.into_iter().enumerate() {
            self.revisions.insert(entry.path.clone(), 0);
            self.rows.insert(
                parent_index + 1 + offset,
                DirectoryTreeRow {
                    entry,
                    parent_path: Some(parent_path.to_owned()),
                    depth,
                    expanded: false,
                    error_message: None,
                },
            );
        }
        self.rows.clone()
    }

    pub(super) fn fail_expand(
        &mut self,
        generation: u64,
        revision: u64,
        path: &str,
        error: &ClientError,
    ) {
        if generation != self.generation || self.revisions.get(path) != Some(&revision) {
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

    pub(super) fn collapse(&mut self, path: &str) -> Result<Vec<DirectoryTreeRow>, ClientError> {
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
        Ok(self.rows.clone())
    }

    pub(super) fn rows(&self) -> Vec<DirectoryTreeRow> {
        self.rows.clone()
    }
}

fn unknown_path(path: &str) -> ClientError {
    ClientError::InvalidPath {
        message: format!("tree entry does not exist: {path}"),
    }
}

fn not_directory(path: &str) -> ClientError {
    ClientError::InvalidPath {
        message: format!("tree entry is not a directory: {path}"),
    }
}

#[cfg(test)]
mod tests {
    use crate::{ClientError, DirectoryEntry, EntryKind};

    use super::{DirectoryTree, ExpandAction};

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

    fn begin_expand(tree: &mut DirectoryTree, path: &str) -> (u64, u64) {
        match tree.prepare_expand(path).unwrap() {
            ExpandAction::Load {
                generation,
                revision,
            } => (generation, revision),
            ExpandAction::Ready(_) => panic!("expected a fresh directory load"),
        }
    }

    #[test]
    fn expands_nested_rows_and_discards_them_on_collapse() {
        let mut tree = DirectoryTree::default();
        let generation = tree.begin_root("/root".into());
        tree.finish_root(generation, vec![entry("/root/a", true)]);
        let (generation, revision) = begin_expand(&mut tree, "/root/a");
        tree.finish_expand(
            generation,
            revision,
            "/root/a",
            vec![entry("/root/a/b", true)],
        );
        let (generation, revision) = begin_expand(&mut tree, "/root/a/b");
        tree.finish_expand(
            generation,
            revision,
            "/root/a/b",
            vec![entry("/root/a/b/file", false)],
        );

        assert_eq!(tree.rows().len(), 3);
        assert_eq!(tree.rows()[2].depth, 2);
        assert_eq!(tree.collapse("/root/a").unwrap().len(), 1);

        let (generation, revision) = begin_expand(&mut tree, "/root/a");
        let rows = tree.finish_expand(
            generation,
            revision,
            "/root/a",
            vec![entry("/root/a/new", false)],
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].entry.path, "/root/a/new");
    }

    #[test]
    fn stale_root_and_collapsed_expansion_results_are_ignored() {
        let mut tree = DirectoryTree::default();
        let old_generation = tree.begin_root("/old".into());
        tree.finish_root(old_generation, vec![entry("/old/a", true)]);
        let (_, old_revision) = begin_expand(&mut tree, "/old/a");

        let new_generation = tree.begin_root("/new".into());
        tree.finish_root(new_generation, vec![entry("/new/file", false)]);
        tree.finish_expand(
            old_generation,
            old_revision,
            "/old/a",
            vec![entry("/old/a/file", false)],
        );
        assert_eq!(tree.rows()[0].entry.path, "/new/file");

        let generation = tree.begin_root("/root".into());
        tree.finish_root(generation, vec![entry("/root/a", true)]);
        let (generation, revision) = begin_expand(&mut tree, "/root/a");
        tree.collapse("/root/a").unwrap();
        let rows = tree.finish_expand(
            generation,
            revision,
            "/root/a",
            vec![entry("/root/a/file", false)],
        );
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].expanded);
    }

    #[test]
    fn failed_expansion_stays_collapsed_and_can_retry() {
        let mut tree = DirectoryTree::default();
        let generation = tree.begin_root("/root".into());
        tree.finish_root(generation, vec![entry("/root/private", true)]);
        let (generation, revision) = begin_expand(&mut tree, "/root/private");
        tree.fail_expand(
            generation,
            revision,
            "/root/private",
            &ClientError::Io {
                message: "permission denied".into(),
            },
        );

        let row = &tree.rows()[0];
        assert!(!row.expanded);
        assert_eq!(
            row.error_message.as_deref(),
            Some("filesystem I/O error: permission denied")
        );
        assert!(matches!(
            tree.prepare_expand("/root/private").unwrap(),
            ExpandAction::Load { .. }
        ));
    }

    #[test]
    fn concurrent_sibling_expansions_merge_by_current_position() {
        let mut tree = DirectoryTree::default();
        let generation = tree.begin_root("/root".into());
        tree.finish_root(
            generation,
            vec![entry("/root/a", true), entry("/root/b", true)],
        );
        let (generation_a, revision_a) = begin_expand(&mut tree, "/root/a");
        let (generation_b, revision_b) = begin_expand(&mut tree, "/root/b");

        tree.finish_expand(
            generation_b,
            revision_b,
            "/root/b",
            vec![entry("/root/b/file", false)],
        );
        let rows = tree.finish_expand(
            generation_a,
            revision_a,
            "/root/a",
            vec![entry("/root/a/file", false)],
        );

        assert_eq!(rows.len(), 4);
        assert!(
            rows.iter()
                .find(|row| row.entry.path == "/root/a")
                .unwrap()
                .expanded
        );
        assert!(
            rows.iter()
                .find(|row| row.entry.path == "/root/b")
                .unwrap()
                .expanded
        );
    }

    #[test]
    fn rejects_unknown_and_non_navigable_entries() {
        let mut tree = DirectoryTree::default();
        let generation = tree.begin_root("/root".into());
        tree.finish_root(generation, vec![entry("/root/file", false)]);

        assert!(matches!(
            tree.prepare_expand("/missing"),
            Err(ClientError::InvalidPath { .. })
        ));
        assert!(matches!(
            tree.collapse("/root/file"),
            Err(ClientError::InvalidPath { .. })
        ));
    }
}
