//! One browsed root as the terminal sees it: a [`Tree`] of rows, the viewport
//! that frames them, and the filter prompt that narrows them.
//!
//! The rows themselves, their expansions and the filter window all live in
//! [`crate::browser`], which the desktop front end drives too. What is here is
//! everything the terminal adds: a viewport whose height only render time knows,
//! a prompt driven one keystroke at a time, and the listing tasks feeding the
//! tree.

use std::{collections::HashMap, fmt, path::Path, sync::Arc};

use crate::{
    Client, ConnectionId, DirectoryEntry, EntryKind,
    browser::{Tree, VisibleRows, in_scope, matches_needle},
    host_path::HostPath,
};
use futures::StreamExt as _;
use tokio::{sync::mpsc, task::JoinHandle};
use uuid::Uuid;

pub(crate) use crate::browser::{
    Expansion as ExpansionStatus, Row as BrowserRow, Status as ListingStatus,
};

/// Rows of context kept past the selection when the viewport scrolls to follow
/// it.
const SCROLL_PADDING: usize = 1;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct BrowserContextId(Uuid);

impl BrowserContextId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// The rows one frame displays, and where the selection sits inside them.
///
/// A filter leaves gaps, so this is a list of borrows rather than a slice. It is
/// still one entry per row on screen, which is what bounds a frame's cost.
#[derive(Debug)]
pub(crate) struct Viewport<'a> {
    pub(crate) rows: Vec<&'a BrowserRow>,
    pub(crate) selected: Option<usize>,
    /// How many rows the query matched, from the same pass that placed the window.
    /// Asking separately would walk the whole tree a second time per frame.
    pub(crate) matches: Option<usize>,
}

/// A live filter over the rows, the directory it has stepped into, and the
/// selection to put back if it is abandoned.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Filter {
    /// What the user typed, and what narrows the rows. The selected candidate is
    /// spelled out over it rather than into it, so walking the candidates never
    /// shrinks the set being walked.
    query: String,
    /// The name of the selected match, spelled out in the prompt in place of the
    /// query. Cleared by anything that edits the query.
    ///
    /// The name as a person reads it, matching what the query is compared against.
    filled: Option<String>,
    /// What the last `/` refused to do, shown in the prompt until the next keystroke
    /// so the key never fails silently.
    warning: Option<&'static str>,
    /// The directories a `/` has stepped into, outermost first. The innermost is
    /// what matching is confined to, which is what makes an empty query read as
    /// "everything in here".
    descents: Vec<Descent>,
    /// Held as a path rather than an index, because rows inserted while the prompt
    /// is open -- by a streaming listing, or by the expansion a `/` starts -- would
    /// drift an index out from under it.
    restore_path: Option<HostPath>,
}

impl Filter {
    fn scope(&self) -> Option<&HostPath> {
        self.descents.last().map(|descent| &descent.path)
    }

    /// What the prompt shows: the selected candidate when one has been spelled out,
    /// and otherwise the query itself.
    fn displayed(&self) -> &str {
        self.filled.as_deref().unwrap_or(&self.query)
    }

    /// Folds a spelled-out candidate back into the query, so editing edits the text
    /// that is on screen.
    fn adopt_filled(&mut self) {
        if let Some(filled) = self.filled.take() {
            self.query = filled;
        }
        self.warning = None;
    }
}

/// A directory a `/` stepped into, and what stepping back out of it takes: the
/// query that was spent entering it, and whether entering it is what opened it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Descent {
    path: HostPath,
    query: String,
    opened: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SelectedEntry {
    pub(crate) path: HostPath,
    pub(crate) name: HostPath,
    pub(crate) kind: EntryKind,
}

pub(crate) struct BrowserContext {
    id: BrowserContextId,
    tree: Tree,
    filter: Option<Filter>,
    /// A position in the visible rows, which is a row index only while no filter
    /// narrows them.
    viewport_offset: usize,
    viewport_height: usize,
    root_listing_task: Option<JoinHandle<()>>,
    /// The task filling each in-flight expansion. The tree holds where the
    /// children go; this holds what to abort when they are no longer wanted.
    listing_tasks: HashMap<u64, JoinHandle<()>>,
    generation: u64,
    next_request_id: u64,
    client: Arc<Client>,
    connection: ConnectionId,
    events: mpsc::Sender<BrowserContextEvent>,
}

