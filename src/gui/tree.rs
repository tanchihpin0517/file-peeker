//! One browsed root: a flat pre-order list of rows, expanded in place as
//! listings stream in.
//!
//! This is the same model the TUI's
//! [`BrowserContext`](crate::tui::browser_context) holds, minus everything to do
//! with a terminal viewport. The duplication is deliberate and temporary: the
//! shared parts want extracting into a headless `browser` layer that both front
//! ends consume, and doing that is a change to the TUI rather than an addition
//! to this one.
//!
//! The one invariant worth restating here, because getting it wrong is what made
//! a large expansion stall: **the insertion point of an in-flight expansion is
//! carried forward, never recomputed.** Walking to the end of a subtree once per
//! arriving group is quadratic in the size of the directory.

use std::collections::HashMap;

use crate::DirectoryEntry;

/// How far a row is from the root. `u16` rather than `usize` so the indent
/// arithmetic converts to `f32` without a lossy cast.
pub(crate) type Depth = u16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Expansion {
    /// Not a directory.
    None,
    Collapsed,
    Loading,
    Expanded,
    Failed,
}

#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) depth: Depth,
    pub(crate) entry: DirectoryEntry,
    pub(crate) expansion: Expansion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Status {
    Loading,
    Complete,
    Failed(String),
}

/// Where a still-loading expansion's next group of children goes.
#[derive(Clone, Copy, Debug)]
struct Placement {
    parent: usize,
    depth: Depth,
    insertion: usize,
}

pub(crate) struct Tree {
    pub(crate) root: String,
    rows: Vec<Row>,
    /// In-flight expansions, keyed by the request that will fill them.
    placements: HashMap<u64, Placement>,
    status: Status,
    selected: usize,
    filter: String,
    /// Rows a filter leaves on screen: the matches plus the ancestors that place
    /// them. `None` means every row.
    window: Option<Vec<(usize, bool)>>,
    matches: usize,
    stale: bool,
}

impl Tree {
    pub(crate) fn new(root: String) -> Self {
        Self {
            root,
            rows: Vec::new(),
            placements: HashMap::new(),
            status: Status::Loading,
            selected: 0,
            filter: String::new(),
            window: None,
            matches: 0,
            stale: true,
        }
    }

    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub(crate) fn status(&self) -> &Status {
        &self.status
    }

