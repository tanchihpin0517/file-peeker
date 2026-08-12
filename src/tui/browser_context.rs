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
#[derive(Debug)]
pub(crate) struct Viewport<'a> {
    pub(crate) rows: &'a [BrowserRow],
    pub(crate) selected: Option<usize>,
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
        let Some(selected) = self.effective_selection().filter(|_| height > 0) else {
            return Viewport {
                rows: &[],
                selected: None,
            };
        };

        // Keep a row of context past the selection wherever the list allows it,
        // so moving off the edge of the viewport scrolls before it has to.
        let padding = SCROLL_PADDING.min(height.saturating_sub(1) / 2);
        let max_offset = self.rows.len().saturating_sub(height);
        let mut offset = self
            .viewport_offset
            .min(max_offset)
            .min(selected.saturating_sub(padding));
        let bottom = (selected + padding).min(self.rows.len() - 1);
        if bottom >= offset + height {
            offset = bottom + 1 - height;
        }
        self.viewport_offset = offset;

        let end = (offset + height).min(self.rows.len());
        Viewport {
            rows: &self.rows[offset..end],
            selected: Some(selected - offset),
        }
    }

    pub(crate) fn move_selection(&mut self, down: bool) {
        if self.rows.is_empty() {
            self.selected_index = None;
            return;
        }
        let last = self.rows.len() - 1;
        let current = self.selected_index.unwrap_or(0).min(last);
        self.selected_index = Some(if down {
            (current + 1).min(last)
        } else {
            current.saturating_sub(1)
        });
    }

    pub(crate) fn scroll_half_page(&mut self, down: bool) {
        let Some(current) = self.effective_selection() else {
            return;
        };
        if self.viewport_height == 0 {
            return;
        }

        let distance = (self.viewport_height / 2).max(1);
        let last = self.rows.len() - 1;
        let max_offset = self.rows.len().saturating_sub(self.viewport_height);
        if down {
            self.selected_index = Some(current.saturating_add(distance).min(last));
            self.viewport_offset = self
                .viewport_offset
                .saturating_add(distance)
                .min(max_offset);
        } else {
            self.selected_index = Some(current.saturating_sub(distance));
            self.viewport_offset = self.viewport_offset.saturating_sub(distance);
        }
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