impl fmt::Debug for BrowserContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserContext")
            .field("id", &self.id)
            .field("tree", &self.tree)
            .field("filter", &self.filter)
            .field("viewport_offset", &self.viewport_offset)
            .field("viewport_height", &self.viewport_height)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl BrowserContext {
    pub(crate) fn new(
        client: Arc<Client>,
        connection: ConnectionId,
        root_path: HostPath,
        events: mpsc::Sender<BrowserContextEvent>,
    ) -> Self {
        let mut context = Self {
            id: BrowserContextId::new(),
            tree: Tree::new(root_path),
            filter: None,
            viewport_offset: 0,
            viewport_height: 0,
            root_listing_task: None,
            listing_tasks: HashMap::new(),
            generation: 0,
            next_request_id: 0,
            client,
            connection,
            events,
        };
        context.start_root_listing();
        context
    }

    pub(crate) fn id(&self) -> BrowserContextId {
        self.id
    }

    pub(crate) fn root_path(&self) -> &HostPath {
        self.tree.root_path()
    }

    pub(crate) fn rows(&self) -> &[BrowserRow] {
        self.tree.rows()
    }

    #[cfg(test)]
    fn entries(&self) -> Vec<DirectoryEntry> {
        self.rows().iter().map(|row| row.entry().clone()).collect()
    }

    #[cfg(test)]
    fn row_names(&self) -> Vec<String> {
        self.rows()
            .iter()
            .map(|row| row.name().display().into_owned())
            .collect()
    }

    /// The names one frame of `height` rows puts on screen.
    #[cfg(test)]
    fn visible_names(&mut self, height: usize) -> Vec<String> {
        self.viewport(height)
            .rows
            .iter()
            .map(|row| row.name().display().into_owned())
            .collect()
    }

    pub(crate) fn status(&self) -> &ListingStatus {
        self.tree.status()
    }

    pub(crate) fn selected_expansion_error(&self) -> Option<(&HostPath, &str)> {
        let row = self.tree.selected_row()?;
        match row.expansion() {
            ExpansionStatus::Failed { error, .. } => Some((row.path(), error)),
            _ => None,
        }
    }

    pub(crate) fn effective_selection(&self) -> Option<usize> {
        self.tree.effective_selection()
    }

    pub(crate) fn selected_entry(&self) -> Option<SelectedEntry> {
        let row = self.tree.selected_row()?;
        Some(SelectedEntry {
            path: row.path().to_owned(),
            name: row.name().to_owned(),
            kind: row.entry().kind,
        })
    }

    #[cfg(test)]
    pub(crate) fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    #[cfg(test)]
    pub(crate) fn set_viewport(&mut self, offset: usize, height: usize) {
        self.viewport_offset = offset;
        self.viewport_height = height;
    }

    /// Fits the viewport to `height` rows and returns the visible slice, with the
    /// selection expressed relative to that slice.
    ///
    /// The renderer calls this once per frame, because the height is only known
    /// at render time and the offset this settles on is what the next half-page
    /// scroll moves from. Handing back a window rather than the whole tree is
    /// what keeps a frame's cost proportional to what is on screen: the renderer
    /// builds one list item per row it is given, so a scroll through a directory
    /// of any size costs the same as a scroll through a small one.
    pub(crate) fn viewport(&mut self, height: usize) -> Viewport<'_> {
        self.viewport_height = height;
        let visible = self.visible_rows();
        let matches = visible.matches();
        let Some(selected) = self
            .effective_selection()
            .filter(|_| height > 0)
            .and_then(|row| visible.position_of(row))
        else {
            return Viewport {
                rows: Vec::new(),
                selected: None,
                matches,
            };
        };

        // Keep a row of context past the selection wherever the list allows it,
        // so moving off the edge of the viewport scrolls before it has to.
        let padding = SCROLL_PADDING.min(height.saturating_sub(1) / 2);
        let max_offset = visible.len().saturating_sub(height);
        let mut offset = self
            .viewport_offset
            .min(max_offset)
            .min(selected.saturating_sub(padding));
        let bottom = (selected + padding).min(visible.len() - 1);
        if bottom >= offset + height {
            offset = bottom + 1 - height;
        }
        self.viewport_offset = offset;

        let end = (offset + height).min(visible.len());
        let rows = self.tree.rows();
        Viewport {
            rows: (offset..end)
                .filter_map(|position| visible.row(position))
                .filter_map(|row| rows.get(row))
                .collect(),
            selected: Some(selected - offset),
            matches,
        }
    }

    pub(crate) fn move_selection(&mut self, down: bool) {
        if self.rows().is_empty() {
            self.tree.set_selected_index(None);
            return;
        }
        let visible = self.visible_rows();
        let Some(current) = self
            .effective_selection()
            .and_then(|row| visible.position_of(row))
        else {
            return;
        };
        let last = visible.len() - 1;
        let position = if down {
            (current + 1).min(last)
        } else {
            current.saturating_sub(1)
        };
        self.tree.set_selected_index(visible.row(position));
        self.fill_from_selection();
    }

    pub(crate) fn scroll_half_page(&mut self, down: bool) {
        let visible = self.visible_rows();
        let Some(current) = self
            .effective_selection()
            .and_then(|row| visible.position_of(row))
        else {
            return;
        };
        if self.viewport_height == 0 {
            return;
        }

        let distance = (self.viewport_height / 2).max(1);
        let last = visible.len() - 1;
        let max_offset = visible.len().saturating_sub(self.viewport_height);
        let position = if down {
            self.viewport_offset = self
                .viewport_offset
                .saturating_add(distance)
                .min(max_offset);
            current.saturating_add(distance).min(last)
        } else {
            self.viewport_offset = self.viewport_offset.saturating_sub(distance);
            current.saturating_sub(distance)
        };
        self.tree.set_selected_index(visible.row(position));
        self.fill_from_selection();
    }

    /// The rows the active filter leaves on screen, or all of them when no prompt
    /// is open.
    fn visible_rows(&self) -> VisibleRows {
        match &self.filter {
            Some(filter) => self.tree.window(&filter.query, filter.scope()),
            None => self.tree.unfiltered(),
        }
    }

    /// What the prompt shows, which is the selected candidate once one has been
    /// spelled out and the typed query until then.
    pub(crate) fn filter(&self) -> Option<&str> {
        self.filter.as_ref().map(Filter::displayed)
    }

    /// The directory a `/` stepped into, relative to the root, which the prompt
    /// shows as the segments already committed.
    pub(crate) fn filter_scope(&self) -> Option<String> {
        let scope = self.filter.as_ref()?.scope()?;
        // Stripped on the escaped form, which has the components the real path has,
        // then decoded for the prompt to show.
        let relative = Path::new(scope.as_escaped())
            .strip_prefix(Path::new(self.tree.root_path().as_escaped()))
            .ok()
            .and_then(Path::to_str)
            .map(str::to_owned);
        Some(
            relative
                .map(HostPath::from_escaped_fragment)
                .as_ref()
                .unwrap_or(scope)
                .display()
                .into_owned(),
        )
    }

    /// What the last `/` refused to do, until the next keystroke clears it.
    pub(crate) fn filter_warning(&self) -> Option<&'static str> {
        self.filter.as_ref()?.warning
    }

    /// Whether the query is worth reporting a match count for. The empty one a `/`
    /// leaves behind narrows to a whole directory, which the prompt already says.
    pub(crate) fn filter_counts_matches(&self) -> bool {
        self.filter
            .as_ref()
            .is_some_and(|filter| !filter.query.is_empty())
    }

    /// How many rows the query matches. The renderer takes this from the viewport it
    /// already computed; asking here walks the tree again.
    #[cfg(test)]
    fn filter_matches(&self) -> Option<usize> {
        self.filter_counts_matches().then_some(())?;
        self.visible_rows().matches()
    }

    /// Opens the filter prompt, remembering the selected row so abandoning it can
    /// put the selection back.
    pub(crate) fn begin_filter(&mut self) {
        self.filter = Some(Filter {
            query: String::new(),
            filled: None,
            warning: None,
            descents: Vec::new(),
            restore_path: self.selected_path(),
        });
    }

    pub(crate) fn extend_filter(&mut self, character: char) {
        if let Some(filter) = &mut self.filter {
            filter.adopt_filled();
            filter.query.push(character);
            self.follow_filter();
        }
    }

    /// Deletes the last character typed, and when there is none, the last `/`.
    ///
    /// Backspace walks the prompt back keystroke by keystroke: over a query it
    /// deletes characters, over the `/` that stepped into a directory it steps back
    /// out of it and restores the query that entered it, and over the `/` that
    /// opened the prompt it closes the prompt.
    pub(crate) fn shorten_filter(&mut self) {
        let Some(filter) = &mut self.filter else {
            return;
        };
        filter.adopt_filled();
        if filter.query.pop().is_some() {
            self.follow_filter();
        } else if filter.descents.is_empty() {
            self.abandon_filter();
        } else {
            self.ascend_filter();
        }
    }

    /// Extends the query to the longest prefix every name it matches agrees on,
    /// the way a shell completes a partial word. Matches that agree on nothing
    /// further leave the query alone, so a completion never narrows what is on
    /// screen -- every match starts with the prefix it completes to.
    pub(crate) fn complete_filter(&mut self) {
        // Extends what is on screen, so walking onto a candidate and then completing
        // does not throw the candidate away and jump the prompt back to the fragment
        // that found it.
        let Some(filter) = &mut self.filter else {
            return;
        };
        filter.adopt_filled();
        if filter.query.is_empty() {
            return;
        }
        let needle = filter.query.to_lowercase();
        let scope = filter.scope();
        let mut matches = self
            .tree
            .rows()
            .iter()
            .filter(|row| {
                scope != Some(row.path())
                    && in_scope(row.path().as_escaped(), scope.map(HostPath::as_escaped))
            })
            .map(|row| row.name().display())
            .filter(|name| matches_needle(name, &needle));
        let Some(first) = matches.next() else {
            return;
        };
        let mut completion = first.into_owned();
        for name in matches {
            completion = completion
                .chars()
                .zip(name.chars())
                .take_while(|(candidate, other)| candidate == other)
                .map(|(candidate, _)| candidate)
                .collect();
            if completion.is_empty() {
                return;
            }
        }
        let Some(filter) = &mut self.filter else {
            return;
        };
        if completion.chars().count() <= filter.query.chars().count() {
            return;
        }
        filter.filled = None;
        filter.warning = None;
        filter.query = completion;
        self.follow_filter();
    }

    /// Steps into the selected directory: it is expanded if it was closed, the
    /// query is spent getting here, and matching is confined to what lies under it.
    ///
    /// The root never moves, so this opens a directory rather than navigating to
    /// one -- the tab, its path, and everything outside stay put, and the prompt
    /// reads as a path because each `/` commits one segment. A name can never
    /// contain a `/`, so nothing is lost by spending the key this way rather than
    /// on a query that could only ever match nothing.
    ///
    /// Only a directory the prompt names outright is opened. A query that has merely
    /// narrowed onto one is narrowing onto its neighbours just as much, so guessing
    /// at it would open the wrong directory as readily as the right one; the prompt
    /// says why it did nothing instead.
    pub(crate) fn descend_filter(&mut self) {
        let Some(filter) = &self.filter else {
            return;
        };
        if self
            .selected_candidate()
            .is_none_or(|name| name != filter.displayed())
        {
            self.warn_filter("no exact match to open");
            return;
        }
        let Some((index, path, closed)) = self
            .effective_selection()
            .and_then(|index| Some((index, self.tree.rows().get(index)?)))
            .filter(|(_, row)| row.entry().navigable)
            .map(|(index, row)| {
                (
                    index,
                    row.path().to_owned(),
                    matches!(
                        row.expansion(),
                        ExpansionStatus::Collapsed | ExpansionStatus::Failed { .. }
                    ),
                )
            })
        else {
            self.warn_filter("not a directory");
            return;
        };
        if closed {
            self.start_directory_listing(index);
        }
        if let Some(filter) = &mut self.filter {
            // What entered the directory is the text that was on screen, a candidate
            // walked onto included, so that is what the query spent here has to be:
            // a backspace over this `/` gives back the name, not the fragment that
            // was typed before the walk.
            filter.adopt_filled();
            let query = std::mem::take(&mut filter.query);
            filter.descents.push(Descent {
                path,
                query,
                opened: closed,
            });
        }
        self.follow_filter();
    }

    /// Clears the last entry in the prompt in one stroke, and closes the prompt once
    /// there is no entry left.
    ///
    /// An entry is a whole segment, which is what makes this the segment-sized
    /// counterpart of backspace: the query if there is one, and otherwise the
    /// directory the last `/` stepped into -- taking with it the query that entered
    /// it, because the directory is the entry rather than the separator in front of
    /// it. `/dir/` therefore clears to `/` in one press rather than back to `/dir`.
    ///
    /// What it clears is what is on screen, so a candidate walked onto goes with it
    /// rather than leaving the fragment that found it behind.
    pub(crate) fn clear_filter_entry(&mut self) {
        let Some(filter) = &mut self.filter else {
            return;
        };
        if !filter.displayed().is_empty() {
            filter.filled = None;
            filter.warning = None;
            filter.query.clear();
            self.follow_filter();
            return;
        }
        if filter.descents.is_empty() {
            self.abandon_filter();
            return;
        }
        self.ascend_filter();
        if let Some(filter) = &mut self.filter {
            filter.filled = None;
            filter.query.clear();
        }
        self.follow_filter();
    }

    /// Steps back out of the last directory a `/` entered, closing it again if
    /// entering it is what opened it, and restoring the query that entered it.
    fn ascend_filter(&mut self) {
        let Some(descent) = self.filter.as_mut().and_then(|filter| {
            let descent = filter.descents.pop()?;
            filter.query.clone_from(&descent.query);
            Some(descent)
        }) else {
            return;
        };
        self.close_descent(&descent);
        self.follow_filter();
    }

    /// Fills the selected match into the query, or closes the prompt when the query
    /// already names it.
    ///
    /// Two presses rather than one, because the first has something to say: a query
    /// that only narrowed to a row is not yet that row, and seeing it spelled out is
    /// what makes the second press a confirmation rather than a guess. A row the
    /// query did not match -- the directory a `/` stepped into, or an ancestor the
    /// arrows walked onto -- has nothing to fill in, so it is taken directly.
    pub(crate) fn submit_filter(&mut self) {
        let Some(filter) = &self.filter else {
            return;
        };
        if self
            .selected_candidate()
            .is_some_and(|name| name != filter.displayed())
        {
            self.fill_from_selection();
        } else {
            self.commit_filter();
        }
    }

    /// Spells the selected match out in the prompt, which is what makes walking the
    /// candidates read as choosing one. Matching still goes by what was typed, so a
    /// candidate spelled out over the query never shrinks the set being walked.
    ///
    /// Does nothing without a prompt, so every selection move can call it. A row the
    /// query did not match has no name to spell out and puts the query back on
    /// screen.
    fn fill_from_selection(&mut self) {
        if self.filter.is_none() {
            return;
        }
        let candidate = self.selected_candidate();
        if let Some(filter) = &mut self.filter {
            filter.filled = candidate;
            filter.warning = None;
        }
    }

    fn warn_filter(&mut self, warning: &'static str) {
        if let Some(filter) = &mut self.filter {
            filter.warning = Some(warning);
        }
    }

    /// The selected row's name, when the query is what put it on screen.
    fn selected_candidate(&self) -> Option<String> {
        let visible = self.visible_rows();
        self.effective_selection()
            .filter(|index| visible.matched(*index))
            .and_then(|index| self.tree.rows().get(index))
            .map(|row| row.name().display().into_owned())
    }

    /// Closes the prompt, keeping the row the filter led to selected and every
    /// directory it opened on the way there open.
    pub(crate) fn commit_filter(&mut self) {
        self.filter = None;
    }

    /// Closes the prompt and leaves the tree as the prompt found it: every directory
    /// it opened closes again, deepest first, and the selection goes back to the row
    /// it started on.
    pub(crate) fn abandon_filter(&mut self) {
        let Some(filter) = self.filter.take() else {
            return;
        };
        for descent in filter.descents.iter().rev() {
            self.close_descent(descent);
        }
        let restored = filter
            .restore_path
            .and_then(|path| self.tree.rows().iter().position(|row| *row.path() == path));
        self.tree.set_selected_index(restored);
    }

    fn close_descent(&mut self, descent: &Descent) {
        if descent.opened
            && let Some(index) = self
                .tree
                .rows()
                .iter()
                .position(|row| *row.path() == descent.path && row.entry().navigable)
        {
            self.collapse_directory(index);
        }
    }

    fn selected_path(&self) -> Option<HostPath> {
        self.tree.selected_row().map(|row| row.path().to_owned())
    }

    /// Moves the selection onto a row the query leaves on screen, so typing never
    /// leaves it pointing at something hidden. A query that matches nothing has
    /// nowhere to put it and leaves it where it was, ready for the next backspace
    /// to widen back onto it.
    fn follow_filter(&mut self) {
        let visible = self.visible_rows();
        let Some(position) = visible.match_position_of(self.tree.selected_index().unwrap_or(0))
        else {
            return;
        };
        self.tree.set_selected_index(visible.row(position));
    }

    pub(crate) fn refresh(&mut self) {
        self.start_root_listing();
    }

    pub(crate) fn enter_selected(&mut self) {
        let Some(path) = self
            .tree
            .selected_row()
            .filter(|row| row.entry().navigable)
            .map(|row| row.path().to_owned())
        else {
            return;
        };
        self.change_root_path(path);
    }

    pub(crate) fn leave(&mut self) {
        // Walked on the escaped form, which carries the same components the real path
        // does, so the parent of an escaped path is the escape of the real parent.
        let root = self.tree.root_path().as_escaped();
        let Some(parent) = Path::new(root).parent().and_then(Path::to_str) else {
            return;
        };
        let parent = if parent.is_empty() { "." } else { parent };
        self.change_root_path(HostPath::from_escaped_fragment(parent.to_owned()));
    }

    /// Expands or collapses the selected directory. Files have no activation
    /// behavior.
    pub(crate) fn activate_selected(&mut self) {
        let Some(index) = self.effective_selection() else {
            return;
        };
        if self.tree.rows()[index].entry().navigable {
            self.toggle_directory(index);
        }
    }

    pub(crate) fn apply(&mut self, event: BrowserContextEvent) {
        if event.context_id != self.id {
            return;
        }
        match event.kind {
            BrowserEvent::Listing {
                generation,
                target,
                result,
            } if generation == self.generation => self.apply_listing(target, result),
            BrowserEvent::Listing { .. } => {}
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.abort_tree_listings();
    }

    fn change_root_path(&mut self, root_path: HostPath) {
        self.tree.reroot(root_path);
        self.viewport_offset = 0;
        self.start_root_listing();
    }

    fn start_root_listing(&mut self) {
        self.abort_tree_listings();
        self.tree.restart();
        self.generation = self.generation.wrapping_add(1);
        let client = Arc::clone(&self.client);
        let connection = self.connection;
        let root_path = self.tree.root_path().to_owned();
        let context_id = self.id;
        let generation = self.generation;
        let events = self.events.clone();
        self.root_listing_task = Some(tokio::spawn(run_listing(
            client,
            connection,
            root_path,
            context_id,
            generation,
            ListingTarget::Root,
            events,
        )));
    }

    fn toggle_directory(&mut self, index: usize) {
        match self.tree.rows()[index].expansion() {
            ExpansionStatus::Collapsed => self.start_directory_listing(index),
            _ => self.collapse_directory(index),
        }
    }

    fn start_directory_listing(&mut self, index: usize) {
        let request_id = self.next_request_id();
        let path = self.tree.begin_expansion(index, request_id);
        let client = Arc::clone(&self.client);
        let connection = self.connection;
        let context_id = self.id;
        let generation = self.generation;
        let target = ListingTarget::Directory {
            parent_path: path.clone(),
            request_id,
        };
        let events = self.events.clone();
        let task = tokio::spawn(run_listing(
            client, connection, path, context_id, generation, target, events,
        ));
        self.listing_tasks.insert(request_id, task);
    }

    fn collapse_directory(&mut self, index: usize) {
        for request_id in self.tree.collapse(index) {
            if let Some(task) = self.listing_tasks.remove(&request_id) {
                task.abort();
            }
        }
    }

    fn apply_listing(&mut self, target: ListingTarget, result: ListingResult) {
        match target {
            ListingTarget::Root => self.apply_root_listing(result),
            ListingTarget::Directory {
                parent_path,
                request_id,
            } => self.apply_directory_listing(&parent_path, request_id, result),
        }
    }

    fn apply_root_listing(&mut self, result: ListingResult) {
        match result {
            ListingResult::Entries(entries) => self.tree.extend_root(entries),
            ListingResult::Finished => {
                self.tree.finish_root(Ok(()));
                self.root_listing_task = None;
            }
            ListingResult::Failed(error) => {
                self.tree.finish_root(Err(error));
                self.root_listing_task = None;
            }
        }
    }

    fn apply_directory_listing(
        &mut self,
        parent_path: &HostPath,
        request_id: u64,
        result: ListingResult,
    ) {
        // Nothing expanding under this request means it is no longer in flight: it
        // was collapsed, or the tree it belonged to was replaced, and its task was
        // aborted with an event already on the channel.
        if self.tree.expanding_path(request_id).is_none() {
            return;
        }
        debug_assert_eq!(
            self.tree.expanding_path(request_id),
            Some(parent_path),
            "a placement should follow the row it is expanding"
        );
        match result {
            ListingResult::Entries(entries) => self.tree.extend_child(request_id, entries),
            ListingResult::Finished => {
                self.listing_tasks.remove(&request_id);
                self.tree.finish_child(request_id, Ok(()));
            }
            ListingResult::Failed(error) => {
                self.listing_tasks.remove(&request_id);
                self.tree.finish_child(request_id, Err(error));
            }
        }
    }

    fn abort_tree_listings(&mut self) {
        if let Some(task) = self.root_listing_task.take() {
            task.abort();
        }
        for (_, task) in self.listing_tasks.drain() {
            task.abort();
        }
    }

    fn next_request_id(&mut self) -> u64 {
        self.next_request_id = self.next_request_id.wrapping_add(1);
        self.next_request_id
    }
}

