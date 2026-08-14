//! The rows of one browsed root, and the in-flight expansions filling them.

use std::collections::HashMap;

use super::{
    Depth, Expansion, Row, Status, child_path,
    window::{VisibleRow, VisibleRows, in_scope, matches_needle},
};
use crate::DirectoryEntry;

/// Where a still-loading expansion's next group of children lands.
///
/// Children always land at the end of their own subtree, so the position is
/// carried forward from group to group rather than searched for again.
/// Recomputing it meant walking the children already placed, once per group,
/// which is quadratic in the size of the directory -- the stall a large expansion
/// showed while a change of directory of the same size did not, since a root
/// listing only ever appends.
///
/// Row indices, so every insertion and removal elsewhere in the tree has to shift
/// them. That is what [`Tree::placements_after_insertion`] and
/// [`Tree::placements_after_removal`] are for, and they are the reason those are
/// the only two places rows may move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Placement {
    /// The row being expanded.
    parent: usize,
    /// The depth its children sit at.
    depth: Depth,
    /// The row index the next group is inserted at.
    insertion: usize,
}

/// One browsed root: rows in pre-order, plus the expansions still streaming into
/// them.
///
/// Nothing here knows how tall a frame is or how to reach a server. The owner
/// starts a listing, feeds each group in with [`Tree::extend_root`] or
/// [`Tree::extend_child`], and closes it with the matching `finish`.
#[derive(Debug)]
pub(crate) struct Tree {
    root_path: String,
    rows: Vec<Row>,
    /// In-flight expansions, keyed by the request that will fill them.
    placements: HashMap<u64, Placement>,
    selected_index: Option<usize>,
    status: Status,
}

impl Tree {
    pub(crate) fn new(root_path: String) -> Self {
        Self {
            root_path,
            rows: Vec::new(),
            placements: HashMap::new(),
            selected_index: None,
            status: Status::Loading,
        }
    }

    pub(crate) fn root_path(&self) -> &str {
        &self.root_path
    }

    pub(crate) fn set_root_path(&mut self, root_path: String) {
        self.root_path = root_path;
    }

    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub(crate) fn status(&self) -> &Status {
        &self.status
    }

    // ── selection ───────────────────────────────────────────────────────────

    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    pub(crate) fn set_selected_index(&mut self, index: Option<usize>) {
        self.selected_index = index;
    }

    /// Where the selection actually sits, which is the first row until one is
    /// chosen and the last row once the tree has shrunk past it.
    pub(crate) fn effective_selection(&self) -> Option<usize> {
        (!self.rows.is_empty()).then(|| self.selected_index.unwrap_or(0).min(self.rows.len() - 1))
    }

    pub(crate) fn selected_row(&self) -> Option<&Row> {
        self.effective_selection()
            .and_then(|index| self.rows.get(index))
    }

    pub(crate) fn clamp_selection(&mut self) {
        self.selected_index = match (self.selected_index, self.rows.len()) {
            (_, 0) => None,
            (Some(index), length) => Some(index.min(length - 1)),
            (None, _) => Some(0),
        };
    }

    // ── the listing arriving ────────────────────────────────────────────────

    /// Drops every row and every in-flight expansion, ready for a fresh root
    /// listing. The caller aborts the listing tasks, which it is holding anyway
    /// because a restart takes all of them.
    ///
    /// The selection is left alone: a refresh that finds the same rows should not
    /// move it.
    pub(crate) fn restart(&mut self) {
        self.rows.clear();
        self.placements.clear();
        self.status = Status::Loading;
    }

    /// Places one group of root entries.
    pub(crate) fn extend_root(&mut self, entries: Vec<DirectoryEntry>) {
        let insertion_index = self.rows.len();
        let count = entries.len();
        let root_path = self.root_path.clone();
        self.rows.extend(entries.into_iter().map(|entry| {
            let path = child_path(&root_path, &entry.name);
            Row::new(path, 0, entry)
        }));
        // Root entries land past every expansion's subtree, so this moves
        // nothing. It is called anyway, so that every insertion into the tree
        // reports itself.
        self.placements_after_insertion(insertion_index, count, 0);
        if self.selected_index.is_none() && !self.rows.is_empty() {
            self.selected_index = Some(0);
        }
    }

