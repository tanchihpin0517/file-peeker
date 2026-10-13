//! The registry, admission, the tree and retention.
//!
//! Every task has a driver: a tokio task that admits it, runs its body, waits
//! for its children and settles it. The registry is behind a `std::sync` lock,
//! so a synchronous caller — a render frame — can read it; no critical section
//! holds an `await`.

use std::{
    any::Any,
    collections::{BTreeSet, HashMap, VecDeque},
    io,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime},
};

use tokio::sync::{Notify, broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{
    AdmissionClass, Handle, Progress, ResourceId, ResourceState, TaskContext, TaskError, TaskEvent,
    TaskId, TaskInfo, TaskKind, TaskState,
};

/// The runtime's policy. The defaults are the proposal's.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    /// Root tasks of a class admitted at once per scope; `None` is unbounded.
    pub interactive_cap: Option<usize>,
    pub transfer_cap: Option<usize>,
    pub background_cap: Option<usize>,
    /// Settled root tasks kept, with their subtrees, for "which failed".
    pub retained_roots: usize,
    /// The shortest gap between two `Progressed` events for one task.
    pub progress_interval: Duration,
    /// Batches a task may emit ahead of its reader.
    pub item_buffer: usize,
    /// Events a slow subscriber may fall behind by before it lags.
    pub event_buffer: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interactive_cap: None,
            transfer_cap: Some(2),
            background_cap: Some(1),
            retained_roots: 256,
            progress_interval: Duration::from_millis(100),
            item_buffer: 16,
            event_buffer: 1024,
        }
    }
}

impl Config {
    fn cap(&self, class: AdmissionClass) -> Option<usize> {
        match class {
            AdmissionClass::Interactive => self.interactive_cap,
            AdmissionClass::Transfer => self.transfer_cap,
            AdmissionClass::Background => self.background_cap,
        }
    }
}

/// The per-process owner of every task. Cheap to clone; clones share one
/// registry.
#[derive(Clone)]
pub(crate) struct Runtime {
    inner: Arc<Inner>,
}

struct Inner {
    config: Config,
    registry: Mutex<Registry>,
    events: broadcast::Sender<TaskEvent>,
    /// Wakes every driver waiting on admission or on its children whenever
    /// anything they could be waiting for changes. Each re-checks its own
    /// condition, so a spurious wake costs one lock.
    changed: Notify,
}

type Scope = (AdmissionClass, Option<ResourceId>);

#[derive(Default)]
struct Registry {
    next_seq: u64,
    tasks: HashMap<TaskId, Entry>,
    settled_roots: VecDeque<TaskId>,
    resources: HashMap<ResourceId, Resource>,
    admitted: HashMap<Scope, usize>,
    /// Capped roots not yet admitted, by submission order, per scope. Only the
    /// first may take a free place, so a queue drains in the order it filled —
    /// which a wake-everyone notify would not otherwise guarantee.
    queues: HashMap<Scope, BTreeSet<u64>>,
}

struct Entry {
    seq: u64,
    info: TaskInfo,
    children: Vec<TaskId>,
    live_children: usize,
    token: CancellationToken,
    throttle: Throttle,
}

#[derive(Default)]
struct Throttle {
    last_sent: Option<Instant>,
    pending: bool,
    flush_scheduled: bool,
}

struct Resource {
    state: ResourceState,
    value: Option<Arc<dyn Any + Send + Sync>>,
}

enum Admission {
    Admit,
    Wait(Option<ResourceId>),
    Fail(io::Error),
}

impl Runtime {
    pub(crate) fn new(config: Config) -> Self {
        let (events, _) = broadcast::channel(config.event_buffer);
        Self {
            inner: Arc::new(Inner {
                config,
                registry: Mutex::new(Registry::default()),
                events,
                changed: Notify::new(),
            }),
        }
    }