impl Drop for BrowserContext {
    fn drop(&mut self) {
        self.abort_tree_listings();
    }
}

#[derive(Debug)]
pub(crate) struct BrowserContextEvent {
    context_id: BrowserContextId,
    kind: BrowserEvent,
}

impl BrowserContextEvent {
    pub(crate) fn context_id(&self) -> BrowserContextId {
        self.context_id
    }
}

#[derive(Debug)]
enum BrowserEvent {
    Listing {
        generation: u64,
        target: ListingTarget,
        result: ListingResult,
    },
}

#[derive(Clone, Debug)]
enum ListingTarget {
    Root,
    Directory {
        parent_path: HostPath,
        request_id: u64,
    },
}

#[derive(Debug)]
enum ListingResult {
    Entries(Vec<DirectoryEntry>),
    Finished,
    Failed(String),
}

/// How many entries one listing event may carry.
///
/// Entries are coalesced into an event per group that is *already there* rather
/// than per entry, which is what keeps placing them off the critical path: making
/// room for children ahead of the rows that follow them costs a move of everything
/// after, so paying that per entry is quadratic in the size of the tree. A group is
/// whatever the stream has ready, so a server that trickles still arrives entry by
/// entry and a directory that is sitting in the page cache arrives in a handful of
/// groups.
const LISTING_GROUP: usize = 512;