    pub(crate) fn finish_root(&mut self, result: Result<(), String>) {
        self.status = match result {
            Ok(()) => Status::Complete,
            Err(error) => Status::Failed(error),
        };
        self.clamp_selection();
    }

    /// The row an in-flight expansion is filling, for the caller to check a
    /// listing against before it feeds the group in.
    pub(crate) fn expanding_path(&self, request_id: u64) -> Option<&str> {
        let placement = self.placements.get(&request_id)?;
        self.rows.get(placement.parent).map(Row::path)
    }

    /// Places one group of a directory's children beneath it.
    ///
    /// A request with no placement is no longer in flight: it was collapsed, or
    /// the tree it belonged to was replaced, and its group was already on the
    /// channel when that happened.
    pub(crate) fn extend_child(&mut self, request_id: u64, entries: Vec<DirectoryEntry>) {
        let Some(placement) = self.placements.get(&request_id).copied() else {
            return;
        };
        let count = entries.len();
        let insertion_index = placement.insertion;
        if let Some(selected_index) = self.selected_index
            && insertion_index <= selected_index
        {
            self.selected_index = Some(selected_index + count);
        }
        let parent_path = self.rows[placement.parent].path().to_owned();
        let rows = entries.into_iter().map(|entry| {
            let path = child_path(&parent_path, &entry.name);
            Row::new(path, placement.depth, entry)
        });
        self.rows.splice(insertion_index..insertion_index, rows);
        self.placements_after_insertion(insertion_index, count, placement.depth);
    }

    pub(crate) fn finish_child(&mut self, request_id: u64, result: Result<(), String>) {
        let Some(placement) = self.placements.remove(&request_id) else {
            return;
        };
        self.rows[placement.parent].expansion = match result {
            Ok(()) => Expansion::Expanded { request_id },
            Err(error) => Expansion::Failed { request_id, error },
        };
        self.clamp_selection();
    }

    // ── expansion ───────────────────────────────────────────────────────────

    /// Marks a row as loading and reports the path to list.
    ///
    /// The caller has already decided the row is worth expanding; a row that is
    /// not [`DirectoryEntry::navigable`] never reaches here.
    pub(crate) fn begin_expansion(&mut self, index: usize, request_id: u64) -> String {
        self.rows[index].expansion = Expansion::Loading { request_id };
        self.placements.insert(
            request_id,
            Placement {
                parent: index,
                // A row is only expanded from collapsed, so it has no children
                // yet and the first group goes directly beneath it.
                depth: self.rows[index].depth().saturating_add(1),
                insertion: index + 1,
            },
        );
        self.rows[index].path().to_owned()
    }

    /// Closes a row's subtree, and reports the expansions that went with it so
    /// the caller can abort their tasks.
    pub(crate) fn collapse(&mut self, index: usize) -> Vec<u64> {
        let end = self.subtree_end(index);
        let request_ids: Vec<_> = self.rows[index..end]
            .iter()
            .filter_map(|row| row.expansion().request_id())
            .collect();
        // Every expansion inside the subtree goes with it, which is what leaves
        // the placements still standing all outside the removed rows.
        let dropped = request_ids
            .into_iter()
            .filter(|request_id| self.placements.remove(request_id).is_some())
            .collect();
        self.rows.drain(index + 1..end);
        self.placements_after_removal(index + 1, end);
        self.rows[index].expansion = Expansion::Collapsed;
        self.selected_index = Some(index);
        dropped
    }