    /// Submits a root task. It is registered, and its `Submitted` event sent,
    /// before this returns.
    pub(crate) fn submit<K: TaskKind>(&self, kind: K) -> Handle<K> {
        self.start(kind, None, CancellationToken::new())
    }

    pub(super) fn submit_child<K: TaskKind>(
        &self,
        parent: TaskId,
        parent_token: &CancellationToken,
        kind: K,
    ) -> Handle<K> {
        self.start(kind, Some(parent), parent_token.child_token())
    }

    /// Cancels a task and its subtree. Idempotent, and a no-op for an unknown
    /// or already settled task: a caller holding an ID past its task's end need
    /// not check first.
    pub(crate) fn cancel(&self, id: TaskId) {
        let registry = self.lock();
        if let Some(entry) = registry.tasks.get(&id)
            && !entry.info.state.is_terminal()
        {
            entry.token.cancel();
        }
    }

    /// Cancels every task that has not settled.
    pub(crate) fn cancel_all(&self) {
        let registry = self.lock();
        for entry in registry.tasks.values() {
            if !entry.info.state.is_terminal() {
                entry.token.cancel();
            }
        }
    }

    /// Every retained task, in submission order.
    pub(crate) fn tasks(&self) -> Vec<TaskInfo> {
        let registry = self.lock();
        let mut entries: Vec<&Entry> = registry.tasks.values().collect();
        entries.sort_by_key(|entry| entry.seq);
        entries
            .into_iter()
            .map(|entry| entry.info.clone())
            .collect()
    }

    pub(crate) fn task(&self, id: TaskId) -> Option<TaskInfo> {
        self.lock().tasks.get(&id).map(|entry| entry.info.clone())
    }