async fn run_listing(
    client: Arc<Client>,
    connection: ConnectionId,
    path: HostPath,
    context_id: BrowserContextId,
    generation: u64,
    target: ListingTarget,
    events: mpsc::Sender<BrowserContextEvent>,
) {
    let result = match client.op_list_dir(&connection, &path).await {
        Ok(listing) => {
            let mut groups = listing.ready_chunks(LISTING_GROUP);
            loop {
                match groups.next().await {
                    Some(group) => {
                        // A group ends at its first error, which is terminal for the
                        // whole listing, so the entries ahead of it still land.
                        let mut entries = Vec::with_capacity(group.len());
                        let mut failure = None;
                        for entry in group {
                            match entry {
                                Ok(entry) => entries.push(entry),
                                Err(error) => {
                                    failure = Some(error.to_string());
                                    break;
                                }
                            }
                        }
                        if !entries.is_empty()
                            && send_listing_result(
                                &events,
                                context_id,
                                generation,
                                target.clone(),
                                ListingResult::Entries(entries),
                            )
                            .await
                            .is_err()
                        {
                            return;
                        }
                        if let Some(error) = failure {
                            break ListingResult::Failed(error);
                        }
                    }
                    None => break ListingResult::Finished,
                }
            }
        }
        Err(error) => ListingResult::Failed(error.to_string()),
    };
    let _ = send_listing_result(&events, context_id, generation, target, result).await;
}

async fn send_listing_result(
    events: &mpsc::Sender<BrowserContextEvent>,
    context_id: BrowserContextId,
    generation: u64,
    target: ListingTarget,
    result: ListingResult,
) -> Result<(), mpsc::error::SendError<BrowserContextEvent>> {
    events
        .send(BrowserContextEvent {
            context_id,
            kind: BrowserEvent::Listing {
                generation,
                target,
                result,
            },
        })
        .await
}

#[cfg(test)]
mod tests {
    use std::{io, sync::Arc, time::Duration};

    use crate::{
        Client, ConnectionId, DirectoryEntry, EntryKind,
        test_support::{Listing, ScriptedServer},
    };
    use tokio::sync::mpsc;

    use super::{
        BrowserContext, BrowserContextEvent, ExpansionStatus, LISTING_GROUP, ListingStatus,
    };

    /// A Client whose one connection replays scripted listings.
    ///
    /// Requests travel the real `Client` path *and* the real wire path, so a
    /// scripted listing arrives having been batched, flattened and validated
    /// exactly as a production one is.
    struct ScriptedClient {
        client: Arc<Client>,
        connection: ConnectionId,
        server: ScriptedServer,
    }

    impl ScriptedClient {
        async fn new(listings: impl IntoIterator<Item = Listing>) -> Arc<Self> {
            let server = ScriptedServer::new(listings);
            let (client, connection) = Client::with_connection_for_test(server.connect().await);
            Arc::new(Self {
                client,
                connection,
                server,
            })
        }

        fn requested_paths(&self) -> Vec<String> {
            self.server.listed_paths()
        }
    }