    pub(crate) fn loading(&self) -> bool {
        self.status == Status::Loading || !self.placements.is_empty()
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn select(&mut self, row: usize) {
        self.selected = row.min(self.rows.len().saturating_sub(1));
    }

    pub(crate) fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    pub(crate) fn filter(&self) -> &str {
        &self.filter
    }

    pub(crate) fn set_filter(&mut self, filter: String) {
        if filter != self.filter {
            self.filter = filter;
            self.stale = true;
        }
    }

    /// Restarts at a new root, dropping everything the old one placed.
    pub(crate) fn reroot(&mut self, root: String) {
        self.root = root;
        self.rows.clear();
        self.placements.clear();
        self.status = Status::Loading;
        self.selected = 0;
        self.stale = true;
    }

    // ── the listing arriving ────────────────────────────────────────────────

    /// Places one group of root entries.
    pub(crate) fn extend_root(&mut self, entries: Vec<DirectoryEntry>) {
        let at = self.rows.len();
        let root = self.root.clone();
        self.rows.extend(entries.into_iter().map(|entry| Row {
            path: child_path(&root, &entry.name),
            name: entry.name.clone(),
            depth: 0,
            expansion: expansion_for(&entry),
            entry,
        }));
        // Root entries land past every subtree, so no placement moves; the call
        // is here so that every insertion into `rows` reports itself.
        self.shift_placements(at, 0, 0);
        self.stale = true;
    }

    pub(crate) fn finish_root(&mut self, result: Result<(), String>) {
        self.status = match result {
            Ok(()) => Status::Complete,
            Err(error) => Status::Failed(error),
        };
        self.stale = true;
    }

    /// Places one group of a directory's children beneath it.
    pub(crate) fn extend_child(&mut self, request: u64, entries: Vec<DirectoryEntry>) {
        let Some(placement) = self.placements.get(&request).copied() else {
            // The expansion was collapsed, or its tree replaced, while this
            // group was already on the channel.
            return;
        };
        let count = entries.len();
        let at = placement.insertion;
        let parent_path = self.rows[placement.parent].path.clone();
        let rows = entries.into_iter().map(|entry| Row {
            path: child_path(&parent_path, &entry.name),
            name: entry.name.clone(),
            depth: placement.depth,
            expansion: expansion_for(&entry),
            entry,
        });
        self.rows.splice(at..at, rows);
        if self.selected >= at {
            self.selected += count;
        }
        self.shift_placements(at, count, placement.depth);
        self.stale = true;
    }

    pub(crate) fn finish_child(&mut self, request: u64, result: &Result<(), String>) {
        let Some(placement) = self.placements.remove(&request) else {
            return;
        };
        self.rows[placement.parent].expansion = if result.is_ok() {
            Expansion::Expanded
        } else {
            Expansion::Failed
        };
        self.stale = true;
    }

    // ── expansion ───────────────────────────────────────────────────────────

    /// Marks a row as loading and reports the path to list, or `None` when the
    /// row is not a directory or was already open (in which case it collapses).
    pub(crate) fn toggle(&mut self, index: usize, request: u64) -> Option<String> {
        let row = self.rows.get(index)?;
        if !row.entry.navigable {
            return None;
        }
        let depth = row.depth;
        match row.expansion {
            Expansion::Collapsed | Expansion::Failed => {
                self.rows[index].expansion = Expansion::Loading;
                self.placements.insert(
                    request,
                    Placement {
                        parent: index,
                        // A row is only expanded from collapsed, so it has no
                        // children yet and the first group goes right beneath it.
                        insertion: index + 1,
                        depth: depth + 1,
                    },
                );
                Some(self.rows[index].path.clone())
            }
            Expansion::Loading | Expansion::Expanded => {
                self.collapse(index);
                None
            }
            Expansion::None => None,
        }
    }

    fn collapse(&mut self, index: usize) {
        let end = self.subtree_end(index);
        // Every expansion inside the subtree goes with it, which is what leaves
        // the surviving placements all outside the removed rows.
        self.placements
            .retain(|_, placement| !(index..end).contains(&placement.parent));
        self.rows.drain(index + 1..end);
        self.unshift_placements(index + 1, end);
        self.rows[index].expansion = Expansion::Collapsed;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.stale = true;
    }

    fn subtree_end(&self, index: usize) -> usize {
        let depth = self.rows[index].depth;
        self.rows[index + 1..]
            .iter()
            .position(|row| row.depth <= depth)
            .map_or(self.rows.len(), |offset| index + 1 + offset)
    }

    /// Keeps in-flight placements pointing at the same rows after `count` rows
    /// at `depth` land at `at`.
    ///
    /// Rows arriving exactly where a group is due belong inside that subtree
    /// only when they are deeper than its children's parent -- a still-streaming
    /// parent inserts its own next group at that very index.
    fn shift_placements(&mut self, at: usize, count: usize, depth: Depth) {
        for placement in self.placements.values_mut() {
            if at <= placement.parent {
                placement.parent += count;
                placement.insertion += count;
            } else if at < placement.insertion
                || (at == placement.insertion && depth >= placement.depth)
            {
                placement.insertion += count;
            }
        }
    }

    /// The same, after the rows in `start..end` are removed.
    fn unshift_placements(&mut self, start: usize, end: usize) {
        let count = end - start;
        for placement in self.placements.values_mut() {
            if placement.parent >= end {
                placement.parent -= count;
                placement.insertion -= count;
            } else if placement.insertion >= end {
                placement.insertion -= count;
            } else if placement.insertion > start {
                placement.insertion = start;
            }
        }
    }

    // ── the filter window ───────────────────────────────────────────────────

    /// How many rows the visible window holds.
    pub(crate) fn visible_len(&mut self) -> usize {
        if self.stale {
            self.recompute();
        }
        self.window.as_ref().map_or(self.rows.len(), Vec::len)
    }

    /// The row at a position in the visible window, and whether it matched.
    pub(crate) fn at(&mut self, position: usize) -> Option<(usize, bool)> {
        if self.stale {
            self.recompute();
        }
        match &self.window {
            Some(window) => window.get(position).copied(),
            None => (position < self.rows.len()).then_some((position, true)),
        }
    }

    pub(crate) fn matches(&mut self) -> Option<usize> {
        if self.stale {
            self.recompute();
        }
        self.window.as_ref().map(|_| self.matches)
    }

    fn recompute(&mut self) {
        self.stale = false;
        if self.filter.is_empty() {
            self.window = None;
            return;
        }
        let needle = self.filter.to_lowercase();
        let mut window = Vec::new();
        let mut ancestors: Vec<(usize, bool)> = Vec::new();
        self.matches = 0;
        for (index, row) in self.rows.iter().enumerate() {
            ancestors.truncate(usize::from(row.depth));
            let hit = row.name.to_lowercase().contains(&needle);
            if hit {
                for (ancestor, emitted) in &mut ancestors {
                    if !*emitted {
                        window.push((*ancestor, false));
                        *emitted = true;
                    }
                }
                window.push((index, true));
                self.matches += 1;
            }
            ancestors.push((index, hit));
        }
        self.window = Some(window);
    }
}

fn expansion_for(entry: &DirectoryEntry) -> Expansion {
    if entry.navigable {
        Expansion::Collapsed
    } else {
        Expansion::None
    }
}

fn child_path(parent: &str, name: &str) -> String {
    std::path::Path::new(parent)
        .join(name)
        .to_string_lossy()
        .into_owned()
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
        assert_eq!(tree.toggle(0, 1).as_deref(), Some("/root/first"));
        tree.extend_child(1, vec![entry("a.txt", false)]);
        tree.extend_child(1, vec![entry("b.txt", false)]);
        tree.finish_child(1, &Ok(()));

        assert_eq!(
            tree.rows()
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "a.txt", "b.txt", "second", "note.txt"]
        );
        assert_eq!(tree.rows()[0].expansion, Expansion::Expanded);
    }

    /// A second expansion further down keeps its own children when the first one
    /// is still streaming into the rows above it.
    #[test]
    fn an_earlier_expansion_pushes_a_later_one_down() {
        let mut tree = tree();
        tree.toggle(0, 1);
        tree.extend_child(1, vec![entry("a.txt", false)]);
        // `second` has moved to row 2 by now.
        assert_eq!(tree.rows()[2].name, "second");
        tree.toggle(2, 2);
        tree.extend_child(1, vec![entry("b.txt", false)]);
        tree.extend_child(2, vec![entry("c.txt", false)]);

        assert_eq!(
            tree.rows()
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "a.txt", "b.txt", "second", "c.txt", "note.txt"]
        );
    }

    /// Collapsing takes the placements inside the subtree with it, so groups
    /// already in flight for them are dropped rather than misplaced.
    #[test]
    fn collapsing_drops_the_expansions_inside_it() {
        let mut tree = tree();
        tree.toggle(0, 1);
        tree.extend_child(1, vec![entry("nested", true)]);
        tree.toggle(1, 2);
        tree.toggle(0, 3); // collapses `first`, taking `nested` with it

        tree.extend_child(2, vec![entry("ghost.txt", false)]);
        assert_eq!(
            tree.rows()
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second", "note.txt"]
        );
    }

    #[test]
    fn a_filter_keeps_matches_and_their_ancestors() {
        let mut tree = tree();
        tree.toggle(0, 1);
        tree.extend_child(1, vec![entry("target.txt", false)]);
        tree.finish_child(1, &Ok(()));

        tree.set_filter("target".into());
        assert_eq!(tree.visible_len(), 2);
        // The directory is carried along only to place the match under it.
        assert_eq!(tree.at(0), Some((0, false)));
        assert_eq!(tree.at(1), Some((1, true)));
        assert_eq!(tree.matches(), Some(1));
    }
}