    /// Every event from now on. A subscriber that falls more than
    /// [`Config::event_buffer`] behind lags rather than blocking a task; it
    /// recovers with a [`Self::tasks`] snapshot.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.inner.events.subscribe()
    }

    pub(crate) fn add_resource(&self, state: ResourceState) -> ResourceId {
        let id = ResourceId::new();
        self.lock()
            .resources
            .insert(id, Resource { state, value: None });
        self.inner.changed.notify_waiters();
        id
    }

    /// Moves a resource to `state`. Leaving `Ready` drops its value.
    pub(crate) fn set_resource_state(&self, id: ResourceId, state: ResourceState) {
        if let Some(resource) = self.lock().resources.get_mut(&id) {
            resource.state = state;
            if state != ResourceState::Ready {
                resource.value = None;
            }
        }
        self.inner.changed.notify_waiters();
    }

    /// Makes a resource `Ready`, holding `value` for the tasks that require it.
    pub(crate) fn set_resource_ready<R: Any + Send + Sync>(&self, id: ResourceId, value: R) {
        if let Some(resource) = self.lock().resources.get_mut(&id) {
            resource.state = ResourceState::Ready;
            resource.value = Some(Arc::new(value));
        }
        self.inner.changed.notify_waiters();
    }

    pub(super) fn resource_value<R: Any + Send + Sync>(&self, id: ResourceId) -> Option<Arc<R>> {
        let registry = self.lock();
        let resource = registry.resources.get(&id)?;
        if resource.state != ResourceState::Ready {
            return None;
        }
        Arc::clone(resource.value.as_ref()?).downcast::<R>().ok()
    }

    pub(super) fn report_progress(&self, id: TaskId, progress: Progress) {
        let interval = self.inner.config.progress_interval;
        let mut registry = self.lock();
        let Some(entry) = registry.tasks.get_mut(&id) else {
            return;
        };
        if entry.info.state.is_terminal() {
            return;
        }
        entry.info.progress = progress;
        let now = Instant::now();
        let due = entry
            .throttle
            .last_sent
            .is_none_or(|last| now.duration_since(last) >= interval);
        if due {
            entry.throttle.last_sent = Some(now);
            entry.throttle.pending = false;
            self.send(TaskEvent::Progressed {
                id,
                progress: entry.info.progress.clone(),
            });
            return;
        }
        entry.throttle.pending = true;
        if !entry.throttle.flush_scheduled {
            entry.throttle.flush_scheduled = true;
            let wait = interval.saturating_sub(
                entry
                    .throttle
                    .last_sent
                    .map_or(Duration::ZERO, |last| now.duration_since(last)),
            );
            let runtime = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(wait).await;
                runtime.flush_progress(id);
            });
        }
    }

    fn flush_progress(&self, id: TaskId) {
        let mut registry = self.lock();
        let Some(entry) = registry.tasks.get_mut(&id) else {
            return;
        };
        entry.throttle.flush_scheduled = false;
        if entry.throttle.pending && !entry.info.state.is_terminal() {
            entry.throttle.pending = false;
            entry.throttle.last_sent = Some(Instant::now());
            self.send(TaskEvent::Progressed {
                id,
                progress: entry.info.progress.clone(),
            });
        }
    }

    fn lock(&self) -> MutexGuard<'_, Registry> {
        // A panic while holding the lock leaves the registry as consistent as
        // the step it was on; every task's driver would otherwise hang behind it.
        self.inner
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn send(&self, event: TaskEvent) {
        // No subscriber is not an error: nobody is watching.
        let _ = self.inner.events.send(event);
    }

    fn start<K: TaskKind>(
        &self,
        kind: K,
        parent: Option<TaskId>,
        token: CancellationToken,
    ) -> Handle<K> {
        let id = TaskId::new();
        let descriptor = kind.describe();
        let requires = kind.requires();
        let scope = (descriptor.admission, requires.first().copied());
        let queued = parent.is_none() && self.inner.config.cap(scope.0).is_some();
        let (items_tx, items_rx) = mpsc::channel(self.inner.config.item_buffer);
        let (outcome_tx, outcome_rx) = oneshot::channel();
        let info = TaskInfo {
            id,
            parent,
            descriptor,
            submitted: SystemTime::now(),
            settled: None,
            state: TaskState::Queued { waiting_on: None },
            progress: Progress::default(),
        };
        let seq = {
            let mut registry = self.lock();
            let seq = registry.next_seq;
            registry.next_seq += 1;
            if queued {
                registry.queues.entry(scope).or_default().insert(seq);
            }
            if let Some(parent) = parent
                && let Some(parent) = registry.tasks.get_mut(&parent)
            {
                parent.children.push(id);
                parent.live_children += 1;
            }
            registry.tasks.insert(
                id,
                Entry {
                    seq,
                    info: info.clone(),
                    children: Vec::new(),
                    live_children: 0,
                    token: token.clone(),
                    throttle: Throttle::default(),
                },
            );
            self.send(TaskEvent::Submitted(info));
            seq
        };
        let runtime = self.clone();
        let admission = Ticket { seq, scope, queued };
        tokio::spawn(async move {
            let outcome = runtime
                .drive(id, kind, requires, admission, token, items_tx)
                .await;
            // The submitter may have stopped waiting; the registry has the
            // outcome either way.
            let _ = outcome_tx.send(outcome);
        });
        Handle::new(id, items_rx, outcome_rx)
    }

    async fn drive<K: TaskKind>(
        &self,
        id: TaskId,
        kind: K,
        requires: Vec<ResourceId>,
        ticket: Ticket,
        token: CancellationToken,
        items: mpsc::Sender<Vec<K::Item>>,
    ) -> Result<K::Output, TaskError> {
        let slot = match self.admit(id, &requires, ticket, &token).await {
            Ok(slot) => slot,
            Err(error) => {
                if ticket.queued {
                    self.leave_queue(ticket);
                }
                self.settle(id, state_of(&error));
                return Err(error);
            }
        };

        self.set_state(id, TaskState::Running);
        // The body runs as a task of its own: a panic in it becomes a failure
        // here instead of a driver that never settles.
        let cx = TaskContext::new(self.clone(), id, token.clone(), items);
        let mut body = tokio::spawn(kind.run(cx));
        let joined = tokio::select! {
            biased;
            joined = &mut body => joined,
            () = token.cancelled() => {
                self.set_state(id, TaskState::Cancelling);
                body.await
            }
        };
        drop(slot);
        self.finish_children(id).await;

        let outcome = match joined {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error))
                if token.is_cancelled() && error.kind() == io::ErrorKind::Interrupted =>
            {
                Err(TaskError::Cancelled)
            }
            Ok(Err(error)) => Err(TaskError::Failed(Arc::new(error))),
            Err(join) => Err(TaskError::Failed(Arc::new(io::Error::other(format!(
                "the task panicked: {join}"
            ))))),
        };
        let state = match &outcome {
            Ok(_) => TaskState::Succeeded,
            Err(error) => state_of(error),
        };
        self.settle(id, state);
        outcome
    }

    /// Waits until every resource the task requires is ready and, for a capped
    /// root, it is first in its scope's queue and the scope has room.
    async fn admit(
        &self,
        id: TaskId,
        requires: &[ResourceId],
        ticket: Ticket,
        token: &CancellationToken,
    ) -> Result<Option<Slot>, TaskError> {
        let cap = if ticket.queued {
            self.inner.config.cap(ticket.scope.0)
        } else {
            None
        };
        loop {
            // Registered before the check, so a change between the check and
            // the wait still wakes this.
            let notified = self.inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let decision = {
                let mut registry = self.lock();
                let decision = admission(&registry, requires, ticket, cap);
                // Only capped roots hold a place: a child runs in its parent's,
                // and an unbounded class has nothing to count.
                if ticket.queued
                    && let Admission::Admit = decision
                {
                    *registry.admitted.entry(ticket.scope).or_default() += 1;
                    dequeue(&mut registry, ticket);
                }
                decision
            };
            match decision {
                Admission::Admit => {
                    if ticket.queued {
                        // The queue's head moved: whoever is next may have
                        // checked while it was still second, and is asleep.
                        self.inner.changed.notify_waiters();
                    }
                    return Ok(ticket.queued.then(|| Slot {
                        runtime: self.clone(),
                        scope: ticket.scope,
                    }));
                }
                Admission::Fail(error) => return Err(TaskError::Failed(Arc::new(error))),
                Admission::Wait(waiting_on) => {
                    self.set_queued(id, waiting_on);
                    tokio::select! {
                        () = notified => {}
                        () = token.cancelled() => return Err(TaskError::Cancelled),
                    }
                }
            }
        }
    }

    /// Cancels whatever children are still running once the body has returned,
    /// and waits for every child to settle: a child cannot outlive its parent.
    async fn finish_children(&self, id: TaskId) {
        loop {
            let notified = self.inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let registry = self.lock();
                let Some(entry) = registry.tasks.get(&id) else {
                    return;
                };
                if entry.live_children == 0 {
                    return;
                }
                for child in &entry.children {
                    if let Some(child) = registry.tasks.get(child)
                        && !child.info.state.is_terminal()
                    {
                        child.token.cancel();
                    }
                }
            }
            notified.await;
        }
    }

    /// Takes a task that will never be admitted out of its queue, so the one
    /// behind it can move up.
    fn leave_queue(&self, ticket: Ticket) {
        dequeue(&mut self.lock(), ticket);
        self.inner.changed.notify_waiters();
    }

    fn set_queued(&self, id: TaskId, waiting_on: Option<ResourceId>) {
        let mut registry = self.lock();
        if let Some(entry) = registry.tasks.get_mut(&id)
            && !matches!(entry.info.state, TaskState::Queued { waiting_on: current } if current == waiting_on)
        {
            entry.info.state = TaskState::Queued { waiting_on };
            self.send(TaskEvent::StateChanged {
                id,
                state: entry.info.state.clone(),
            });
        }
    }

    fn set_state(&self, id: TaskId, state: TaskState) {
        let mut registry = self.lock();
        if let Some(entry) = registry.tasks.get_mut(&id) {
            entry.info.state = state.clone();
            self.send(TaskEvent::StateChanged { id, state });
        }
    }

    fn settle(&self, id: TaskId, state: TaskState) {
        {
            let mut registry = self.lock();
            let Some(entry) = registry.tasks.get_mut(&id) else {
                return;
            };
            // The latest progress always reaches observers before the end.
            if entry.throttle.pending {
                entry.throttle.pending = false;
                self.send(TaskEvent::Progressed {
                    id,
                    progress: entry.info.progress.clone(),
                });
            }
            entry.info.state = state.clone();
            entry.info.settled = Some(SystemTime::now());
            let parent = entry.info.parent;
            self.send(TaskEvent::StateChanged { id, state });

            if let Some(parent) = parent {
                if let Some(parent) = registry.tasks.get_mut(&parent) {
                    parent.live_children = parent.live_children.saturating_sub(1);
                }
            } else {
                registry.settled_roots.push_back(id);
                while registry.settled_roots.len() > self.inner.config.retained_roots {
                    if let Some(oldest) = registry.settled_roots.pop_front() {
                        remove_subtree(&mut registry, oldest);
                    }
                }
            }
        }
        self.inner.changed.notify_waiters();
    }
}

