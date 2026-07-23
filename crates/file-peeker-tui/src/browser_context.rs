use std::{collections::HashMap, fmt, io, path::Path, sync::Arc};

use file_peeker_client::{DirectoryEntry, EntryKind, EntryStream, Session};
use futures::{FutureExt as _, TryStreamExt as _, future::BoxFuture};
use tokio::{sync::mpsc, task::JoinHandle};
use uuid::Uuid;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OpenStatus {
    Idle,
    Opening(String),
    Opened(String),
    Failed { path: String, error: String },
}

pub(crate) struct BrowserContext {
    id: BrowserContextId,
    root_path: String,
    rows: Vec<BrowserRow>,
    selected_index: Option<usize>,
    status: ListingStatus,
    root_listing_task: Option<JoinHandle<()>>,
    directory_listing_tasks: HashMap<u64, JoinHandle<()>>,
    generation: u64,
    next_request_id: u64,
    pending_open_path: Option<String>,
    active_open_request: Option<u64>,
    open_status: OpenStatus,
    open_task: Option<JoinHandle<()>>,
    source: Arc<dyn BrowserSource>,
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
            .field("status", &self.status)
            .field("generation", &self.generation)
            .field("pending_open_path", &self.pending_open_path)
            .field("open_status", &self.open_status)
            .finish_non_exhaustive()
    }
}

impl BrowserContext {
    pub(crate) fn new(
        session: Arc<Session>,
        root_path: String,
        events: mpsc::Sender<BrowserContextEvent>,
    ) -> Self {
        Self::from_source(session, root_path, events)
    }

