use std::{collections::HashMap, fmt, path::Path, sync::Arc};

use crate::{Client, ConnectionId, DirectoryEntry, EntryKind};
use futures::TryStreamExt as _;
use tokio::{sync::mpsc, task::JoinHandle};
use uuid::Uuid;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ListingStatus {
    Loading,
    Complete,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExpansionStatus {
    Collapsed,
    Loading { request_id: u64 },
    Expanded { request_id: u64 },
    Failed { request_id: u64, error: String },
}

impl ExpansionStatus {
    fn request_id(&self) -> Option<u64> {
        match self {
            Self::Collapsed => None,
            Self::Loading { request_id }
            | Self::Expanded { request_id }
            | Self::Failed { request_id, .. } => Some(*request_id),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BrowserRow {
    path: String,
    depth: usize,
    entry: DirectoryEntry,
    expansion: ExpansionStatus,
}

/// The rows one frame displays, and where the selection sits inside them.
///
/// A filter leaves gaps, so this is a list of borrows rather than a slice. It is
/// still one entry per row on screen, which is what bounds a frame's cost.
#[derive(Debug)]
pub(crate) struct Viewport<'a> {
    pub(crate) rows: Vec<&'a BrowserRow>,
    pub(crate) selected: Option<usize>,
}

/// A live filter over the rows, and the selection to put back if it is
/// abandoned.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Filter {
    query: String,
    restore_selection: Option<usize>,
}

/// Which rows a frame may show: every row, or the rows a filter matched
/// together with the ancestors that keep them readable.
///
/// Filtering never removes rows. Expansion, streaming insertion, and collapse
/// all address rows by index, so a filter that dropped them would move every
/// index out from under an in-flight listing. It is a window instead, and the
/// only state a filter moves is the selection and the scroll offset -- both of
/// which are positions in this window rather than row indices.
#[derive(Clone, Debug, Eq, PartialEq)]
enum VisibleRows {
    All(usize),
    Filtered {
        rows: Vec<VisibleRow>,
        matches: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VisibleRow {
    row: usize,
    /// False for a row on screen only to place a match underneath it.
    matched: bool,
}

impl VisibleRows {
    fn len(&self) -> usize {
        match self {
            Self::All(count) => *count,
            Self::Filtered { rows, .. } => rows.len(),
        }
    }

    /// How many rows matched outright, excluding the ancestors carried along for
    /// readability. `None` when every row is on screen anyway.
    fn matches(&self) -> Option<usize> {
        match self {
            Self::All(_) => None,
            Self::Filtered { matches, .. } => Some(*matches),
        }
    }

    fn row(&self, position: usize) -> Option<usize> {
        match self {
            Self::All(count) => (position < *count).then_some(position),
            Self::Filtered { rows, .. } => rows.get(position).map(|visible| visible.row),
        }
    }

    /// Where `row` sits, or where the nearest visible row after it sits, so a
    /// selection the filter just hid still lands somewhere sensible. `None` when
    /// there is nowhere to land at all.
    fn position_of(&self, row: usize) -> Option<usize> {
        let last = self.len().checked_sub(1)?;
        Some(match self {
            Self::All(_) => row.min(last),
            Self::Filtered { rows, .. } => start_of(rows, row).min(last),
        })
    }

    /// Where the nearest match at or after `row` sits, falling back to the last
    /// match. This is where a filter puts the selection: an ancestor is context,
    /// not something the user asked to land on.
    ///
    /// A filtered list only exists because something matched, so a non-empty one
    /// always has a match to offer.
    fn match_position_of(&self, row: usize) -> Option<usize> {
        let Self::Filtered { rows, .. } = self else {
            return self.position_of(row);
        };
        let start = start_of(rows, row);
        rows.iter()
            .skip(start)
            .position(|visible| visible.matched)
            .map(|offset| start + offset)
            .or_else(|| rows.iter().rposition(|visible| visible.matched))
    }
}

/// The position `row` would take among visible rows, which is where it sits when
/// it is visible itself.
fn start_of(rows: &[VisibleRow], row: usize) -> usize {
    rows.partition_point(|visible| visible.row < row)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SelectedEntry {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) kind: EntryKind,
}

impl BrowserRow {
    fn new(path: String, depth: usize, entry: DirectoryEntry) -> Self {
        Self {
            path,
            depth,
            entry,
            expansion: ExpansionStatus::Collapsed,
        }
    }

    pub(crate) fn depth(&self) -> usize {
        self.depth
    }

    pub(crate) fn entry(&self) -> &DirectoryEntry {
        &self.entry
    }

    pub(crate) fn expansion(&self) -> &ExpansionStatus {
        &self.expansion
    }
}

pub(crate) struct BrowserContext {
    id: BrowserContextId,
    root_path: String,
    rows: Vec<BrowserRow>,
    selected_index: Option<usize>,
    filter: Option<Filter>,
    /// A position in the visible rows, which is a row index only while no filter
    /// narrows them.
    viewport_offset: usize,
    viewport_height: usize,
    status: ListingStatus,
    root_listing_task: Option<JoinHandle<()>>,
    directory_listing_tasks: HashMap<u64, JoinHandle<()>>,
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
            .field("root_path", &self.root_path)
            .field("rows", &self.rows)
            .field("selected_index", &self.selected_index)
            .field("filter", &self.filter)
            .field("viewport_offset", &self.viewport_offset)
            .field("viewport_height", &self.viewport_height)
            .field("status", &self.status)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl BrowserContext {
    pub(crate) fn new(
        client: Arc<Client>,
        connection: ConnectionId,
        root_path: String,
        events: mpsc::Sender<BrowserContextEvent>,
    ) -> Self {
        let mut context = Self {
            id: BrowserContextId::new(),
            root_path,
            rows: Vec::new(),
            selected_index: None,
            filter: None,
            viewport_offset: 0,
            viewport_height: 0,
            status: ListingStatus::Loading,
            root_listing_task: None,
            directory_listing_tasks: HashMap::new(),
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

    pub(crate) fn root_path(&self) -> &str {
        &self.root_path
    }

    pub(crate) fn rows(&self) -> &[BrowserRow] {
        &self.rows
    }

    #[cfg(test)]
    fn entries(&self) -> Vec<DirectoryEntry> {
        self.rows.iter().map(|row| row.entry.clone()).collect()
    }

    #[cfg(test)]
    fn row_names(&self) -> Vec<String> {
        self.rows.iter().map(|row| row.entry.name.clone()).collect()
    }

    /// The names one frame of `height` rows puts on screen.
    #[cfg(test)]
    fn visible_names(&mut self, height: usize) -> Vec<String> {
        self.viewport(height)
            .rows
            .iter()
            .map(|row| row.entry.name.clone())
            .collect()
    }

    pub(crate) fn status(&self) -> &ListingStatus {
        &self.status
    }

    pub(crate) fn selected_expansion_error(&self) -> Option<(&str, &str)> {
        let row = self
            .effective_selection()
            .and_then(|index| self.rows.get(index))?;
        match &row.expansion {
            ExpansionStatus::Failed { error, .. } => Some((&row.path, error)),
            _ => None,
        }
    }

    pub(crate) fn effective_selection(&self) -> Option<usize> {
        (!self.rows.is_empty()).then(|| self.selected_index.unwrap_or(0).min(self.rows.len() - 1))
    }

    pub(crate) fn selected_entry(&self) -> Option<SelectedEntry> {
        let row = self
            .effective_selection()
            .and_then(|index| self.rows.get(index))?;
        Some(SelectedEntry {
            path: row.path.clone(),
            name: row.entry.name.clone(),
            kind: row.entry.kind,
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
        let Some(selected) = self
            .effective_selection()
            .filter(|_| height > 0)
            .and_then(|row| visible.position_of(row))
        else {
            return Viewport {
                rows: Vec::new(),
                selected: None,
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
        Viewport {
            rows: (offset..end)
                .filter_map(|position| visible.row(position))
                .filter_map(|row| self.rows.get(row))
                .collect(),
            selected: Some(selected - offset),
        }
    }

    pub(crate) fn move_selection(&mut self, down: bool) {
        if self.rows.is_empty() {
            self.selected_index = None;
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
        self.selected_index = visible.row(position);
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
        self.selected_index = visible.row(position);
    }

    /// The rows the active filter leaves on screen: every name that matches, plus
    /// the ancestors of each match, so a hit inside a deep directory still reads
    /// as being inside it.
    ///
    /// Recomputed per call rather than cached. That costs one pass over the tree
    /// per keystroke and per frame drawn while filtering, which is the same order
    /// as the matching itself, and it means no row mutation anywhere else has to
    /// remember to invalidate anything. An unfiltered browser pays nothing.
    fn visible_rows(&self) -> VisibleRows {
        let Some(needle) = self.needle() else {
            return VisibleRows::All(self.rows.len());
        };
        let mut rows = Vec::new();
        let mut matches = 0;
        // The ancestor chain of the row under consideration, and whether each
        // link has been emitted already. Rows are in pre-order, so truncating to
        // the current depth leaves exactly that row's ancestors.
        let mut ancestors: Vec<(usize, bool)> = Vec::new();
        for (index, row) in self.rows.iter().enumerate() {
            ancestors.truncate(row.depth);
            let hit = matches_needle(&row.entry.name, &needle);
            if hit {
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
                    matched: true,
                });
                matches += 1;
            }
            ancestors.push((index, hit));
        }
        VisibleRows::Filtered { rows, matches }
    }

    /// The active query, lowercased for matching. `None` covers both no filter at
    /// all and an open prompt with nothing typed yet, which narrow nothing.
    fn needle(&self) -> Option<String> {
        self.filter
            .as_ref()
            .map(|filter| filter.query.to_lowercase())
            .filter(|needle| !needle.is_empty())
    }

    pub(crate) fn filter(&self) -> Option<&str> {
        self.filter.as_ref().map(|filter| filter.query.as_str())
    }

    /// How many rows the active query matches, or `None` when nothing is narrowed.
    pub(crate) fn filter_matches(&self) -> Option<usize> {
        self.visible_rows().matches()
    }

    /// Opens the filter prompt, remembering the selection so abandoning it can put
    /// it back.
    pub(crate) fn begin_filter(&mut self) {
        self.filter = Some(Filter {
            query: String::new(),
            restore_selection: self.selected_index,
        });
    }

    pub(crate) fn extend_filter(&mut self, character: char) {
        if let Some(filter) = &mut self.filter {
            filter.query.push(character);
            self.follow_filter();
        }
    }

    pub(crate) fn shorten_filter(&mut self) {
        if let Some(filter) = &mut self.filter {
            filter.query.pop();
            self.follow_filter();
        }
    }

    /// Extends the query to the longest prefix every name it matches agrees on,
    /// the way a shell completes a partial word. Matches that agree on nothing
    /// further leave the query alone, so a completion never narrows what is on
    /// screen -- every match starts with the prefix it completes to.
    pub(crate) fn complete_filter(&mut self) {
        let Some(needle) = self.needle() else {
            return;
        };
        let mut matches = self
            .rows
            .iter()
            .map(|row| row.entry.name.as_str())
            .filter(|name| matches_needle(name, &needle));
        let Some(first) = matches.next() else {
            return;
        };
        let mut completion = first.to_owned();
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
        filter.query = completion;
        self.follow_filter();
    }

    /// Closes the prompt, keeping the row the filter led to selected.
    pub(crate) fn commit_filter(&mut self) {
        self.filter = None;
    }

    /// Closes the prompt and puts the selection back where it started.
    pub(crate) fn abandon_filter(&mut self) {
        if let Some(filter) = self.filter.take() {
            self.selected_index = filter.restore_selection;
        }
    }

    /// Moves the selection onto a row the query leaves on screen, so typing never
    /// leaves it pointing at something hidden. A query that matches nothing has
    /// nowhere to put it and leaves it where it was, ready for the next backspace
    /// to widen back onto it.
    fn follow_filter(&mut self) {
        let visible = self.visible_rows();
        let Some(position) = visible.match_position_of(self.selected_index.unwrap_or(0)) else {
            return;
        };
        self.selected_index = visible.row(position);
    }

    pub(crate) fn refresh(&mut self) {
        self.start_root_listing();
    }

    pub(crate) fn enter_selected(&mut self) {
        let Some(path) = self
            .effective_selection()
            .and_then(|index| self.rows.get(index))
            .filter(|row| row.entry.navigable)
            .map(|row| row.path.clone())
        else {
            return;
        };
        self.change_root_path(Path::new(&path));
    }

    pub(crate) fn leave(&mut self) {
        let Some(parent) = Path::new(&self.root_path).parent() else {
            return;
        };
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        }
        .to_path_buf();
        self.change_root_path(&parent);
    }

    /// Expands or collapses the selected directory. Files have no activation
    /// behavior.
    pub(crate) fn activate_selected(&mut self) {
        let Some(index) = self.effective_selection() else {
            return;
        };
        if self.rows[index].entry.navigable {
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

    fn change_root_path(&mut self, root_path: &Path) {
        self.root_path = root_path.to_string_lossy().into_owned();
        self.selected_index = None;
        self.viewport_offset = 0;
        self.start_root_listing();
    }

    fn start_root_listing(&mut self) {
        self.abort_tree_listings();
        self.generation = self.generation.wrapping_add(1);
        self.rows.clear();
        self.status = ListingStatus::Loading;
        let client = Arc::clone(&self.client);
        let connection = self.connection;
        let root_path = self.root_path.clone();
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
        match self.rows[index].expansion {
            ExpansionStatus::Collapsed => self.start_directory_listing(index),
            _ => self.collapse_directory(index),
        }
    }

    fn start_directory_listing(&mut self, index: usize) {
        let request_id = self.next_request_id();
        let path = self.rows[index].path.clone();
        self.rows[index].expansion = ExpansionStatus::Loading { request_id };
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
        self.directory_listing_tasks.insert(request_id, task);
    }

    fn collapse_directory(&mut self, index: usize) {
        let end = self.subtree_end(index);
        let request_ids: Vec<_> = self.rows[index..end]
            .iter()
            .filter_map(|row| row.expansion.request_id())
            .collect();
        for request_id in request_ids {
            if let Some(task) = self.directory_listing_tasks.remove(&request_id) {
                task.abort();
            }
        }
        self.rows.drain(index + 1..end);
        self.rows[index].expansion = ExpansionStatus::Collapsed;
        self.selected_index = Some(index);
    }

    fn subtree_end(&self, index: usize) -> usize {
        let depth = self.rows[index].depth;
        self.rows[index + 1..]
            .iter()
            .position(|row| row.depth <= depth)
            .map_or(self.rows.len(), |offset| index + 1 + offset)
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
            ListingResult::Entry(entry) => {
                let path = child_path(&self.root_path, &entry.name);
                self.rows.push(BrowserRow::new(path, 0, entry));
                if self.selected_index.is_none() {
                    self.selected_index = Some(0);
                }
            }
            ListingResult::Finished => {
                self.status = ListingStatus::Complete;
                self.root_listing_task = None;
                self.clamp_selection();
            }
            ListingResult::Failed(error) => {
                self.status = ListingStatus::Failed(error);
                self.root_listing_task = None;
                self.clamp_selection();
            }
        }
    }

    fn apply_directory_listing(
        &mut self,
        parent_path: &str,
        request_id: u64,
        result: ListingResult,
    ) {
        let Some(index) = self.rows.iter().position(|row| {
            row.path == parent_path && row.expansion.request_id() == Some(request_id)
        }) else {
            return;
        };
        match result {
            ListingResult::Entry(entry) => {
                let depth = self.rows[index].depth + 1;
                let path = child_path(parent_path, &entry.name);
                let insertion_index = self.subtree_end(index);
                if let Some(selected_index) = self.selected_index
                    && insertion_index <= selected_index
                {
                    self.selected_index = Some(selected_index + 1);
                }
                self.rows
                    .insert(insertion_index, BrowserRow::new(path, depth, entry));
            }
            ListingResult::Finished => {
                self.directory_listing_tasks.remove(&request_id);
                self.rows[index].expansion = ExpansionStatus::Expanded { request_id };
                self.clamp_selection();
            }
            ListingResult::Failed(error) => {
                self.directory_listing_tasks.remove(&request_id);
                self.rows[index].expansion = ExpansionStatus::Failed { request_id, error };
                self.clamp_selection();
            }
        }
    }

    fn abort_tree_listings(&mut self) {
        if let Some(task) = self.root_listing_task.take() {
            task.abort();
        }
        for (_, task) in self.directory_listing_tasks.drain() {
            task.abort();
        }
    }

    fn next_request_id(&mut self) -> u64 {
        self.next_request_id = self.next_request_id.wrapping_add(1);
        self.next_request_id
    }

    fn clamp_selection(&mut self) {
        self.selected_index = match (self.selected_index, self.rows.len()) {
            (_, 0) => None,
            (Some(index), length) => Some(index.min(length - 1)),
            (None, _) => Some(0),
        };
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
        parent_path: String,
        request_id: u64,
    },
}

#[derive(Debug)]
enum ListingResult {
    Entry(DirectoryEntry),
    Finished,
    Failed(String),
}

async fn run_listing(
    client: Arc<Client>,
    connection: ConnectionId,
    path: String,
    context_id: BrowserContextId,
    generation: u64,
    target: ListingTarget,
    events: mpsc::Sender<BrowserContextEvent>,
) {
    let result = match client.op_list_dir(&connection, &path).await {
        Ok(mut listing) => loop {
            match listing.try_next().await {
                Ok(Some(entry)) => {
                    if send_listing_result(
                        &events,
                        context_id,
                        generation,
                        target.clone(),
                        ListingResult::Entry(entry),
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                }
                Ok(None) => break ListingResult::Finished,
                Err(error) => break ListingResult::Failed(error.to_string()),
            }
        },
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

fn child_path(parent: &str, name: &str) -> String {
    Path::new(parent).join(name).to_string_lossy().into_owned()
}

/// Whether a name matches a filter query, which is a case-insensitive substring.
///
/// Substring rather than a fuzzy subsequence, because completion has to have
/// something to complete to: a subsequence match shares no prefix with what was
/// typed, so `Tab` would have nothing to agree on.
fn matches_needle(name: &str, needle: &str) -> bool {
    name.to_lowercase().contains(needle)
}

#[cfg(test)]
mod tests {
    use std::{io, sync::Arc, time::Duration};

    use crate::{
        Client, ConnectionId, DirectoryEntry, EntryKind,
        test_support::{Listing, ScriptedServer},
    };
    use tokio::sync::mpsc;

    use super::{BrowserContext, BrowserContextEvent, ExpansionStatus, ListingStatus};

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
        assert_eq!(viewport.rows[0].entry.name, "item-0");

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
        assert_eq!(viewport.rows[0].entry.name, "item-1");
        assert_eq!(viewport.selected, Some(4));
        assert_eq!(context.viewport_offset(), 1);

        // The window stops at the end of the list instead of running past it.
        while context.effective_selection() != Some(11) {
            context.move_selection(true);
        }
        let viewport = context.viewport(6);
        assert_eq!(viewport.rows.len(), 6);
        assert_eq!(viewport.rows[5].entry.name, "item-11");
        assert_eq!(viewport.selected, Some(5));
        assert_eq!(context.viewport_offset(), 6);

        // Moving back up scrolls the other way, again keeping a row of context.
        for _ in 0..5 {
            context.move_selection(false);
        }
        let viewport = context.viewport(6);
        assert_eq!(viewport.rows[0].entry.name, "item-5");
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
        assert_eq!(viewport.rows[0].entry.name, "item-2");
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

        context.selected_index = Some(10);
        context.set_viewport(6, 6);
        context.scroll_half_page(true);
        assert_eq!(context.effective_selection(), Some(11));
        assert_eq!(context.viewport_offset(), 6);
        context.scroll_half_page(true);
        assert_eq!(context.effective_selection(), Some(11));
        assert_eq!(context.viewport_offset(), 6);

        context.selected_index = Some(4);
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
        assert_eq!(context.selected_index, Some(2));
        apply_next(&mut context, &mut receiver).await;
        assert_eq!(context.effective_selection(), Some(0));
        apply_next(&mut context, &mut receiver).await;
        assert_eq!(context.effective_selection(), Some(1));
        apply_next(&mut context, &mut receiver).await;
        assert_eq!(context.selected_index, Some(1));
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
        assert_eq!(receiver.len(), 1);
        apply_next(&mut context, &mut receiver).await;
        apply_next(&mut context, &mut receiver).await;
        apply_next(&mut context, &mut receiver).await;

        assert_eq!(context.entries(), [entry("one"), entry("two")]);
        assert_eq!(context.status(), &ListingStatus::Complete);
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
        while matches!(context.rows[0].expansion, ExpansionStatus::Loading { .. }) {
            apply_next(&mut context, &mut receiver).await;
        }

        assert_eq!(
            context
                .rows()
                .iter()
                .map(|row| (row.path.as_str(), row.depth))
                .collect::<Vec<_>>(),
            [("/home/reports", 0), ("/home/reports/first.txt", 1)]
        );
        assert!(matches!(
            context.rows[0].expansion,
            ExpansionStatus::Expanded { .. }
        ));

        context.activate_selected();
        assert_eq!(context.rows().len(), 1);
        assert_eq!(context.rows[0].expansion, ExpansionStatus::Collapsed);

        context.activate_selected();
        while matches!(context.rows[0].expansion, ExpansionStatus::Loading { .. }) {
            apply_next(&mut context, &mut receiver).await;
        }
        assert_eq!(context.rows[1].path, "/home/reports/second.txt");
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
        while !context.directory_listing_tasks.is_empty() {
            apply_next(&mut context, &mut receiver).await;
        }

        assert_eq!(
            context
                .rows()
                .iter()
                .map(|row| (row.entry.name.as_str(), row.depth))
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
            context.rows[context.effective_selection().unwrap()]
                .entry
                .name,
            "root.txt"
        );

        apply_next(&mut context, &mut receiver).await;

        assert_eq!(context.effective_selection(), Some(2));
        assert_eq!(context.rows[2].entry.name, "root.txt");
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
        while matches!(context.rows[0].expansion, ExpansionStatus::Loading { .. }) {
            apply_next(&mut context, &mut receiver).await;
        }
        assert_eq!(context.rows[1].entry.name, "current.txt");
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

        assert_eq!(context.rows.len(), 2);
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
        while matches!(context.rows[0].expansion, ExpansionStatus::Loading { .. }) {
            apply_next(&mut context, &mut receiver).await;
        }
        assert_eq!(context.rows[1].path, "/home/linked-directory/child.txt");

        context.move_selection(true);
        context.move_selection(true);
        context.activate_selected();
        assert_eq!(context.rows.len(), 3);
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
        while matches!(context.rows[0].expansion, ExpansionStatus::Loading { .. }) {
            apply_next(&mut context, &mut receiver).await;
        }
        assert_eq!(
            context.row_names(),
            ["reports", "january.txt", "summary.txt", "root.txt"]
        );
        context
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
