use std::{fmt, io, sync::Arc};

use file_peeker_client::{DirectoryEntry, ListStream, Session};
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
    path: String,
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
            .field("path", &self.path)
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
        path: String,
        events: mpsc::Sender<BrowserContextEvent>,
    ) -> Self {
        Self::from_source(session, path, events)
    }

    fn from_source(
        source: Arc<dyn BrowserSource>,
        path: String,
        events: mpsc::Sender<BrowserContextEvent>,
    ) -> Self {
        let mut context = Self {
            id: BrowserContextId::new(),
            path,
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

    pub(crate) fn path(&self) -> &str {
        &self.path
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

    pub(crate) fn apply(&mut self, event: BrowserContextEvent) {
        if event.context_id != self.id || event.generation != self.generation {
            return;
        }
        match event.result {
            ListingResult::Batch(entries) => self.append_entries(entries),
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

    fn start_listing(&mut self) {
        self.abort_listing();
        self.generation = self.generation.wrapping_add(1);
        self.entries.clear();
        self.status = ListingStatus::Loading;
        let source = Arc::clone(&self.source);
        let path = self.path.clone();
        let context_id = self.id;
        let generation = self.generation;
        let events = self.events.clone();
        self.listing_task = Some(tokio::spawn(run_listing(
            source, path, context_id, generation, events,
        )));
    }

    fn abort_listing(&mut self) {
        if let Some(task) = self.listing_task.take() {
            task.abort();
        }
    }

    fn append_entries(&mut self, entries: Vec<DirectoryEntry>) {
        self.entries.extend(entries);
        if self.selected_index.is_none() && !self.entries.is_empty() {
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
    fn list(self: Arc<Self>, path: String) -> BoxFuture<'static, io::Result<ListStream>>;
}

impl BrowserSource for Session {
    fn list(self: Arc<Self>, path: String) -> BoxFuture<'static, io::Result<ListStream>> {
        async move { self.op_list(&path).await }.boxed()
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
    Batch(Vec<DirectoryEntry>),
    Finished,
    Failed(String),
}

async fn run_listing(
    source: Arc<dyn BrowserSource>,
    path: String,
    context_id: BrowserContextId,
    generation: u64,
    events: mpsc::Sender<BrowserContextEvent>,
) {
    let result = match source.list(path).await {
        Ok(mut listing) => loop {
            match listing.try_next().await {
                Ok(Some(entries)) => {
                    if send_result(
                        &events,
                        context_id,
                        generation,
                        ListingResult::Batch(entries),
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

    use file_peeker_client::{DirectoryEntry, EntryKind, ListStream};
    use futures::{FutureExt as _, StreamExt as _, future::BoxFuture, stream};
    use tokio::sync::mpsc;

    use super::{BrowserContext, BrowserContextEvent, BrowserSource, ListingStatus};

    struct ScriptedBrowserSource {
        listings: Mutex<VecDeque<io::Result<ListStream>>>,
    }

    impl std::fmt::Debug for ScriptedBrowserSource {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("ScriptedBrowserSource")
                .finish_non_exhaustive()
        }
    }

    impl ScriptedBrowserSource {
        fn new(listings: impl IntoIterator<Item = io::Result<ListStream>>) -> Arc<Self> {
            Arc::new(Self {
                listings: Mutex::new(listings.into_iter().collect()),
            })
        }
    }

    impl BrowserSource for ScriptedBrowserSource {
        fn list(self: Arc<Self>, _path: String) -> BoxFuture<'static, io::Result<ListStream>> {
            async move {
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

    fn listing<I>(batches: I) -> ListStream
    where
        I: IntoIterator<Item = io::Result<Vec<DirectoryEntry>>>,
        I::IntoIter: Send + 'static,
    {
        stream::iter(batches).boxed()
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
        let source = ScriptedBrowserSource::new([Ok(listing([Ok(vec![entry("one")])]))]);
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
            Ok(vec![entry("kept")]),
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
            Ok(listing([Ok(vec![entry("stale")])])),
            Ok(listing([Ok(vec![entry("current")])])),
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
            Ok(listing([Ok(vec![entry("a"), entry("b"), entry("c")])])),
            Ok(listing([Ok(vec![entry("new-a"), entry("new-b")])])),
        ]);
        let (mut context, mut receiver) = context(source, 8);
        apply_next(&mut context, &mut receiver).await;
        apply_next(&mut context, &mut receiver).await;
        context.move_selection(true);
        context.move_selection(true);

        context.refresh();
        assert!(context.entries().is_empty());
        assert_eq!(context.selected_index, Some(2));
        apply_next(&mut context, &mut receiver).await;
        assert_eq!(context.effective_selection(), Some(1));
        apply_next(&mut context, &mut receiver).await;
        assert_eq!(context.selected_index, Some(1));
    }

    #[tokio::test]
    async fn bounded_events_backpressure_the_listing() {
        let source = ScriptedBrowserSource::new([Ok(listing([
            Ok(vec![entry("one")]),
            Ok(vec![entry("two")]),
        ]))]);
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
