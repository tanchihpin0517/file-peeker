use std::{fmt, io, path::Path, sync::Arc};

use file_peeker_client::{DirectoryEntry, EntryStream, Session};
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

pub(crate) struct BrowserContext {
    id: BrowserContextId,
    root_path: String,
    entries: Vec<DirectoryEntry>,
    selected_index: Option<usize>,
    status: ListingStatus,
    listing_task: Option<JoinHandle<()>>,
    generation: u64,
    source: Arc<dyn BrowserSource>,
    events: mpsc::Sender<BrowserContextEvent>,
}

impl fmt::Debug for BrowserContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserContext")
            .field("id", &self.id)
            .field("root_path", &self.root_path)
            .field("entries", &self.entries)
            .field("selected_index", &self.selected_index)
            .field("status", &self.status)
            .field("generation", &self.generation)
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
            entries: Vec::new(),
            selected_index: None,
            status: ListingStatus::Loading,
            listing_task: None,
            generation: 0,
            source,
            events,
        };
        context.start_listing();
        context
    }

    pub(crate) fn id(&self) -> BrowserContextId {
        self.id
    }

    pub(crate) fn root_path(&self) -> &str {
        &self.root_path
    }

    pub(crate) fn entries(&self) -> &[DirectoryEntry] {
        &self.entries
    }

    pub(crate) fn status(&self) -> &ListingStatus {
        &self.status
    }

    pub(crate) fn effective_selection(&self) -> Option<usize> {
        (!self.entries.is_empty())
            .then(|| self.selected_index.unwrap_or(0).min(self.entries.len() - 1))
    }

    pub(crate) fn move_selection(&mut self, down: bool) {
        if self.entries.is_empty() {
            self.selected_index = None;
            return;
        }
        let last = self.entries.len() - 1;
        let current = self.selected_index.unwrap_or(0).min(last);
        self.selected_index = Some(if down {
            (current + 1).min(last)
        } else {
            current.saturating_sub(1)
        });
    }

    pub(crate) fn refresh(&mut self) {
        self.start_listing();
    }

    pub(crate) fn enter_selected(&mut self) {
        let Some(entry) = self
            .effective_selection()
            .and_then(|index| self.entries.get(index))
            .filter(|entry| entry.navigable)
        else {
            return;
        };
        let root_path = Path::new(&self.root_path).join(&entry.name);
        self.change_root_path(&root_path);
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

    pub(crate) fn apply(&mut self, event: BrowserContextEvent) {
        if event.context_id != self.id || event.generation != self.generation {
            return;
        }
        match event.result {
            ListingResult::Entry(entry) => self.append_entry(entry),
            ListingResult::Finished => {
                self.status = ListingStatus::Complete;
                self.listing_task = None;
                self.clamp_selection();
            }
            ListingResult::Failed(error) => {
                self.status = ListingStatus::Failed(error);
                self.listing_task = None;
                self.clamp_selection();
            }
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.abort_listing();
    }

    fn change_root_path(&mut self, root_path: &Path) {
        self.root_path = root_path.to_string_lossy().into_owned();
        self.selected_index = None;
        self.start_listing();
    }

    fn start_listing(&mut self) {
        self.abort_listing();
        self.generation = self.generation.wrapping_add(1);
        self.entries.clear();
        self.status = ListingStatus::Loading;
        let source = Arc::clone(&self.source);
        let root_path = self.root_path.clone();
        let context_id = self.id;
        let generation = self.generation;
        let events = self.events.clone();
        self.listing_task = Some(tokio::spawn(run_listing(
            source, root_path, context_id, generation, events,
        )));
    }

    fn abort_listing(&mut self) {
        if let Some(task) = self.listing_task.take() {
            task.abort();
        }
    }

    fn append_entry(&mut self, entry: DirectoryEntry) {
        self.entries.push(entry);
        if self.selected_index.is_none() {
            self.selected_index = Some(0);
        }
    }

    fn clamp_selection(&mut self) {
        self.selected_index = match (self.selected_index, self.entries.len()) {
            (_, 0) => None,
            (Some(index), length) => Some(index.min(length - 1)),
            (None, _) => Some(0),
        };
    }
}

impl Drop for BrowserContext {
    fn drop(&mut self) {
        self.abort_listing();
    }
}

trait BrowserSource: fmt::Debug + Send + Sync {
    fn list_dir(self: Arc<Self>, path: String) -> BoxFuture<'static, io::Result<EntryStream>>;
}

impl BrowserSource for Session {
    fn list_dir(self: Arc<Self>, path: String) -> BoxFuture<'static, io::Result<EntryStream>> {
        async move { self.op_list_dir(&path).await }.boxed()
    }
}

#[derive(Debug)]
pub(crate) struct BrowserContextEvent {
    context_id: BrowserContextId,
    generation: u64,
    result: ListingResult,
}

impl BrowserContextEvent {
    pub(crate) fn context_id(&self) -> BrowserContextId {
        self.context_id
    }
}

#[derive(Debug)]
enum ListingResult {
    Entry(DirectoryEntry),
    Finished,
    Failed(String),
}

async fn run_listing(
    source: Arc<dyn BrowserSource>,
    root_path: String,
    context_id: BrowserContextId,
    generation: u64,
    events: mpsc::Sender<BrowserContextEvent>,
) {
    let result = match source.list_dir(root_path).await {
        Ok(mut listing) => loop {
            match listing.try_next().await {
                Ok(Some(entry)) => {
                    if send_result(&events, context_id, generation, ListingResult::Entry(entry))
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
    let _ = send_result(&events, context_id, generation, result).await;
}

async fn send_result(
    events: &mpsc::Sender<BrowserContextEvent>,
    context_id: BrowserContextId,
    generation: u64,
    result: ListingResult,
) -> Result<(), mpsc::error::SendError<BrowserContextEvent>> {
    events
        .send(BrowserContextEvent {
            context_id,
            generation,
            result,
        })
        .await
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

    use super::{BrowserContext, BrowserContextEvent, BrowserSource, ListingStatus};

    struct ScriptedBrowserSource {
        listings: Mutex<VecDeque<io::Result<EntryStream>>>,
        requested_paths: Mutex<Vec<String>>,
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
            })
        }

        fn requested_paths(&self) -> Vec<String> {
            self.requested_paths.lock().unwrap().clone()
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
    async fn dropping_a_context_cancels_its_listing() {
        let source = ScriptedBrowserSource::new([Ok(stream::pending().boxed())]);
        let (context, _receiver) = context(source, 1);
        let task = context.listing_task.as_ref().unwrap().abort_handle();

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