fn admission(
    registry: &Registry,
    requires: &[ResourceId],
    ticket: Ticket,
    cap: Option<usize>,
) -> Admission {
    for resource in requires {
        match registry
            .resources
            .get(resource)
            .map(|resource| resource.state)
        {
            Some(ResourceState::Ready) => {}
            Some(ResourceState::Pending | ResourceState::Closing) => {
                return Admission::Wait(Some(*resource));
            }
            Some(ResourceState::Gone) => {
                return Admission::Fail(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "a resource this task requires is gone",
                ));
            }
            None => {
                return Admission::Fail(io::Error::new(
                    io::ErrorKind::NotFound,
                    "a resource this task requires does not exist",
                ));
            }
        }
    }
    let Some(cap) = cap else {
        return Admission::Admit;
    };
    let admitted = registry.admitted.get(&ticket.scope).copied().unwrap_or(0);
    let first = registry
        .queues
        .get(&ticket.scope)
        .and_then(|queue| queue.first().copied());
    if admitted < cap && first == Some(ticket.seq) {
        Admission::Admit
    } else {
        Admission::Wait(None)
    }
}

/// Where a task stands for admission: its place in submission order, the scope
/// it counts against, and whether it queues in that scope at all.
#[derive(Clone, Copy)]
struct Ticket {
    seq: u64,
    scope: Scope,
    queued: bool,
}

fn dequeue(registry: &mut Registry, ticket: Ticket) {
    if let Some(queue) = registry.queues.get_mut(&ticket.scope) {
        queue.remove(&ticket.seq);
        if queue.is_empty() {
            registry.queues.remove(&ticket.scope);
        }
    }
}

fn remove_subtree(registry: &mut Registry, id: TaskId) {
    if let Some(entry) = registry.tasks.remove(&id) {
        for child in entry.children {
            remove_subtree(registry, child);
        }
    }
}

fn state_of(error: &TaskError) -> TaskState {
    match error {
        TaskError::Failed(error) => TaskState::Failed(Arc::clone(error)),
        TaskError::Cancelled => TaskState::Cancelled,
    }
}

/// One admitted root's place in its scope, given back when the body ends.
struct Slot {
    runtime: Runtime,
    scope: Scope,
}

impl Drop for Slot {
    fn drop(&mut self) {
        {
            let mut registry = self.runtime.lock();
            if let Some(admitted) = registry.admitted.get_mut(&self.scope) {
                *admitted = admitted.saturating_sub(1);
            }
        }
        self.runtime.inner.changed.notify_waiters();
    }
}