    /// Collects entries into a scripted listing, stopping at the first error
    /// and replaying it as the stream's terminal item.
    fn listing(entries: impl IntoIterator<Item = io::Result<DirectoryEntry>>) -> Listing {
        let mut collected = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => collected.push(entry),
                Err(error) => return Listing::Failing(collected, error),
            }
        }
        Listing::Entries(collected)
    }

    fn entry(name: &str) -> DirectoryEntry {
        DirectoryEntry {
            name: name.into(),
            kind: EntryKind::File,
            navigable: false,
        }
    }

    fn directory(name: &str) -> DirectoryEntry {
        DirectoryEntry {
            name: name.into(),
            kind: EntryKind::Directory,
            navigable: true,
        }
    }

    fn symlink(name: &str, navigable: bool) -> DirectoryEntry {
        DirectoryEntry {
            name: name.into(),
            kind: EntryKind::Symlink,
            navigable,
        }
    }

    fn context(
        source: &Arc<ScriptedClient>,
        capacity: usize,
    ) -> (BrowserContext, mpsc::Receiver<BrowserContextEvent>) {
        let (events, receiver) = mpsc::channel(capacity);
        (
            BrowserContext::new(
                Arc::clone(&source.client),
                source.connection,
                "/home".into(),
                events,
            ),
            receiver,
        )
    }

    async fn apply_next(
        context: &mut BrowserContext,
        receiver: &mut mpsc::Receiver<BrowserContextEvent>,
    ) {
        let event = receiver.recv().await.expect("listing event");
        context.apply(event);
    }

    #[tokio::test]
    async fn creation_starts_and_completes_a_listing() {
        let source = ScriptedClient::new([listing([Ok(entry("one"))])]).await;
        let (mut context, mut receiver) = context(&source, 4);

        assert_eq!(context.status(), &ListingStatus::Loading);
        apply_next(&mut context, &mut receiver).await;
        apply_next(&mut context, &mut receiver).await;

        assert_eq!(context.entries(), [entry("one")]);
        assert_eq!(context.status(), &ListingStatus::Complete);
        assert_eq!(context.effective_selection(), Some(0));
    }

    /// Placing entries costs a pass over the tree -- finding the parent row, making
    /// room for its children -- so paying that per entry is quadratic in the size of
    /// the directory, which a large one feels as the loop stalling. Entries have to
    /// reach the context in groups for that cost to be paid per group instead.
    #[tokio::test]
    async fn a_large_listing_arrives_in_groups_rather_than_one_entry_at_a_time() {
        const ENTRIES: usize = 500;

        let source = ScriptedClient::new([listing(
            (0..ENTRIES).map(|index| Ok(entry(&format!("entry-{index:03}")))),
        )])
        .await;
        let (mut context, mut receiver) = context(&source, ENTRIES + 4);
        let mut events = 0;
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
            events += 1;
        }

        assert_eq!(context.rows().len(), ENTRIES);
        assert!(
            events <= ENTRIES / 8,
            "{ENTRIES} entries arrived in {events} events"
        );
    }

    /// A context whose listing has been fully applied.
    async fn scrolled(entries: usize, source: &Arc<ScriptedClient>) -> BrowserContext {
        let (mut context, mut receiver) = context(source, entries + 4);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }
        context
    }

    /// The renderer builds one list item per row it is handed, so a frame must be
    /// handed only the rows it can show. Handing over the whole tree is what made
    /// a keystroke in a large directory cost time proportional to its size.
    #[tokio::test]
    async fn the_viewport_yields_only_visible_rows_and_follows_the_selection() {
        let source = ScriptedClient::new([listing(
            (0..12).map(|index| Ok(entry(&format!("item-{index}")))),
        )])
        .await;
        let mut context = scrolled(12, &source).await;

        let viewport = context.viewport(6);
        assert_eq!(viewport.rows.len(), 6);
        assert_eq!(viewport.selected, Some(0));
        assert_eq!(viewport.rows[0].name(), "item-0");

        // Moving inside the window does not scroll it.
        for _ in 0..4 {
            context.move_selection(true);
        }
        let viewport = context.viewport(6);
        assert_eq!(viewport.selected, Some(4));
        assert_eq!(context.viewport_offset(), 0);

        // Reaching the edge scrolls, keeping a row of context past the selection.
        context.move_selection(true);
        let viewport = context.viewport(6);
        assert_eq!(viewport.rows.len(), 6);
        assert_eq!(viewport.rows[0].name(), "item-1");
        assert_eq!(viewport.selected, Some(4));
        assert_eq!(context.viewport_offset(), 1);

        // The window stops at the end of the list instead of running past it.
        while context.effective_selection() != Some(11) {
            context.move_selection(true);
        }
        let viewport = context.viewport(6);
        assert_eq!(viewport.rows.len(), 6);
        assert_eq!(viewport.rows[5].name(), "item-11");
        assert_eq!(viewport.selected, Some(5));
        assert_eq!(context.viewport_offset(), 6);

        // Moving back up scrolls the other way, again keeping a row of context.
        for _ in 0..5 {
            context.move_selection(false);
        }
        let viewport = context.viewport(6);
        assert_eq!(viewport.rows[0].name(), "item-5");
        assert_eq!(viewport.selected, Some(1));
    }

    #[tokio::test]
    async fn the_viewport_handles_lists_shorter_than_the_frame_and_no_frame_at_all() {
        let source = ScriptedClient::new([listing(
            (0..3).map(|index| Ok(entry(&format!("item-{index}")))),
        )])
        .await;
        let mut context = scrolled(3, &source).await;

        // A short list fills what it can and no more.
        let viewport = context.viewport(20);
        assert_eq!(viewport.rows.len(), 3);
        assert_eq!(viewport.selected, Some(0));

        // A zero-height body has nothing to show and no selection to place.
        let viewport = context.viewport(0);
        assert!(viewport.rows.is_empty());
        assert_eq!(viewport.selected, None);

        // A single-row body still tracks the selection.
        context.move_selection(true);
        context.move_selection(true);
        let viewport = context.viewport(1);
        assert_eq!(viewport.rows.len(), 1);
        assert_eq!(viewport.rows[0].name(), "item-2");
        assert_eq!(viewport.selected, Some(0));
    }

    #[tokio::test]
    async fn half_page_scroll_moves_selection_and_viewport_and_clamps_at_boundaries() {
        let source = ScriptedClient::new([listing(
            (0..12).map(|index| Ok(entry(&format!("item-{index}")))),
        )])
        .await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }
        context.set_viewport(0, 6);

        context.scroll_half_page(true);
        assert_eq!(context.effective_selection(), Some(3));
        assert_eq!(context.viewport_offset(), 3);

        context.scroll_half_page(false);
        assert_eq!(context.effective_selection(), Some(0));
        assert_eq!(context.viewport_offset(), 0);

        context.tree.set_selected_index(Some(10));
        context.set_viewport(6, 6);
        context.scroll_half_page(true);
        assert_eq!(context.effective_selection(), Some(11));
        assert_eq!(context.viewport_offset(), 6);
        context.scroll_half_page(true);
        assert_eq!(context.effective_selection(), Some(11));
        assert_eq!(context.viewport_offset(), 6);

        context.tree.set_selected_index(Some(4));
        context.set_viewport(4, 6);
        context.scroll_half_page(false);
        assert_eq!(context.effective_selection(), Some(1));
        assert_eq!(context.viewport_offset(), 1);
    }

    #[tokio::test]
    async fn terminal_failure_preserves_partial_entries() {
        let source = ScriptedClient::new([listing([
            Ok(entry("kept")),
            Err(io::Error::other("failed")),
        ])])
        .await;
        let (mut context, mut receiver) = context(&source, 4);

        apply_next(&mut context, &mut receiver).await;
        apply_next(&mut context, &mut receiver).await;

        assert_eq!(context.entries(), [entry("kept")]);
        assert_eq!(context.status(), &ListingStatus::Failed("failed".into()));
    }

    #[tokio::test]
    async fn refresh_clears_entries_and_rejects_queued_stale_results() {
        let source = ScriptedClient::new([
            listing([Ok(entry("stale"))]),
            listing([Ok(entry("current"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 8);
        let stale = receiver.recv().await.expect("stale batch");

        context.refresh();
        context.apply(stale);
        assert!(context.entries().is_empty());

        while context.status() != &ListingStatus::Complete {
            let event = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .expect("replacement listing event timeout")
                .expect("replacement listing event");
            context.apply(event);
        }
        assert_eq!(context.entries(), [entry("current")]);
        assert_eq!(context.status(), &ListingStatus::Complete);
    }

    #[tokio::test]
    async fn refresh_preserves_then_clamps_selection() {
        let source = ScriptedClient::new([
            listing([Ok(entry("a")), Ok(entry("b")), Ok(entry("c"))]),
            listing([Ok(entry("new-a")), Ok(entry("new-b"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 8);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }
        context.move_selection(true);
        context.move_selection(true);

        context.refresh();
        assert!(context.entries().is_empty());
        assert_eq!(
            context.tree.selected_index(),
            Some(2),
            "the index outlives the rows"
        );

        // How many entries share an event is up to what the stream had ready, so the
        // invariant is checked per event rather than per row: the index may still
        // point past what has arrived, but what is shown never does.
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
            assert!(
                context
                    .effective_selection()
                    .is_none_or(|index| index < context.rows().len())
            );
        }
        assert_eq!(
            context.tree.selected_index(),
            Some(1),
            "finishing clamps it into the shorter listing"
        );
    }

    #[tokio::test]
    async fn entering_selected_directory_changes_path_and_rejects_stale_results() {
        let source = ScriptedClient::new([
            listing([Ok(directory("reports"))]),
            listing([Ok(entry("current"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 8);
        apply_next(&mut context, &mut receiver).await;
        let stale_completion = receiver.recv().await.expect("stale completion");

        context.enter_selected();
        assert_eq!(context.root_path(), "/home/reports");
        assert!(context.entries().is_empty());
        assert_eq!(context.effective_selection(), None);
        assert_eq!(context.status(), &ListingStatus::Loading);

        context.apply(stale_completion);
        assert_eq!(context.status(), &ListingStatus::Loading);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        assert_eq!(context.entries(), [entry("current")]);
        assert_eq!(source.requested_paths(), ["/home", "/home/reports"]);
    }

    #[tokio::test]
    async fn entering_non_navigable_entry_does_nothing() {
        let source = ScriptedClient::new([listing([Ok(entry("report.txt"))])]).await;
        let (mut context, mut receiver) = context(&source, 4);
        apply_next(&mut context, &mut receiver).await;

        context.enter_selected();

        assert_eq!(context.root_path(), "/home");
        assert_eq!(context.entries(), [entry("report.txt")]);
        assert_eq!(source.requested_paths(), ["/home"]);
    }

    #[tokio::test]
    async fn leaving_changes_to_parent_and_does_nothing_at_root() {
        let source = ScriptedClient::new([
            listing([Ok(entry("initial"))]),
            listing([Ok(entry("parent"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 8);
        apply_next(&mut context, &mut receiver).await;
        let stale_completion = receiver.recv().await.expect("stale completion");

        context.leave();
        assert_eq!(context.root_path(), "/");
        assert_eq!(context.effective_selection(), None);
        context.apply(stale_completion);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.leave();
        tokio::task::yield_now().await;
        assert_eq!(context.root_path(), "/");
        assert_eq!(source.requested_paths(), ["/home", "/"]);
    }

    #[tokio::test]
    async fn failed_navigation_keeps_attempted_path() {
        let source = ScriptedClient::new([
            listing([Ok(directory("missing"))]),
            Listing::Rejected(io::Error::new(io::ErrorKind::NotFound, "missing directory")),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 8);
        apply_next(&mut context, &mut receiver).await;
        let stale_completion = receiver.recv().await.expect("stale completion");

        context.enter_selected();
        context.apply(stale_completion);
        while matches!(context.status(), ListingStatus::Loading) {
            apply_next(&mut context, &mut receiver).await;
        }

        assert_eq!(context.root_path(), "/home/missing");
        assert_eq!(
            context.status(),
            &ListingStatus::Failed("missing directory".into())
        );
    }

    #[tokio::test]
    async fn bounded_events_backpressure_the_listing() {
        let source = ScriptedClient::new([listing([Ok(entry("one")), Ok(entry("two"))])]).await;
        let (mut context, mut receiver) = context(&source, 1);

        tokio::time::timeout(Duration::from_secs(1), async {
            while receiver.len() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first bounded event timeout");
        assert_eq!(
            receiver.len(),
            1,
            "the listing task waits rather than running ahead of a full channel"
        );

        // Drained by condition, not by count: how many entries share an event is up
        // to what the stream had ready.
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }
        assert_eq!(context.entries(), [entry("one"), entry("two")]);
    }

    #[tokio::test]
    async fn toggling_directory_expands_inline_without_changing_root_and_reloads_after_collapse() {
        let source = ScriptedClient::new([
            listing([Ok(directory("reports"))]),
            listing([Ok(entry("first.txt"))]),
            listing([Ok(entry("second.txt"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 8);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.activate_selected();
        assert_eq!(context.root_path(), "/home");
        while matches!(
            context.rows()[0].expansion(),
            &ExpansionStatus::Loading { .. }
        ) {
            apply_next(&mut context, &mut receiver).await;
        }

        assert_eq!(
            context
                .rows()
                .iter()
                .map(|row| (row.path().as_escaped(), row.depth()))
                .collect::<Vec<_>>(),
            [("/home/reports", 0), ("/home/reports/first.txt", 1)]
        );
        assert!(matches!(
            context.rows()[0].expansion(),
            &ExpansionStatus::Expanded { .. }
        ));

        context.activate_selected();
        assert_eq!(context.rows().len(), 1);
        assert_eq!(context.rows()[0].expansion(), &ExpansionStatus::Collapsed);

        context.activate_selected();
        while matches!(
            context.rows()[0].expansion(),
            &ExpansionStatus::Loading { .. }
        ) {
            apply_next(&mut context, &mut receiver).await;
        }
        assert_eq!(context.rows()[1].path(), "/home/reports/second.txt");
        assert_eq!(
            source.requested_paths(),
            ["/home", "/home/reports", "/home/reports"]
        );
    }

    #[tokio::test]
    async fn nested_expansion_inserts_children_before_the_next_sibling() {
        let source = ScriptedClient::new([
            listing([Ok(directory("reports")), Ok(entry("root.txt"))]),
            listing([Ok(directory("year")), Ok(entry("summary.txt"))]),
            listing([Ok(entry("january.txt"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.activate_selected();
        apply_next(&mut context, &mut receiver).await;
        context.move_selection(true);
        context.activate_selected();
        while !context.listing_tasks.is_empty() {
            apply_next(&mut context, &mut receiver).await;
        }

        assert_eq!(
            context
                .rows()
                .iter()
                .map(|row| (row.name().as_escaped(), row.depth()))
                .collect::<Vec<_>>(),
            [
                ("reports", 0),
                ("year", 1),
                ("january.txt", 2),
                ("summary.txt", 1),
                ("root.txt", 0),
            ]
        );
    }

    /// A listing long enough to arrive in more than one group, with a directory
    /// as the last entry of the first group. Expanding that one puts its children
    /// exactly where the group after it is about to land, which is the one place
    /// an insertion cannot tell from a row index alone whether it belongs inside
    /// the subtree or after it.
    fn grouped_listing(prefix: &str, directory_at_group_end: bool) -> Listing {
        let last = LISTING_GROUP - 1;
        listing((0..LISTING_GROUP + 88).map(|index| {
            Ok(if directory_at_group_end && index == last {
                directory("nested")
            } else {
                entry(&format!("{prefix}-{index:03}"))
            })
        }))
    }

    /// Applies one event and reports that a whole group arrived, which the tests
    /// below need in order to expand the row a group boundary falls on.
    async fn apply_group(
        context: &mut BrowserContext,
        receiver: &mut mpsc::Receiver<BrowserContextEvent>,
    ) {
        let before = context.rows().len();
        apply_next(context, receiver).await;
        assert_eq!(
            context.rows().len() - before,
            LISTING_GROUP,
            "a scripted listing should arrive a full group at a time"
        );
    }

    /// The rest of a root listing lands after the subtree of a directory expanded
    /// while it was still streaming, and leaves that subtree's own insertion point
    /// alone -- both groups want the same row index, and only the deeper one owns
    /// it.
    #[tokio::test]
    async fn a_root_group_lands_after_the_subtree_of_a_directory_expanded_mid_stream() {
        let source = ScriptedClient::new([
            grouped_listing("root", true),
            listing([Ok(entry("child-0")), Ok(entry("child-1"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 16);
        apply_group(&mut context, &mut receiver).await;

        // The directory sits at the end of what has arrived, so its children are
        // due at the same row as the root listing's next group.
        context.tree.set_selected_index(Some(LISTING_GROUP - 1));
        context.activate_selected();
        while !context.listing_tasks.is_empty() || context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        let rows: Vec<_> = context
            .rows()
            .iter()
            .map(|row| (row.name().as_escaped(), row.depth()))
            .collect();
        assert_eq!(rows.len(), LISTING_GROUP + 88 + 2);
        assert_eq!(
            &rows[LISTING_GROUP - 1..LISTING_GROUP + 3],
            [
                ("nested", 0),
                ("child-0", 1),
                ("child-1", 1),
                ("root-512", 0),
            ]
        );
        assert_eq!(rows[rows.len() - 1], ("root-599", 0));
    }

    /// A nested expansion's children land under it rather than after everything
    /// its parent has yet to stream, whichever of the two listings the events
    /// arrive from first.
    #[tokio::test]
    async fn a_nested_expansions_children_land_ahead_of_its_parents_later_groups() {
        let source = ScriptedClient::new([
            listing([Ok(directory("outer"))]),
            grouped_listing("outer", true),
            listing([Ok(entry("inner-0")), Ok(entry("inner-1"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.activate_selected();
        apply_group(&mut context, &mut receiver).await;
        context.tree.set_selected_index(Some(LISTING_GROUP));
        context.activate_selected();
        while !context.listing_tasks.is_empty() {
            apply_next(&mut context, &mut receiver).await;
        }

        let rows: Vec<_> = context
            .rows()
            .iter()
            .map(|row| (row.name().as_escaped(), row.depth()))
            .collect();
        assert_eq!(rows.len(), LISTING_GROUP + 88 + 3);
        assert_eq!(
            &rows[LISTING_GROUP..LISTING_GROUP + 4],
            [
                ("nested", 1),
                ("inner-0", 2),
                ("inner-1", 2),
                ("outer-512", 1),
            ]
        );
        assert_eq!(rows[rows.len() - 1], ("outer-599", 1));
    }

    /// Two directories streaming at once keep their children apart: the groups of
    /// the earlier one land ahead of the later one, which the later one's own
    /// insertion point has to move out of the way of.
    #[tokio::test]
    async fn an_earlier_expansions_groups_push_a_later_one_further_down() {
        let source = ScriptedClient::new([
            listing([Ok(directory("first")), Ok(directory("second"))]),
            grouped_listing("first", false),
            listing([Ok(entry("second-0")), Ok(entry("second-1"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.activate_selected();
        apply_group(&mut context, &mut receiver).await;
        context.tree.set_selected_index(Some(LISTING_GROUP + 1));
        context.activate_selected();
        while !context.listing_tasks.is_empty() {
            apply_next(&mut context, &mut receiver).await;
        }

        let rows: Vec<_> = context
            .rows()
            .iter()
            .map(|row| (row.name().as_escaped(), row.depth()))
            .collect();
        assert_eq!(rows.len(), LISTING_GROUP + 88 + 4);
        assert_eq!(
            &rows[rows.len() - 4..],
            [
                ("first-599", 1),
                ("second", 0),
                ("second-0", 1),
                ("second-1", 1),
            ]
        );
    }

    /// Collapsing a subtree while a later directory is still streaming keeps that
    /// directory's remaining children underneath it, rather than at the row index
    /// they were bound for before the collapse closed the gap.
    #[tokio::test]
    async fn collapsing_an_earlier_subtree_keeps_a_later_expansions_children_under_it() {
        let source = ScriptedClient::new([
            listing([Ok(directory("first")), Ok(directory("second"))]),
            grouped_listing("first", false),
            listing([Ok(entry("second-0")), Ok(entry("second-1"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.activate_selected();
        apply_group(&mut context, &mut receiver).await;
        context.tree.set_selected_index(Some(LISTING_GROUP + 1));
        context.activate_selected();
        // Collapsing the first directory while it streams takes its children back
        // out from under the second one's insertion point.
        context.tree.set_selected_index(Some(0));
        context.activate_selected();
        while !context.listing_tasks.is_empty() {
            apply_next(&mut context, &mut receiver).await;
        }

        assert_eq!(
            context
                .rows()
                .iter()
                .map(|row| (row.name().as_escaped(), row.depth()))
                .collect::<Vec<_>>(),
            [
                ("first", 0),
                ("second", 0),
                ("second-0", 1),
                ("second-1", 1),
            ]
        );
        assert_eq!(context.rows()[0].expansion(), &ExpansionStatus::Collapsed);
    }

    #[tokio::test]
    async fn streamed_children_do_not_move_selection_off_an_existing_row() {
        let source = ScriptedClient::new([
            listing([Ok(directory("reports")), Ok(entry("root.txt"))]),
            listing([Ok(entry("child.txt"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 8);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.activate_selected();
        context.move_selection(true);
        assert_eq!(
            context.rows()[context.effective_selection().unwrap()].name(),
            "root.txt"
        );

        apply_next(&mut context, &mut receiver).await;

        assert_eq!(context.effective_selection(), Some(2));
        assert_eq!(context.rows()[2].name(), "root.txt");
    }

    #[tokio::test]
    async fn collapsing_rejects_already_queued_child_events() {
        let source = ScriptedClient::new([
            listing([Ok(directory("reports"))]),
            listing([Ok(entry("stale.txt"))]),
            listing([Ok(entry("current.txt"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 8);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.activate_selected();
        let stale = receiver.recv().await.expect("queued child event");
        context.activate_selected();
        context.apply(stale);
        assert_eq!(context.rows().len(), 1);

        context.activate_selected();
        while matches!(
            context.rows()[0].expansion(),
            &ExpansionStatus::Loading { .. }
        ) {
            apply_next(&mut context, &mut receiver).await;
        }
        assert_eq!(context.rows()[1].name(), "current.txt");
    }

    #[tokio::test]
    async fn activating_files_and_non_directory_symlinks_does_nothing() {
        let source = ScriptedClient::new([listing([
            Ok(entry("report.txt")),
            Ok(symlink("latest", false)),
        ])])
        .await;
        let (mut context, mut receiver) = context(&source, 8);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.activate_selected();
        context.move_selection(true);
        context.activate_selected();

        assert_eq!(context.rows().len(), 2);
        assert_eq!(source.requested_paths(), ["/home"]);
    }

    #[tokio::test]
    async fn navigable_symlinks_expand_and_special_entries_do_nothing() {
        let special = DirectoryEntry {
            name: "socket".into(),
            kind: EntryKind::Other,
            navigable: false,
        };
        let source = ScriptedClient::new([
            listing([Ok(symlink("linked-directory", true)), Ok(special)]),
            listing([Ok(entry("child.txt"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 8);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.activate_selected();
        while matches!(
            context.rows()[0].expansion(),
            &ExpansionStatus::Loading { .. }
        ) {
            apply_next(&mut context, &mut receiver).await;
        }
        assert_eq!(context.rows()[1].path(), "/home/linked-directory/child.txt");

        context.move_selection(true);
        context.move_selection(true);
        context.activate_selected();
        assert_eq!(context.rows().len(), 3);
    }

    /// A tree with one expanded directory, so filtering has both a nested match
    /// and a top-level one to place.
    async fn expanded_tree() -> BrowserContext {
        let source = ScriptedClient::new([
            listing([Ok(directory("reports")), Ok(entry("root.txt"))]),
            listing([Ok(entry("january.txt")), Ok(entry("summary.txt"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }
        context.activate_selected();
        while matches!(
            context.rows()[0].expansion(),
            &ExpansionStatus::Loading { .. }
        ) {
            apply_next(&mut context, &mut receiver).await;
        }
        assert_eq!(
            context.row_names(),
            ["reports", "january.txt", "summary.txt", "root.txt"]
        );
        context
    }

    /// A browser holding one directory and two files beside it, plus a scripted
    /// listing of that directory for a `/` to open.
    async fn nested_source() -> Arc<ScriptedClient> {
        ScriptedClient::new([
            listing([
                Ok(directory("reports")),
                Ok(entry("january-outside.txt")),
                Ok(entry("outside.txt")),
            ]),
            listing([Ok(entry("january.txt")), Ok(entry("june.txt"))]),
            listing([Ok(entry("january.txt")), Ok(entry("june.txt"))]),
        ])
        .await
    }

    /// `/` opens a directory rather than navigating to one: the root stays where it
    /// is, and everything outside the directory drops out of the matching.
    #[tokio::test]
    async fn stepping_into_a_directory_scopes_the_query_without_moving_the_root() {
        let source = nested_source().await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.begin_filter();
        for character in "rep".chars() {
            context.extend_filter(character);
        }

        // A query that has only narrowed onto the directory does not open it.
        context.descend_filter();
        assert_eq!(context.filter_scope(), None);
        assert_eq!(context.filter_warning(), Some("no exact match to open"));

        context.complete_filter();
        assert_eq!(context.filter(), Some("reports"));
        assert_eq!(
            context.filter_warning(),
            None,
            "the next keystroke clears it"
        );
        context.descend_filter();

        assert_eq!(context.root_path(), "/home", "the root never moves");
        assert_eq!(context.filter_scope().as_deref(), Some("reports"));
        assert_eq!(
            context.filter(),
            Some(""),
            "the query is spent stepping in, ready for the next segment"
        );
        assert_eq!(
            context.visible_names(8),
            ["reports"],
            "the directory shows while its children are still loading"
        );

        while matches!(
            context.rows()[0].expansion(),
            &ExpansionStatus::Loading { .. }
        ) {
            apply_next(&mut context, &mut receiver).await;
        }
        assert_eq!(
            context.visible_names(8),
            ["reports", "january.txt", "june.txt"],
            "an empty query inside a directory means everything in it"
        );

        for character in "jan".chars() {
            context.extend_filter(character);
        }
        assert_eq!(
            context.filter_matches(),
            Some(1),
            "january-outside.txt is out of scope"
        );
        assert_eq!(context.visible_names(8), ["reports", "january.txt"]);

        // Completion agrees only among the matches inside the directory. The
        // out-of-scope january-outside.txt would have cut this back to "january".
        context.complete_filter();
        assert_eq!(context.filter(), Some("january.txt"));

        // A file is named outright here, and still has nothing to step into.
        context.descend_filter();
        assert_eq!(context.filter_scope().as_deref(), Some("reports"));
        assert_eq!(context.filter(), Some("january.txt"));
        assert_eq!(context.filter_warning(), Some("not a directory"));
    }

    /// Enter is a confirmation, so it first spells out what the query narrowed to
    /// and only takes it once the query says it outright.
    #[tokio::test]
    async fn enter_fills_the_selected_match_in_before_it_takes_it() {
        let source = nested_source().await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.begin_filter();
        context.extend_filter('u');
        assert_eq!(context.filter_matches(), Some(2), "both outside names");
        assert_eq!(
            context.selected_entry().unwrap().name,
            "january-outside.txt"
        );

        context.submit_filter();
        assert_eq!(
            context.filter(),
            Some("january-outside.txt"),
            "the first Enter spells the selected match out"
        );
        assert_eq!(
            context.filter_matches(),
            Some(2),
            "spelling a candidate out must not shrink the set it came from"
        );
        assert_eq!(
            context.selected_entry().unwrap().name,
            "january-outside.txt"
        );

        context.submit_filter();
        assert_eq!(
            context.filter(),
            None,
            "the prompt already named the row, so the second Enter takes it"
        );
        assert_eq!(
            context.selected_entry().unwrap().name,
            "january-outside.txt"
        );

        // A row the query did not match has nothing to fill in. Stepping into a
        // directory leaves the selection on the directory itself, which is on screen
        // to place its contents rather than because it matched.
        context.begin_filter();
        for character in "rep".chars() {
            context.extend_filter(character);
        }
        context.submit_filter();
        context.descend_filter();
        assert_eq!(context.filter_scope().as_deref(), Some("reports"));
        assert_eq!(context.selected_entry().unwrap().name, "reports");
        context.submit_filter();
        assert_eq!(context.filter(), None, "taken directly");
        assert_eq!(context.selected_entry().unwrap().name, "reports");
    }

    /// Walking the candidates spells each one out in the prompt, which is only
    /// useful if the walk survives it: matching goes by what was typed, so the set
    /// being walked stays put while the text on screen changes.
    #[tokio::test]
    async fn walking_the_candidates_spells_each_one_out_without_narrowing_them() {
        let source = nested_source().await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.begin_filter();
        context.extend_filter('u');
        assert_eq!(context.filter(), Some("u"), "the query as typed");

        context.move_selection(true);
        assert_eq!(context.filter(), Some("outside.txt"));
        assert_eq!(context.filter_matches(), Some(2));

        context.move_selection(false);
        assert_eq!(
            context.filter(),
            Some("january-outside.txt"),
            "the walk still has both candidates to move between"
        );

        // Typing edits the text on screen, so the spelled-out candidate is what the
        // next keystroke lands on.
        context.shorten_filter();
        assert_eq!(context.filter(), Some("january-outside.tx"));
        context.extend_filter('t');
        assert_eq!(context.filter(), Some("january-outside.txt"));
        assert_eq!(context.filter_matches(), Some(1), "typed, so it narrows");
    }

    /// Clearing takes one whole entry per press, which is the same ladder backspace
    /// walks a character at a time: the query, then the directory a `/` stepped into
    /// along with the query that entered it, then the prompt itself.
    #[tokio::test]
    async fn clearing_takes_the_last_entry_and_then_closes_the_prompt() {
        let source = ScriptedClient::new([
            listing([Ok(directory("work-notes")), Ok(directory("workspace"))]),
            listing([Ok(entry("file.txt"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.begin_filter();
        for character in "wo".chars() {
            context.extend_filter(character);
        }
        context.move_selection(true);
        context.descend_filter();
        while matches!(
            context.rows()[1].expansion(),
            &ExpansionStatus::Loading { .. }
        ) {
            apply_next(&mut context, &mut receiver).await;
        }
        for character in "fil".chars() {
            context.extend_filter(character);
        }
        assert_eq!(context.filter(), Some("fil"));

        // The query goes, and the directory it was narrowing stays open.
        context.clear_filter_entry();
        assert_eq!(context.filter(), Some(""));
        assert_eq!(context.filter_scope().as_deref(), Some("workspace"));
        assert_eq!(context.visible_names(8), ["workspace", "file.txt"]);

        // With no query left, the next press is the directory itself, and the name
        // that entered it goes with it rather than being left behind as a query.
        context.clear_filter_entry();
        assert_eq!(context.filter(), Some(""));
        assert_eq!(context.filter_scope(), None);
        assert_eq!(
            context.row_names(),
            ["work-notes", "workspace"],
            "the directory the prompt opened is closed again"
        );

        // With no entry left at all, the prompt goes.
        context.clear_filter_entry();
        assert_eq!(context.filter(), None);
    }

    /// Clearing takes what is on screen with it, not just the query underneath.
    #[tokio::test]
    async fn clearing_takes_a_candidate_walked_onto_with_it() {
        let source = ScriptedClient::new([listing([
            Ok(directory("work-notes")),
            Ok(directory("workspace")),
        ])])
        .await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.begin_filter();
        for character in "wo".chars() {
            context.extend_filter(character);
        }
        context.move_selection(true);
        assert_eq!(context.filter(), Some("workspace"));

        context.clear_filter_entry();
        assert_eq!(
            context.filter(),
            Some(""),
            "the spelled-out name goes too, leaving no `wo` behind"
        );
    }

    /// What entered a directory is the text that was on screen. Walking onto a
    /// candidate spells its name out, so a backspace over that `/` has to give the
    /// name back rather than the fragment typed before the walk.
    #[tokio::test]
    async fn backspace_over_a_slash_restores_the_name_that_entered_the_directory() {
        let source = ScriptedClient::new([
            listing([Ok(directory("work-notes")), Ok(directory("workspace"))]),
            listing([Ok(entry("file.txt"))]),
        ])
        .await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.begin_filter();
        for character in "wo".chars() {
            context.extend_filter(character);
        }
        assert_eq!(context.filter(), Some("wo"));
        assert_eq!(context.filter_matches(), Some(2));

        context.move_selection(true);
        assert_eq!(
            context.filter(),
            Some("workspace"),
            "walking onto a candidate spells it out"
        );

        context.descend_filter();
        assert_eq!(context.filter_scope().as_deref(), Some("workspace"));
        assert_eq!(context.filter(), Some(""));

        context.shorten_filter();
        assert_eq!(
            context.filter(),
            Some("workspace"),
            "the name that entered it, not the `wo` that found it"
        );
        assert_eq!(context.filter_scope(), None);
    }

    /// Backspace walks the prompt back one keystroke at a time, including over each
    /// `/`: out of the directory it opened, then out of the prompt itself.
    #[tokio::test]
    async fn backspace_undoes_each_slash_and_then_the_prompt() {
        let source = nested_source().await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }
        context.move_selection(true);
        let restored = context.selected_entry().unwrap().name;

        context.begin_filter();
        for character in "reports".chars() {
            context.extend_filter(character);
        }
        context.descend_filter();
        while matches!(
            context.rows()[0].expansion(),
            &ExpansionStatus::Loading { .. }
        ) {
            apply_next(&mut context, &mut receiver).await;
        }
        context.extend_filter('j');
        assert_eq!(context.filter_matches(), Some(2));

        context.shorten_filter();
        assert_eq!(context.filter(), Some(""), "back to the whole directory");

        // The backspace over the `/` closes the directory it opened and hands back
        // the query that entered it.
        context.shorten_filter();
        assert_eq!(context.filter(), Some("reports"));
        assert_eq!(context.filter_scope(), None);
        assert_eq!(
            context.row_names(),
            ["reports", "january-outside.txt", "outside.txt"],
            "the directory the prompt opened is closed again"
        );
        assert_eq!(context.selected_entry().unwrap().name, "reports");

        while context.filter() != Some("") {
            context.shorten_filter();
        }

        // Nothing is left to delete but the `/` that opened the prompt.
        context.shorten_filter();
        assert_eq!(context.filter(), None);
        assert_eq!(
            context.selected_entry().unwrap().name,
            restored,
            "closing this way puts the selection back, like Esc"
        );
    }

    /// A directory the prompt opened is part of what the prompt did, so abandoning
    /// closes it and committing keeps it. One the user had already expanded is not,
    /// and stays open either way.
    #[tokio::test]
    async fn only_the_directories_the_prompt_opened_close_when_it_is_abandoned() {
        let source = nested_source().await;
        let (mut context, mut receiver) = context(&source, 16);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.begin_filter();
        // Enter spells the selected directory out, which is what lets `/` open it.
        context.submit_filter();
        context.descend_filter();
        while matches!(
            context.rows()[0].expansion(),
            &ExpansionStatus::Loading { .. }
        ) {
            apply_next(&mut context, &mut receiver).await;
        }
        assert_eq!(context.rows().len(), 5);

        context.abandon_filter();
        assert_eq!(
            context.row_names(),
            ["reports", "january-outside.txt", "outside.txt"]
        );

        context.begin_filter();
        // Enter spells the selected directory out, which is what lets `/` open it.
        context.submit_filter();
        context.descend_filter();
        while matches!(
            context.rows()[0].expansion(),
            &ExpansionStatus::Loading { .. }
        ) {
            apply_next(&mut context, &mut receiver).await;
        }
        context.commit_filter();
        assert_eq!(
            context.row_names(),
            [
                "reports",
                "january.txt",
                "june.txt",
                "january-outside.txt",
                "outside.txt"
            ],
            "committing keeps what the prompt opened on the way to the row it kept"
        );

        // Stepping into an already-open directory is not what opened it.
        context.begin_filter();
        context.submit_filter();
        context.descend_filter();
        assert_eq!(context.filter_scope().as_deref(), Some("reports"));
        context.abandon_filter();
        assert_eq!(context.rows().len(), 5);
    }

    /// A match inside a directory is unreadable without the directory, so the
    /// ancestors of every match come along even though they do not match.
    #[tokio::test]
    async fn filtering_keeps_matches_and_the_ancestors_that_place_them() {
        let mut context = expanded_tree().await;

        for character in "january".chars() {
            context.extend_filter(character);
        }
        assert_eq!(context.filter(), None, "typing without a prompt open");

        context.begin_filter();
        for character in "january".chars() {
            context.extend_filter(character);
        }

        assert_eq!(context.filter(), Some("january"));
        assert_eq!(context.filter_matches(), Some(1));
        assert_eq!(context.visible_names(8), ["reports", "january.txt"]);
        assert_eq!(
            context.selected_entry().unwrap().name,
            "january.txt",
            "the selection lands on the match, not on the directory holding it"
        );
        // The rows themselves are untouched, so an in-flight listing still
        // addresses the same indices.
        assert_eq!(
            context.row_names(),
            ["reports", "january.txt", "summary.txt", "root.txt"]
        );
    }

    /// Filtering is a way to move: it takes the selection to what was typed, and
    /// the two ways out differ only in whether that move is kept.
    #[tokio::test]
    async fn a_filter_moves_the_selection_and_only_abandoning_puts_it_back() {
        let mut context = expanded_tree().await;
        assert_eq!(context.effective_selection(), Some(0));

        context.begin_filter();
        for character in "root".chars() {
            context.extend_filter(character);
        }
        assert_eq!(context.effective_selection(), Some(3));

        context.abandon_filter();
        assert_eq!(context.filter(), None);
        assert_eq!(context.effective_selection(), Some(0));

        context.begin_filter();
        for character in "root".chars() {
            context.extend_filter(character);
        }
        context.commit_filter();
        assert_eq!(context.filter(), None);
        assert_eq!(context.effective_selection(), Some(3));
        assert_eq!(context.visible_names(8).len(), 4);
    }

    /// Selection and scrolling walk what is on screen, not the rows behind it.
    #[tokio::test]
    async fn movement_under_a_filter_steps_between_matches() {
        let source = ScriptedClient::new([listing(
            (0..12).map(|index| Ok(entry(&format!("item-{index}")))),
        )])
        .await;
        let mut context = scrolled(12, &source).await;

        context.begin_filter();
        context.extend_filter('1');
        assert_eq!(
            context.visible_names(8),
            ["item-1", "item-10", "item-11"],
            "'1' matches one single digit and both teens"
        );

        context.move_selection(true);
        assert_eq!(context.selected_entry().unwrap().name, "item-10");
        context.move_selection(true);
        assert_eq!(context.selected_entry().unwrap().name, "item-11");
        context.move_selection(true);
        assert_eq!(
            context.selected_entry().unwrap().name,
            "item-11",
            "the last match is the end of the list"
        );
        context.move_selection(false);
        assert_eq!(context.selected_entry().unwrap().name, "item-10");

        context.set_viewport(0, 2);
        context.scroll_half_page(true);
        assert_eq!(context.selected_entry().unwrap().name, "item-11");
    }

    #[tokio::test]
    async fn tab_completes_to_the_prefix_the_matches_agree_on() {
        let source = ScriptedClient::new([listing([
            Ok(entry("report-2024.txt")),
            Ok(entry("report-2025.txt")),
            Ok(entry("notes.txt")),
        ])])
        .await;
        let mut context = scrolled(3, &source).await;

        context.begin_filter();
        for character in "rep".chars() {
            context.extend_filter(character);
        }
        context.complete_filter();
        assert_eq!(context.filter(), Some("report-202"));
        assert_eq!(context.filter_matches(), Some(2));

        // Matches that agree on nothing further leave the query alone.
        context.complete_filter();
        assert_eq!(context.filter(), Some("report-202"));

        context.extend_filter('4');
        context.complete_filter();
        assert_eq!(context.filter(), Some("report-2024.txt"));
        assert_eq!(context.filter_matches(), Some(1));
    }

    /// A query that matches nothing must not throw the selection away, because
    /// the next backspace is meant to widen straight back onto it.
    #[tokio::test]
    async fn a_filter_matching_nothing_shows_nothing_and_backspace_widens_back() {
        let mut context = expanded_tree().await;
        context.move_selection(true);

        context.begin_filter();
        for character in "zzz".chars() {
            context.extend_filter(character);
        }
        assert_eq!(context.filter_matches(), Some(0));
        assert!(context.visible_names(8).is_empty());
        assert_eq!(context.viewport(8).selected, None);
        assert_eq!(context.effective_selection(), Some(1));

        for _ in 0..3 {
            context.shorten_filter();
        }
        assert_eq!(context.filter(), Some(""));
        assert_eq!(context.filter_matches(), None, "nothing narrowed");
        assert_eq!(context.visible_names(8).len(), 4);
        assert_eq!(context.selected_entry().unwrap().name, "january.txt");
    }

    #[tokio::test]
    async fn dropping_a_context_cancels_its_listing() {
        let source = ScriptedClient::new([Listing::Pending]).await;
        let (context, _receiver) = context(&source, 1);
        let task = context.root_listing_task.as_ref().unwrap().abort_handle();

        drop(context);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("listing cancellation timeout");

        assert!(task.is_finished());
    }
}