    fn subtree_end(&self, index: usize) -> usize {
        let depth = self.rows[index].depth();
        self.rows[index + 1..]
            .iter()
            .position(|row| row.depth() <= depth)
            .map_or(self.rows.len(), |offset| index + 1 + offset)
    }

    /// Keeps every in-flight expansion pointing at the rows it was pointing at,
    /// after `count` rows at depth `depth` land at `insertion_index`.
    ///
    /// Rows landing exactly where an expansion's next group goes are the one case
    /// an index cannot settle on its own: they belong inside that subtree when
    /// they are a descendant of it, and follow it when they are a sibling of its
    /// children -- which a still-streaming parent listing inserts at the very same
    /// index. Depth is what tells the two apart.
    fn placements_after_insertion(&mut self, insertion_index: usize, count: usize, depth: Depth) {
        for placement in self.placements.values_mut() {
            if insertion_index <= placement.parent {
                placement.parent += count;
                placement.insertion += count;
            } else if insertion_index < placement.insertion
                || (insertion_index == placement.insertion && depth >= placement.depth)
            {
                placement.insertion += count;
            }
        }
    }

    /// Keeps every in-flight expansion pointing at the rows it was pointing at,
    /// after the rows in `start..end` are removed. Expansions inside that range
    /// are the caller's to drop first.
    fn placements_after_removal(&mut self, start: usize, end: usize) {
        let count = end - start;
        for placement in self.placements.values_mut() {
            if placement.parent >= end {
                placement.parent -= count;
                placement.insertion -= count;
            } else if placement.insertion >= end {
                placement.insertion -= count;
            } else if placement.insertion > start {
                // The removed rows were inside this subtree, which now ends where
                // they began.
                placement.insertion = start;
            }
        }
    }

    // ── the filter window ───────────────────────────────────────────────────

    /// The rows a query leaves on screen: every name that matches, plus the
    /// ancestors of each match, so a hit inside a deep directory still reads as
    /// being inside it. `scope` confines matching to one directory, and stays on
    /// screen itself whatever the query does.
    ///
    /// Recomputed per call rather than cached. That costs one pass over the tree
    /// per keystroke and per frame drawn while filtering, which is the same order
    /// as the matching itself, and it means no row mutation anywhere else has to
    /// remember to invalidate anything. An unfiltered browser pays nothing.
    pub(crate) fn window(&self, query: &str, scope: Option<&str>) -> VisibleRows {
        let needle = (!query.is_empty()).then(|| query.to_lowercase());
        if needle.is_none() && scope.is_none() {
            return VisibleRows::All(self.rows.len());
        }
        let mut rows = Vec::new();
        let mut matches = 0;
        // The ancestor chain of the row under consideration, and whether each link
        // has been emitted already. Rows are in pre-order, so truncating to the
        // current depth leaves exactly that row's ancestors.
        let mut ancestors: Vec<(usize, bool)> = Vec::new();
        for (index, row) in self.rows.iter().enumerate() {
            ancestors.truncate(usize::from(row.depth()));
            // The directory a `/` stepped into stays on screen whatever the query
            // does, so stepping into an empty or still-loading one still shows
            // where the prompt is pointed.
            let entered = scope == Some(row.path());
            let hit = !entered
                && in_scope(row.path(), scope)
                && needle
                    .as_ref()
                    .is_none_or(|needle| matches_needle(row.name(), needle));
            if hit || entered {
                for (ancestor, emitted) in &mut ancestors {
                    if !*emitted {
                        rows.push(VisibleRow {
                            row: *ancestor,
                            matched: false,
                        });
                        *emitted = true;
                    }
                }
                rows.push(VisibleRow {
                    row: index,
                    matched: hit,
                });
                matches += usize::from(hit);
            }
            ancestors.push((index, hit || entered));
        }
        VisibleRows::Filtered { rows, matches }
    }

    /// Every row, for a browser with no filter open at all.
    pub(crate) fn unfiltered(&self) -> VisibleRows {
        VisibleRows::All(self.rows.len())
    }
}
