//! The runtime with test kinds only: no filesystem, no sockets.

use std::{
    future::Future,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::sync::{Notify, broadcast};

use super::*;

const PATIENCE: Duration = Duration::from_secs(5);

fn runtime() -> Runtime {
    Runtime::new(Config::default())
}

fn descriptor(admission: AdmissionClass) -> TaskDescriptor {
    TaskDescriptor::new("test.kind", admission)
}

/// Returns its value at once.
struct Echo(u32);

impl TaskKind for Echo {
    type Item = ();
    type Output = u32;
    fn describe(&self) -> TaskDescriptor {
        descriptor(AdmissionClass::Interactive).with("value", self.0.to_string())
    }
    fn run(self, _cx: TaskContext<()>) -> impl Future<Output = io::Result<u32>> + Send {
        std::future::ready(Ok(self.0))
    }
}

/// Fails at once.
struct Fail;

impl TaskKind for Fail {
    type Item = ();
    type Output = ();
    fn describe(&self) -> TaskDescriptor {
        descriptor(AdmissionClass::Interactive)
    }
    fn run(self, _cx: TaskContext<()>) -> impl Future<Output = io::Result<()>> + Send {
        std::future::ready(Err(io::Error::other("it broke")))
    }
}

struct Panic;

impl TaskKind for Panic {
    type Item = ();
    type Output = ();
    fn describe(&self) -> TaskDescriptor {
        descriptor(AdmissionClass::Interactive)
    }
    #[allow(clippy::unused_async)]
    async fn run(self, _cx: TaskContext<()>) -> io::Result<()> {
        panic!("the body panicked");
    }
}

/// Runs until its gate opens or it is cancelled. Cancelled, it spends a moment
/// cleaning up — long enough to be seen `Cancelling` — and reports it the way a
/// body should: `Interrupted`.
#[derive(Clone)]
struct Wait {
    gate: Arc<Notify>,
    class: AdmissionClass,
    requires: Vec<ResourceId>,
    ran: Arc<AtomicBool>,
}

impl Wait {
    fn new(class: AdmissionClass) -> Self {
        Self {
            gate: Arc::new(Notify::new()),
            class,
            requires: Vec::new(),
            ran: Arc::new(AtomicBool::new(false)),
        }
    }

    fn requiring(mut self, resource: ResourceId) -> Self {
        self.requires.push(resource);
        self
    }
}

impl TaskKind for Wait {
    type Item = ();
    type Output = ();
    fn describe(&self) -> TaskDescriptor {
        descriptor(self.class)
    }
    fn requires(&self) -> Vec<ResourceId> {
        self.requires.clone()
    }
    async fn run(self, cx: TaskContext<()>) -> io::Result<()> {
        self.ran.store(true, Ordering::SeqCst);
        tokio::select! {
            () = self.gate.notified() => Ok(()),
            () = cx.cancelled() => {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Err(io::ErrorKind::Interrupted.into())
            }
        }
    }
}

/// Emits `count` batches of one item each, or forever when `None`, and stops
/// cleanly when told to.
struct Stream {
    count: Option<u32>,
    stopped: Arc<AtomicBool>,
}

impl TaskKind for Stream {
    type Item = u32;
    type Output = ();
    fn describe(&self) -> TaskDescriptor {
        descriptor(AdmissionClass::Interactive)
    }
    async fn run(self, cx: TaskContext<u32>) -> io::Result<()> {
        let mut next = 0;
        while self.count.is_none_or(|count| next < count) {
            if cx.emit(vec![next]).await == Err(Stop) {
                self.stopped.store(true, Ordering::SeqCst);
                return Ok(());
            }
            next += 1;
            cx.progress(Progress {
                phase: None,
                measure: Measure::Items {
                    done: u64::from(next),
                },
            });
        }
        Ok(())
    }
}

/// Reports a thousand progress updates as fast as it can.
struct Busy;

impl TaskKind for Busy {
    type Item = ();
    type Output = ();
    fn describe(&self) -> TaskDescriptor {
        descriptor(AdmissionClass::Interactive)
    }
    async fn run(self, cx: TaskContext<()>) -> io::Result<()> {
        for done in 0..1000 {
            cx.progress(Progress {
                phase: Some("counting"),
                measure: Measure::Bytes {
                    done,
                    total: Some(999),
                },
            });
            tokio::task::yield_now().await;
        }
        Ok(())
    }
}

/// What a parent does with its children.
#[derive(Clone)]
enum Plan {
    /// Starts a child and returns without waiting for it.
    Abandon(Wait),
    /// Starts a child and waits for it.
    Await(Wait),
    /// Starts a failing child and succeeds anyway.
    Tolerate,
    /// Starts a child of its own class and waits for it.
    SameClass,
}

struct Parent {
    plan: Plan,
    class: AdmissionClass,
}

impl TaskKind for Parent {
    type Item = ();
    type Output = ();
    fn describe(&self) -> TaskDescriptor {
        descriptor(self.class)
    }
    async fn run(self, cx: TaskContext<()>) -> io::Result<()> {
        match self.plan {
            Plan::Abandon(child) => {
                let _detached = cx.spawn(child);
                Ok(())
            }
            Plan::Await(child) => match cx.spawn(child).await {
                Ok(()) => Ok(()),
                Err(TaskError::Cancelled) => Err(io::ErrorKind::Interrupted.into()),
                Err(TaskError::Failed(error)) => Err(io::Error::other(error.to_string())),
            },
            Plan::Tolerate => {
                assert!(cx.spawn(Fail).await.is_err());
                Ok(())
            }
            Plan::SameClass => {
                let child = Wait::new(self.class);
                let gate = Arc::clone(&child.gate);
                let handle = cx.spawn(child);
                tokio::time::sleep(Duration::from_millis(20)).await;
                gate.notify_one();
                handle
                    .await
                    .map_err(|error| io::Error::other(error.to_string()))
            }
        }
    }
}

/// Polls until `id` is in a state `want` accepts.
async fn until(runtime: &Runtime, id: TaskId, want: impl Fn(&TaskState) -> bool) -> TaskInfo {
    tokio::time::timeout(PATIENCE, async {
        loop {
            if let Some(info) = runtime.task(id)
                && want(&info.state)
            {
                return info;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("task {id} never reached the state: {:?}", runtime.task(id)))
}

fn drain(events: &mut broadcast::Receiver<TaskEvent>) -> Vec<TaskEvent> {
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    seen
}

fn state_names(events: &[TaskEvent], id: TaskId) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            TaskEvent::Submitted(info) if info.id == id => Some("submitted"),
            TaskEvent::StateChanged { id: of, state } if *of == id => Some(match state {
                TaskState::Queued { .. } => "queued",
                TaskState::Running => "running",
                TaskState::Cancelling => "cancelling",
                TaskState::Succeeded => "succeeded",
                TaskState::Failed(_) => "failed",
                TaskState::Cancelled => "cancelled",
            }),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_runs_and_succeeds_through_the_whole_lifecycle() {
    let runtime = runtime();
    let mut events = runtime.subscribe();
    let handle = runtime.submit(Echo(7));
    let id = handle.id();
    assert_eq!(handle.await.unwrap(), 7);

    let info = until(&runtime, id, TaskState::is_terminal).await;
    assert!(matches!(info.state, TaskState::Succeeded));
    assert!(info.settled.unwrap() >= info.submitted);
    assert_eq!(info.descriptor.kind, "test.kind");
    assert_eq!(info.descriptor.subject[0].value, "7");
    assert_eq!(
        state_names(&drain(&mut events), id),
        ["submitted", "running", "succeeded"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failure_is_shared_with_every_observer() {
    let runtime = runtime();
    let handle = runtime.submit(Fail);
    let id = handle.id();
    let Err(TaskError::Failed(error)) = handle.await else {
        panic!("expected a failure");
    };
    assert_eq!(error.to_string(), "it broke");
    let TaskState::Failed(seen) = runtime.task(id).unwrap().state else {
        panic!("expected a failed state");
    };
    assert!(Arc::ptr_eq(&error, &seen));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panicking_body_fails_rather_than_hanging() {
    let runtime = runtime();
    let handle = runtime.submit(Panic);
    let Err(TaskError::Failed(error)) = handle.await else {
        panic!("expected a failure");
    };
    assert!(error.to_string().contains("panicked"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_running_task_passes_through_cancelling() {
    let runtime = runtime();
    let mut events = runtime.subscribe();
    let handle = runtime.submit(Wait::new(AdmissionClass::Interactive));
    let id = handle.id();
    until(&runtime, id, |state| matches!(state, TaskState::Running)).await;

    runtime.cancel(id);
    assert!(matches!(handle.await, Err(TaskError::Cancelled)));
    assert_eq!(
        state_names(&drain(&mut events), id),
        ["submitted", "running", "cancelling", "cancelled"]
    );
    // Again, and for a task that never existed: neither is an error.
    runtime.cancel(id);
    runtime.cancel(TaskId::new());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_body_that_finishes_despite_a_cancel_still_succeeds() {
    struct Stubborn;
    impl TaskKind for Stubborn {
        type Item = ();
        type Output = ();
        fn describe(&self) -> TaskDescriptor {
            descriptor(AdmissionClass::Interactive)
        }
        async fn run(self, cx: TaskContext<()>) -> io::Result<()> {
            cx.cancelled().await;
            assert!(cx.is_cancelled());
            Ok(())
        }
    }
    let runtime = runtime();
    let handle = runtime.submit(Stubborn);
    let id = handle.id();
    until(&runtime, id, |state| matches!(state, TaskState::Running)).await;
    runtime.cancel(id);
    // It did its work; reporting it cancelled would be untrue.
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_queues_on_a_pending_resource_and_runs_once_it_is_ready() {
    let runtime = runtime();
    let resource = runtime.add_resource(ResourceState::Pending);
    let wait = Wait::new(AdmissionClass::Interactive).requiring(resource);
    let ran = Arc::clone(&wait.ran);
    let gate = Arc::clone(&wait.gate);
    let handle = runtime.submit(wait);
    let id = handle.id();

    let info = until(&runtime, id, |state| {
        matches!(
            state,
            TaskState::Queued {
                waiting_on: Some(_)
            }
        )
    })
    .await;
    assert!(matches!(info.state, TaskState::Queued { waiting_on: Some(r) } if r == resource));
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!ran.load(Ordering::SeqCst));

    runtime.set_resource_ready(resource, "connection");
    until(&runtime, id, |state| matches!(state, TaskState::Running)).await;
    gate.notify_one();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gone_or_unknown_resource_fails_at_admission() {
    let runtime = runtime();
    let gone = runtime.add_resource(ResourceState::Gone);
    let wait = Wait::new(AdmissionClass::Interactive).requiring(gone);
    let ran = Arc::clone(&wait.ran);
    assert!(matches!(
        runtime.submit(wait).await,
        Err(TaskError::Failed(error)) if error.kind() == io::ErrorKind::NotConnected
    ));
    assert!(!ran.load(Ordering::SeqCst));

    let unknown = ResourceId::new();
    assert!(matches!(
        runtime.submit(Wait::new(AdmissionClass::Interactive).requiring(unknown)).await,
        Err(TaskError::Failed(error)) if error.kind() == io::ErrorKind::NotFound
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closing_resource_queues_and_a_gone_one_then_fails_it() {
    let runtime = runtime();
    let resource = runtime.add_resource(ResourceState::Ready);
    runtime.set_resource_state(resource, ResourceState::Closing);
    let wait = Wait::new(AdmissionClass::Interactive).requiring(resource);
    let ran = Arc::clone(&wait.ran);
    let handle = runtime.submit(wait);
    until(&runtime, handle.id(), |state| {
        matches!(
            state,
            TaskState::Queued {
                waiting_on: Some(_)
            }
        )
    })
    .await;
    runtime.set_resource_state(resource, ResourceState::Gone);
    assert!(matches!(handle.await, Err(TaskError::Failed(_))));
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queued_task_is_cancelled_without_ever_running() {
    let runtime = runtime();
    let resource = runtime.add_resource(ResourceState::Pending);
    let wait = Wait::new(AdmissionClass::Interactive).requiring(resource);
    let ran = Arc::clone(&wait.ran);
    let handle = runtime.submit(wait);
    let id = handle.id();
    until(&runtime, id, |state| {
        matches!(state, TaskState::Queued { .. })
    })
    .await;
    runtime.cancel(id);
    assert!(matches!(handle.await, Err(TaskError::Cancelled)));
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_body_reads_the_value_of_a_resource_it_requires() {
    struct Read(ResourceId);
    impl TaskKind for Read {
        type Item = ();
        type Output = String;
        fn describe(&self) -> TaskDescriptor {
            descriptor(AdmissionClass::Interactive)
        }
        fn requires(&self) -> Vec<ResourceId> {
            vec![self.0]
        }
        fn run(self, cx: TaskContext<()>) -> impl Future<Output = io::Result<String>> + Send {
            let value = cx.resource::<String>(self.0).expect("a ready string");
            assert!(cx.resource::<u32>(self.0).is_none(), "the wrong type");
            std::future::ready(Ok(format!("{value} for {}", cx.id())))
        }
    }
    let runtime = runtime();
    let resource = runtime.add_resource(ResourceState::Pending);
    runtime.set_resource_ready(resource, "machine one".to_owned());
    let handle = runtime.submit(Read(resource));
    let id = handle.id();
    assert_eq!(handle.await.unwrap(), format!("machine one for {id}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_class_cap_queues_roots_per_scope() {
    let runtime = runtime();
    let machine = runtime.add_resource(ResourceState::Ready);
    let other = runtime.add_resource(ResourceState::Ready);
    let copies: Vec<Wait> = (0..3)
        .map(|_| Wait::new(AdmissionClass::Transfer).requiring(machine))
        .collect();
    let handles: Vec<_> = copies
        .iter()
        .map(|copy| runtime.submit(copy.clone()))
        .collect();
    let ids: Vec<TaskId> = handles.iter().map(Handle::id).collect();

    until(&runtime, ids[0], |state| {
        matches!(state, TaskState::Running)
    })
    .await;
    until(&runtime, ids[1], |state| {
        matches!(state, TaskState::Running)
    })
    .await;
    // The cap is two per machine, so the third waits on admission, not on a
    // resource.
    until(&runtime, ids[2], |state| {
        matches!(state, TaskState::Queued { waiting_on: None })
    })
    .await;

    // Another machine's scope is its own.
    let elsewhere = Wait::new(AdmissionClass::Transfer).requiring(other);
    let elsewhere_gate = Arc::clone(&elsewhere.gate);
    let elsewhere = runtime.submit(elsewhere);
    until(&runtime, elsewhere.id(), |state| {
        matches!(state, TaskState::Running)
    })
    .await;

    copies[0].gate.notify_one();
    until(&runtime, ids[2], |state| {
        matches!(state, TaskState::Running)
    })
    .await;

    copies[1].gate.notify_one();
    copies[2].gate.notify_one();
    elsewhere_gate.notify_one();
    for handle in handles {
        handle.await.unwrap();
    }
    elsewhere.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capped_queue_admits_in_submission_order() {
    let runtime = runtime();
    let machine = runtime.add_resource(ResourceState::Ready);
    // Background is capped at one per machine, so these run strictly one at a
    // time, and must start in the order they were submitted.
    let jobs: Vec<Wait> = (0..6)
        .map(|_| Wait::new(AdmissionClass::Background).requiring(machine))
        .collect();
    let handles: Vec<_> = jobs.iter().map(|job| runtime.submit(job.clone())).collect();
    // One in the middle gives up while queued; the line closes up behind it.
    runtime.cancel(handles[3].id());
    for (index, job) in jobs.iter().enumerate() {
        if index == 3 {
            continue;
        }
        until(&runtime, handles[index].id(), |state| {
            matches!(state, TaskState::Running)
        })
        .await;
        for later in &jobs[index + 1..] {
            assert!(!later.ran.load(Ordering::SeqCst), "admitted out of order");
        }
        job.gate.notify_one();
    }
    for (index, handle) in handles.into_iter().enumerate() {
        let outcome = handle.await;
        if index == 3 {
            assert!(matches!(outcome, Err(TaskError::Cancelled)));
        } else {
            outcome.unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_runs_under_its_parents_admission() {
    let runtime = runtime();
    // The background cap is one: were the child admitted like a root, it would
    // wait for the slot its own parent holds, forever.
    let handle = runtime.submit(Parent {
        plan: Plan::SameClass,
        class: AdmissionClass::Background,
    });
    tokio::time::timeout(PATIENCE, handle)
        .await
        .expect("parent and child deadlocked")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_cannot_outlive_its_parent() {
    let runtime = runtime();
    let child = Wait::new(AdmissionClass::Interactive);
    let handle = runtime.submit(Parent {
        plan: Plan::Abandon(child),
        class: AdmissionClass::Interactive,
    });
    let parent = handle.id();
    handle.await.unwrap();

    let tasks = runtime.tasks();
    let child = tasks
        .iter()
        .find(|info| info.parent == Some(parent))
        .expect("the child is in the registry");
    assert!(matches!(child.state, TaskState::Cancelled), "{child:?}");
    let parent = runtime.task(parent).unwrap();
    assert!(parent.settled.unwrap() >= child.settled.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_parent_cancels_its_subtree() {
    let runtime = runtime();
    let child = Wait::new(AdmissionClass::Interactive);
    let ran = Arc::clone(&child.ran);
    let handle = runtime.submit(Parent {
        plan: Plan::Await(child),
        class: AdmissionClass::Interactive,
    });
    let parent = handle.id();
    tokio::time::timeout(PATIENCE, async {
        while !ran.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();

    runtime.cancel(parent);
    assert!(matches!(handle.await, Err(TaskError::Cancelled)));
    let child = runtime
        .tasks()
        .into_iter()
        .find(|info| info.parent == Some(parent))
        .unwrap();
    assert!(matches!(child.state, TaskState::Cancelled), "{child:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_parent_decides_what_its_childs_failure_means() {
    let runtime = runtime();
    let handle = runtime.submit(Parent {
        plan: Plan::Tolerate,
        class: AdmissionClass::Interactive,
    });
    let parent = handle.id();
    handle.await.unwrap();
    let child = runtime
        .tasks()
        .into_iter()
        .find(|info| info.parent == Some(parent))
        .unwrap();
    assert!(matches!(child.state, TaskState::Failed(_)));
    assert!(matches!(
        runtime.task(parent).unwrap().state,
        TaskState::Succeeded
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn items_stream_to_the_handle_in_order() {
    let runtime = runtime();
    let stopped = Arc::new(AtomicBool::new(false));
    let mut handle = runtime.submit(Stream {
        count: Some(40),
        stopped: Arc::clone(&stopped),
    });
    let mut seen = Vec::new();
    while let Some(batch) = handle.next().await {
        seen.extend(batch);
    }
    handle.outcome().await.unwrap();
    assert_eq!(seen, (0..40).collect::<Vec<_>>());
    assert!(!stopped.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_the_handle_stops_a_stream_at_its_next_emit() {
    let runtime = runtime();
    let stopped = Arc::new(AtomicBool::new(false));
    let mut handle = runtime.submit(Stream {
        count: None,
        stopped: Arc::clone(&stopped),
    });
    let id = handle.id();
    handle.next().await.unwrap();
    drop(handle);
    let info = until(&runtime, id, TaskState::is_terminal).await;
    assert!(stopped.load(Ordering::SeqCst));
    assert!(matches!(info.state, TaskState::Succeeded));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn emitting_is_bounded_by_the_reader() {
    struct Count(Arc<AtomicUsize>);
    impl TaskKind for Count {
        type Item = u32;
        type Output = ();
        fn describe(&self) -> TaskDescriptor {
            descriptor(AdmissionClass::Interactive)
        }
        async fn run(self, cx: TaskContext<u32>) -> io::Result<()> {
            loop {
                if cx.emit(vec![0]).await.is_err() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    let runtime = Runtime::new(Config {
        item_buffer: 2,
        ..Config::default()
    });
    let sent = Arc::new(AtomicUsize::new(0));
    let handle = runtime.submit(Count(Arc::clone(&sent)));
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Two batches fit in the channel; the third send is the one waiting.
    assert_eq!(sent.load(Ordering::SeqCst), 2);
    // Cancelled while it waits for room, it stops rather than hanging.
    runtime.cancel(handle.id());
    assert!(matches!(handle.outcome().await, Err(TaskError::Cancelled)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn progress_is_coalesced_and_the_last_value_arrives_before_the_end() {
    let runtime = Runtime::new(Config {
        progress_interval: Duration::from_secs(60),
        ..Config::default()
    });
    let mut events = runtime.subscribe();
    let handle = runtime.submit(Busy);
    let id = handle.id();
    handle.await.unwrap();
    let events = drain(&mut events);

    let progressed: Vec<&Progress> = events
        .iter()
        .filter_map(|event| match event {
            TaskEvent::Progressed { id: of, progress } if *of == id => Some(progress),
            _ => None,
        })
        .collect();
    // The first update goes out at once; the other 999 within the interval
    // collapse into the one flushed at the end.
    assert_eq!(progressed.len(), 2, "{progressed:?}");
    let finished = Measure::Bytes {
        done: 999,
        total: Some(999),
    };
    assert_eq!(progressed[1].measure, finished);
    let last_progress = events
        .iter()
        .rposition(|event| matches!(event, TaskEvent::Progressed { .. }))
        .unwrap();
    let settled = events
        .iter()
        .rposition(|event| {
            matches!(
                event,
                TaskEvent::StateChanged {
                    state: TaskState::Succeeded,
                    ..
                }
            )
        })
        .unwrap();
    assert!(last_progress < settled);
    assert_eq!(runtime.task(id).unwrap().progress.measure, finished);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retention_keeps_the_most_recent_settled_roots_and_their_subtrees() {
    let runtime = Runtime::new(Config {
        retained_roots: 3,
        ..Config::default()
    });
    let mut parents = Vec::new();
    for _ in 0..5 {
        let handle = runtime.submit(Parent {
            plan: Plan::Tolerate,
            class: AdmissionClass::Interactive,
        });
        parents.push(handle.id());
        handle.await.unwrap();
    }
    let tasks = runtime.tasks();
    let roots: Vec<TaskId> = tasks
        .iter()
        .filter(|info| info.parent.is_none())
        .map(|info| info.id)
        .collect();
    assert_eq!(roots, parents[2..]);
    // Each retained root kept its one child; the evicted roots took theirs.
    assert_eq!(tasks.len(), 6);
    assert!(
        tasks
            .iter()
            .all(|info| info.parent.is_none_or(|parent| roots.contains(&parent)))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_snapshot_is_in_submission_order() {
    let runtime = runtime();
    let ids: Vec<TaskId> = (0..10)
        .map(|value| runtime.submit(Echo(value)).id())
        .collect();
    let listed: Vec<TaskId> = runtime.tasks().into_iter().map(|info| info.id).collect();
    assert_eq!(listed, ids);
}
