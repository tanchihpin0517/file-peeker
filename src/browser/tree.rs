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

    /// Restarts at a new root, dropping everything the old one placed. The caller
    /// starts the listing, and aborts the tasks the old root left running.
    pub(crate) fn reroot(&mut self, root_path: String) {
        self.root_path = root_path;
        self.selected_index = None;
        self.restart();
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

#[cfg(test)]
mod tests {
    use super::{Expansion, Tree};
    use crate::{DirectoryEntry, EntryKind};

    fn entry(name: &str, navigable: bool) -> DirectoryEntry {
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

    fn names(tree: &Tree) -> Vec<&str> {
        tree.rows().iter().map(super::Row::name).collect()
    }

    fn placed(tree: &Tree) -> Vec<(&str, u16)> {
        tree.rows()
            .iter()
            .map(|row| (row.name(), row.depth()))
            .collect()
    }

    fn tree() -> Tree {
        let mut tree = Tree::new("/root".into());
        tree.extend_root(vec![
            entry("first", true),
            entry("second", true),
            entry("note.txt", false),
        ]);
        tree.finish_root(Ok(()));
        tree
    }

    /// The insertion point is carried across groups, so a second group lands
    /// after the first rather than on top of it.
    #[test]
    fn groups_of_one_expansion_land_in_order() {
        let mut tree = tree();
        assert_eq!(tree.begin_expansion(0, 1), "/root/first");
        tree.extend_child(1, vec![entry("a.txt", false)]);
        tree.extend_child(1, vec![entry("b.txt", false)]);
        tree.finish_child(1, Ok(()));

        assert_eq!(
            names(&tree),
            ["first", "a.txt", "b.txt", "second", "note.txt"]
        );
        assert_eq!(
            tree.rows()[0].expansion(),
            &Expansion::Expanded { request_id: 1 }
        );
    }

    /// A second expansion further down keeps its own children when the first one
    /// is still streaming into the rows above it.
    #[test]
    fn an_earlier_expansion_pushes_a_later_one_down() {
        let mut tree = tree();
        tree.begin_expansion(0, 1);
        tree.extend_child(1, vec![entry("a.txt", false)]);
        // `second` has moved to row 2 by now.
        assert_eq!(tree.rows()[2].name(), "second");
        tree.begin_expansion(2, 2);
        tree.extend_child(1, vec![entry("b.txt", false)]);
        tree.extend_child(2, vec![entry("c.txt", false)]);

        assert_eq!(
            names(&tree),
            ["first", "a.txt", "b.txt", "second", "c.txt", "note.txt"]
        );
    }

    /// A root group arriving at the very index an expansion's next group is due
    /// at is a sibling of that expansion's parent, not of its children, so the
    /// children still land ahead of it. Depth is the only thing that tells the
    /// two apart.
    #[test]
    fn a_root_group_lands_after_the_subtree_of_a_directory_expanded_mid_stream() {
        let mut tree = Tree::new("/root".into());
        tree.extend_root(vec![entry("dir", true)]);
        tree.begin_expansion(0, 1);
        tree.extend_child(1, vec![entry("a.txt", false)]);
        // The root listing is still streaming, and its next group appends at the
        // index the expansion is about to fill.
        tree.extend_root(vec![entry("sibling.txt", false)]);
        tree.extend_child(1, vec![entry("b.txt", false)]);

        assert_eq!(
            placed(&tree),
            [("dir", 0), ("a.txt", 1), ("b.txt", 1), ("sibling.txt", 0)]
        );
    }

    /// The mirror case: rows deeper than an expansion's children belong inside
    /// the subtree they landed in, so the parent's next group goes after them.
    #[test]
    fn a_nested_expansions_children_land_ahead_of_its_parents_later_groups() {
        let mut tree = Tree::new("/root".into());
        tree.extend_root(vec![entry("dir", true)]);
        tree.begin_expansion(0, 1);
        tree.extend_child(1, vec![entry("nested", true)]);
        tree.begin_expansion(1, 2);
        tree.extend_child(2, vec![entry("deep.txt", false)]);
        tree.extend_child(1, vec![entry("later.txt", false)]);

        assert_eq!(
            placed(&tree),
            [("dir", 0), ("nested", 1), ("deep.txt", 2), ("later.txt", 1)]
        );
    }

    /// Collapsing takes the placements inside the subtree with it, so groups
    /// already in flight for them are dropped rather than misplaced.
    #[test]
    fn collapsing_drops_the_expansions_inside_it() {
        let mut tree = tree();
        tree.begin_expansion(0, 1);
        tree.extend_child(1, vec![entry("nested", true)]);
        tree.begin_expansion(1, 2);

        // Collapsing `first` takes `nested` with it, and reports both so their
        // listing tasks can be aborted.
        assert_eq!(tree.collapse(0), [1, 2]);
        tree.extend_child(2, vec![entry("ghost.txt", false)]);
        assert_eq!(names(&tree), ["first", "second", "note.txt"]);
    }

    /// A collapse above a still-streaming expansion walks its insertion point
    /// back by what was removed, so its next group still lands under it.
    #[test]
    fn collapsing_above_a_live_expansion_walks_its_insertion_point_back() {
        let mut tree = tree();
        tree.begin_expansion(0, 1);
        tree.extend_child(1, vec![entry("a.txt", false), entry("b.txt", false)]);
        tree.finish_child(1, Ok(()));
        // `second` sits at row 3 now, behind the two children of `first`.
        tree.begin_expansion(3, 2);
        tree.extend_child(2, vec![entry("c.txt", false)]);

        tree.collapse(0);
        tree.extend_child(2, vec![entry("d.txt", false)]);

        assert_eq!(
            placed(&tree),
            [
                ("first", 0),
                ("second", 0),
                ("c.txt", 1),
                ("d.txt", 1),
                ("note.txt", 0)
            ]
        );
    }

    #[test]
    fn a_filter_keeps_matches_and_their_ancestors() {
        let mut tree = tree();
        tree.begin_expansion(0, 1);
        tree.extend_child(1, vec![entry("target.txt", false)]);
        tree.finish_child(1, Ok(()));

        let window = tree.window("target", None);
        assert_eq!(window.len(), 2);
        // The directory is carried along only to place the match under it.
        assert_eq!(window.row(0), Some(0));
        assert!(!window.matched(0));
        assert_eq!(window.row(1), Some(1));
        assert!(window.matched(1));
        assert_eq!(window.matches(), Some(1));
    }

    /// A scope confines matching to one directory, and keeps that directory on
    /// screen whatever the query does.
    #[test]
    fn a_scope_confines_matching_to_the_directory_it_names() {
        let mut tree = tree();
        tree.begin_expansion(0, 1);
        tree.extend_child(1, vec![entry("note.txt", false)]);
        tree.finish_child(1, Ok(()));

        // Two rows are named `note.txt`; only the one inside `first` is in scope.
        let window = tree.window("note", Some("/root/first"));
        assert_eq!(window.matches(), Some(1));
        assert_eq!(window.len(), 2);
        assert_eq!(window.row(0), Some(0));
        assert!(!window.matched(0));
        assert_eq!(window.row(1), Some(1));
    }
}