    fn from_source(
        source: Arc<dyn BrowserSource>,
        root_path: String,
        events: mpsc::Sender<BrowserContextEvent>,
    ) -> Self {
        let mut context = Self {
            id: BrowserContextId::new(),
            root_path,
            rows: Vec::new(),
            selected_index: None,
            status: ListingStatus::Loading,
            root_listing_task: None,
            directory_listing_tasks: HashMap::new(),
            generation: 0,
            next_request_id: 0,
            pending_open_path: None,
            active_open_request: None,
            open_status: OpenStatus::Idle,
            open_task: None,
            source,
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

    pub(crate) fn pending_open_path(&self) -> Option<&str> {
        self.pending_open_path.as_deref()
    }

    pub(crate) fn open_status(&self) -> &OpenStatus {
        &self.open_status
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

    pub(crate) fn move_selection(&mut self, down: bool) {
        self.clear_settled_open_status();
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

    pub(crate) fn refresh(&mut self) {
        self.pending_open_path = None;
        self.clear_settled_open_status();
        self.start_root_listing();
    }

    pub(crate) fn enter_selected(&mut self) {
        self.clear_settled_open_status();
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
        self.clear_settled_open_status();
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

    pub(crate) fn activate_selected(&mut self) {
        self.clear_settled_open_status();
        let Some(index) = self.effective_selection() else {
            return;
        };
        if self.rows[index].entry.navigable {
            self.toggle_directory(index);
            return;
        }
        if matches!(
            self.rows[index].entry.kind,
            EntryKind::File | EntryKind::Symlink
        ) && !matches!(self.open_status, OpenStatus::Opening(_))
        {
            self.pending_open_path = Some(self.rows[index].path.clone());
        }
    }

    pub(crate) fn cancel_open_confirmation(&mut self) {
        self.pending_open_path = None;
    }

    pub(crate) fn confirm_open(&mut self) {
        let Some(path) = self.pending_open_path.take() else {
            return;
        };
        if matches!(self.open_status, OpenStatus::Opening(_)) {
            return;
        }
        let request_id = self.next_request_id();
        self.active_open_request = Some(request_id);
        self.open_status = OpenStatus::Opening(path.clone());
        let source = Arc::clone(&self.source);
        let context_id = self.id;
        let events = self.events.clone();
        self.open_task = Some(tokio::spawn(run_open_file(
            source, path, context_id, request_id, events,
        )));
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
            BrowserEvent::Open {
                request_id,
                path,
                result,
            } if self.active_open_request == Some(request_id) => {
                self.active_open_request = None;
                self.open_task = None;
                self.open_status = match result {
                    OpenResult::Finished => OpenStatus::Opened(path),
                    OpenResult::Failed(error) => OpenStatus::Failed { path, error },
                };
            }
            _ => {}
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.abort_tree_listings();
        if let Some(task) = self.open_task.take() {
            task.abort();
        }
        self.active_open_request = None;
        self.pending_open_path = None;
        self.open_status = OpenStatus::Idle;
    }

    fn change_root_path(&mut self, root_path: &Path) {
        self.root_path = root_path.to_string_lossy().into_owned();
        self.selected_index = None;
        self.pending_open_path = None;
        self.start_root_listing();
    }

    fn start_root_listing(&mut self) {
        self.abort_tree_listings();
        self.generation = self.generation.wrapping_add(1);
        self.rows.clear();
        self.status = ListingStatus::Loading;
        let source = Arc::clone(&self.source);
        let root_path = self.root_path.clone();
        let context_id = self.id;
        let generation = self.generation;
        let events = self.events.clone();
        self.root_listing_task = Some(tokio::spawn(run_listing(
            source,
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
        let source = Arc::clone(&self.source);
        let context_id = self.id;
        let generation = self.generation;
        let target = ListingTarget::Directory {
            parent_path: path.clone(),
            request_id,
        };
        let events = self.events.clone();
        let task = tokio::spawn(run_listing(
            source, path, context_id, generation, target, events,
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

    fn clear_settled_open_status(&mut self) {
        if !matches!(self.open_status, OpenStatus::Opening(_)) {
            self.open_status = OpenStatus::Idle;
        }
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
        if let Some(task) = self.open_task.take() {
            task.abort();
        }
    }
}

trait BrowserSource: fmt::Debug + Send + Sync {
    fn list_dir(self: Arc<Self>, path: String) -> BoxFuture<'static, io::Result<EntryStream>>;
    fn open_file(self: Arc<Self>, path: String) -> BoxFuture<'static, io::Result<()>>;
}

impl BrowserSource for Session {
    fn list_dir(self: Arc<Self>, path: String) -> BoxFuture<'static, io::Result<EntryStream>> {
        async move { self.op_list_dir(&path).await }.boxed()
    }

    fn open_file(self: Arc<Self>, path: String) -> BoxFuture<'static, io::Result<()>> {
        async move { self.op_open_file(&path).await }.boxed()
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
    Open {
        request_id: u64,
        path: String,
        result: OpenResult,
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
    source: Arc<dyn BrowserSource>,
    path: String,
    context_id: BrowserContextId,
    generation: u64,
    target: ListingTarget,
    events: mpsc::Sender<BrowserContextEvent>,
) {
    let result = match source.list_dir(path).await {
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

#[derive(Debug)]
enum OpenResult {
    Finished,
    Failed(String),
}

async fn run_open_file(
    source: Arc<dyn BrowserSource>,
    path: String,
    context_id: BrowserContextId,
    request_id: u64,
    events: mpsc::Sender<BrowserContextEvent>,
) {
    let result = match source.open_file(path.clone()).await {
        Ok(()) => OpenResult::Finished,
        Err(error) => OpenResult::Failed(error.to_string()),
    };
    let _ = events
        .send(BrowserContextEvent {
            context_id,
            kind: BrowserEvent::Open {
                request_id,
                path,
                result,
            },
        })
        .await;
}

fn child_path(parent: &str, name: &str) -> String {
    Path::new(parent).join(name).to_string_lossy().into_owned()
}
#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use file_peeker_client::{DirectoryEntry, EntryKind, EntryStream};
    use futures::{FutureExt as _, StreamExt as _, future::BoxFuture, stream};
    use tokio::sync::mpsc;

    use super::{
        BrowserContext, BrowserContextEvent, BrowserSource, ExpansionStatus, ListingStatus,
        OpenStatus,
    };

    struct ScriptedBrowserSource {
        listings: Mutex<VecDeque<io::Result<EntryStream>>>,
        requested_paths: Mutex<Vec<String>>,
        open_results: Mutex<VecDeque<io::Result<()>>>,
        opened_paths: Mutex<Vec<String>>,
    }

    impl std::fmt::Debug for ScriptedBrowserSource {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("ScriptedBrowserSource")
                .finish_non_exhaustive()
        }
    }

    impl ScriptedBrowserSource {
        fn new(listings: impl IntoIterator<Item = io::Result<EntryStream>>) -> Arc<Self> {
            Arc::new(Self {
                listings: Mutex::new(listings.into_iter().collect()),
                requested_paths: Mutex::new(Vec::new()),
                open_results: Mutex::new(VecDeque::new()),
                opened_paths: Mutex::new(Vec::new()),
            })
        }

        fn with_open_results(
            listings: impl IntoIterator<Item = io::Result<EntryStream>>,
            open_results: impl IntoIterator<Item = io::Result<()>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                listings: Mutex::new(listings.into_iter().collect()),
                requested_paths: Mutex::new(Vec::new()),
                open_results: Mutex::new(open_results.into_iter().collect()),
                opened_paths: Mutex::new(Vec::new()),
            })
        }

        fn requested_paths(&self) -> Vec<String> {
            self.requested_paths.lock().unwrap().clone()
        }

        fn opened_paths(&self) -> Vec<String> {
            self.opened_paths.lock().unwrap().clone()
        }
    }

    impl BrowserSource for ScriptedBrowserSource {
        fn list_dir(self: Arc<Self>, path: String) -> BoxFuture<'static, io::Result<EntryStream>> {
            async move {
                self.requested_paths.lock().unwrap().push(path);
                self.listings
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| Err(io::Error::other("no scripted listing")))
            }
            .boxed()
        }

        fn open_file(self: Arc<Self>, path: String) -> BoxFuture<'static, io::Result<()>> {
            async move {
                self.opened_paths.lock().unwrap().push(path);
                self.open_results
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Ok(()))
            }
            .boxed()
        }
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

    fn listing<I>(entries: I) -> EntryStream
    where
        I: IntoIterator<Item = io::Result<DirectoryEntry>>,
        I::IntoIter: Send + 'static,
    {
        stream::iter(entries).boxed()
    }

    fn context(
        source: Arc<ScriptedBrowserSource>,
        capacity: usize,
    ) -> (BrowserContext, mpsc::Receiver<BrowserContextEvent>) {
        let (events, receiver) = mpsc::channel(capacity);
        (
            BrowserContext::from_source(source, "/home".into(), events),
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
        let source = ScriptedBrowserSource::new([Ok(listing([Ok(entry("one"))]))]);
        let (mut context, mut receiver) = context(source, 4);

        assert_eq!(context.status(), &ListingStatus::Loading);
        apply_next(&mut context, &mut receiver).await;
        apply_next(&mut context, &mut receiver).await;

        assert_eq!(context.entries(), [entry("one")]);
        assert_eq!(context.status(), &ListingStatus::Complete);
        assert_eq!(context.effective_selection(), Some(0));
    }

    #[tokio::test]
    async fn terminal_failure_preserves_partial_entries() {
        let source = ScriptedBrowserSource::new([Ok(listing([
            Ok(entry("kept")),
            Err(io::Error::other("failed")),
        ]))]);
        let (mut context, mut receiver) = context(source, 4);

        apply_next(&mut context, &mut receiver).await;
        apply_next(&mut context, &mut receiver).await;

        assert_eq!(context.entries(), [entry("kept")]);
        assert_eq!(context.status(), &ListingStatus::Failed("failed".into()));
    }

    #[tokio::test]
    async fn refresh_clears_entries_and_rejects_queued_stale_results() {
        let source = ScriptedBrowserSource::new([
            Ok(listing([Ok(entry("stale"))])),
            Ok(listing([Ok(entry("current"))])),
        ]);
        let (mut context, mut receiver) = context(source, 8);
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
        let source = ScriptedBrowserSource::new([
            Ok(listing([Ok(entry("a")), Ok(entry("b")), Ok(entry("c"))])),
            Ok(listing([Ok(entry("new-a")), Ok(entry("new-b"))])),
        ]);
        let (mut context, mut receiver) = context(source, 8);
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
        let source = ScriptedBrowserSource::new([
            Ok(listing([Ok(directory("reports"))])),
            Ok(listing([Ok(entry("current"))])),
        ]);
        let (mut context, mut receiver) = context(Arc::clone(&source), 8);
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
        let source = ScriptedBrowserSource::new([Ok(listing([Ok(entry("report.txt"))]))]);
        let (mut context, mut receiver) = context(Arc::clone(&source), 4);
        apply_next(&mut context, &mut receiver).await;

        context.enter_selected();

        assert_eq!(context.root_path(), "/home");
        assert_eq!(context.entries(), [entry("report.txt")]);
        assert_eq!(source.requested_paths(), ["/home"]);
    }

    #[tokio::test]
    async fn leaving_changes_to_parent_and_does_nothing_at_root() {
        let source = ScriptedBrowserSource::new([
            Ok(listing([Ok(entry("initial"))])),
            Ok(listing([Ok(entry("parent"))])),
        ]);
        let (mut context, mut receiver) = context(Arc::clone(&source), 8);
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
        let source = ScriptedBrowserSource::new([
            Ok(listing([Ok(directory("missing"))])),
            Err(io::Error::new(io::ErrorKind::NotFound, "missing directory")),
        ]);
        let (mut context, mut receiver) = context(source, 8);
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
        let source =
            ScriptedBrowserSource::new([Ok(listing([Ok(entry("one")), Ok(entry("two"))]))]);
        let (mut context, mut receiver) = context(source, 1);

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
        let source = ScriptedBrowserSource::new([
            Ok(listing([Ok(directory("reports"))])),
            Ok(listing([Ok(entry("first.txt"))])),
            Ok(listing([Ok(entry("second.txt"))])),
        ]);
        let (mut context, mut receiver) = context(Arc::clone(&source), 8);
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
        let source = ScriptedBrowserSource::new([
            Ok(listing([Ok(directory("reports")), Ok(entry("root.txt"))])),
            Ok(listing([Ok(directory("year")), Ok(entry("summary.txt"))])),
            Ok(listing([Ok(entry("january.txt"))])),
        ]);
        let (mut context, mut receiver) = context(source, 16);
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
        let source = ScriptedBrowserSource::new([
            Ok(listing([Ok(directory("reports")), Ok(entry("root.txt"))])),
            Ok(listing([Ok(entry("child.txt"))])),
        ]);
        let (mut context, mut receiver) = context(source, 8);
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
        let source = ScriptedBrowserSource::new([
            Ok(listing([Ok(directory("reports"))])),
            Ok(listing([Ok(entry("stale.txt"))])),
            Ok(listing([Ok(entry("current.txt"))])),
        ]);
        let (mut context, mut receiver) = context(source, 8);
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
    async fn files_and_non_directory_symlinks_require_confirmation_before_opening() {
        let source = ScriptedBrowserSource::with_open_results(
            [Ok(listing([
                Ok(entry("report.txt")),
                Ok(symlink("latest", false)),
            ]))],
            [
                Ok(()),
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "opener denied request",
                )),
            ],
        );
        let (mut context, mut receiver) = context(Arc::clone(&source), 8);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.activate_selected();
        assert_eq!(context.pending_open_path(), Some("/home/report.txt"));
        assert!(source.opened_paths().is_empty());
        context.confirm_open();
        assert_eq!(
            context.open_status(),
            &OpenStatus::Opening("/home/report.txt".into())
        );
        apply_next(&mut context, &mut receiver).await;
        assert_eq!(
            context.open_status(),
            &OpenStatus::Opened("/home/report.txt".into())
        );

        context.move_selection(true);
        context.activate_selected();
        assert_eq!(context.pending_open_path(), Some("/home/latest"));
        context.confirm_open();
        apply_next(&mut context, &mut receiver).await;
        assert_eq!(
            context.open_status(),
            &OpenStatus::Failed {
                path: "/home/latest".into(),
                error: "opener denied request".into(),
            }
        );
        assert_eq!(source.opened_paths(), ["/home/report.txt", "/home/latest"]);
    }

    #[tokio::test]
    async fn navigable_symlinks_expand_and_special_entries_do_nothing() {
        let special = DirectoryEntry {
            name: "socket".into(),
            kind: EntryKind::Other,
            navigable: false,
        };
        let source = ScriptedBrowserSource::new([
            Ok(listing([
                Ok(symlink("linked-directory", true)),
                Ok(special),
            ])),
            Ok(listing([Ok(entry("child.txt"))])),
        ]);
        let (mut context, mut receiver) = context(Arc::clone(&source), 8);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.activate_selected();
        assert_eq!(context.pending_open_path(), None);
        while matches!(context.rows[0].expansion, ExpansionStatus::Loading { .. }) {
            apply_next(&mut context, &mut receiver).await;
        }
        assert_eq!(context.rows[1].path, "/home/linked-directory/child.txt");

        context.move_selection(true);
        context.move_selection(true);
        context.activate_selected();
        assert_eq!(context.pending_open_path(), None);
        assert!(source.opened_paths().is_empty());
    }

    #[tokio::test]
    async fn cancelling_confirmation_does_not_open_the_file() {
        let source = ScriptedBrowserSource::new([Ok(listing([Ok(entry("report.txt"))]))]);
        let (mut context, mut receiver) = context(Arc::clone(&source), 4);
        while context.status() != &ListingStatus::Complete {
            apply_next(&mut context, &mut receiver).await;
        }

        context.activate_selected();
        context.cancel_open_confirmation();

        assert_eq!(context.pending_open_path(), None);
        assert!(source.opened_paths().is_empty());
    }

    #[tokio::test]
    async fn dropping_a_context_cancels_its_listing() {
        let source = ScriptedBrowserSource::new([Ok(stream::pending().boxed())]);
        let (context, _receiver) = context(source, 1);
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
