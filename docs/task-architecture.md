# Task Architecture

How v2's task runtime is built: its parts, the lifecycle every task goes through,
and the rules that hold for every kind. [Task pipeline](task-pipeline.md) follows
one task through the whole system; [v2 architecture](design/v2-architecture.md)
is the proposal this implements and still owns everything not built yet.

## Status

Implemented in [`src/task/`](../src/task/mod.rs). The server holds one runtime
from startup to shutdown. Its only kind so far is `demo.count`; the filesystem
kinds arrive in step 6 of the build order.

## The rule

> **Nothing asynchronous happens except as a task.**

A task is one unit of asynchronous, fallible work. It is submitted, admitted,
run, observed and settled through one lifecycle that the runtime owns, so no
caller tracks a future's lifecycle for itself. Synchronous changes — moving a
selection, typing a filter — are not tasks: they have no duration and cannot
fail.

> **The runtime owns lifecycle, never payload.**

It stores what a task is and how it is going. What a task produces — entries,
bytes — goes to the code that submitted it and is not retained.

## Parts

```text
src/task/
├── mod.rs        the shared types: TaskId, TaskKind, TaskDescriptor, TaskState,
│                 Progress, TaskInfo, TaskEvent, ResourceId, ResourceState
├── runtime.rs    Runtime: the registry, admission, the tree, events, retention
├── context.rs    TaskContext: everything a kind's body can reach
└── handle.rs     Handle: the submitter's view of one task
```

| Part | Owns |
| --- | --- |
| `Runtime` | The registry of tasks and resources, behind one `std::sync::Mutex`; the broadcast channel of events; one `Notify` that wakes waiting drivers |
| Driver | One tokio task per task: admits it, runs its body, waits for its children, settles it |
| Body | The kind's `run`, spawned as a tokio task of its own |
| `TaskContext` | The body's only way to reach the runtime |
| `Handle` | The submitter's ID, item receiver and outcome receiver |

`Runtime` is cheap to clone; clones share one registry. `task` names nothing else
in the crate except through the trait it defines: the runtime has no idea what a
filesystem is.

## A kind

```rust
pub(crate) trait TaskKind: Send + 'static {
    type Item: Send + 'static;     // emitted while running; () for none
    type Output: Send + 'static;   // delivered once, on success

    fn describe(&self) -> TaskDescriptor;
    fn requires(&self) -> Vec<ResourceId> { Vec::new() }
    fn run(self, cx: TaskContext<Self::Item>)
        -> impl Future<Output = io::Result<Self::Output>> + Send;
}
```

The kind set is open: a new family of work is a new implementation, not a new
variant every match must learn. `TaskDescriptor` is the one closed shape —
`kind` (a stable dotted name such as `demo.count`), `subject` (named operands)
and `admission` (its class) — so anything that lists tasks renders any kind
without knowing it.

A body sees the runtime only through its context:

| `TaskContext` | Does |
| --- | --- |
| `cancelled()` / `is_cancelled()` | Resolves, or answers, when this task or an ancestor is cancelled |
| `progress(p)` | Records progress; observers see it coalesced |
| `emit(items)` | Sends a batch to the submitter. Bounded, so awaiting it is backpressure. `Err(Stop)` when nothing receives any more, or when cancelled while waiting for room |
| `spawn(kind)` | Starts a child task |
| `resource::<R>(id)` | The value of a ready resource the task declared it requires |
| `id()` | This task's ID |

## Lifecycle

![Task lifecycle: Queued, Running, Cancelling, and the three terminal states](assets/task-lifecycle.svg)

| State | Means |
| --- | --- |
| `Queued { waiting_on: Some(r) }` | Waiting for resource `r` to become ready |
| `Queued { waiting_on: None }` | Waiting for admission: its class's cap is full, or it is not first in line |
| `Running` | The body is executing |
| `Cancelling` | Cancellation was requested while running; the body is cleaning up |
| `Succeeded`, `Failed(Arc<io::Error>)`, `Cancelled` | Terminal; never changes again |

`Failed` holds an `Arc` because several observers read one failure.

### How a task ends

| The body returns | Cancel requested? | Settles |
| --- | --- | --- |
| `Ok(output)` | either | `Succeeded` — it finished the work, so saying otherwise would be untrue |
| `Err` of kind `Interrupted` | yes | `Cancelled` |
| `Err` of kind `Interrupted` | no | `Failed` — an interruption nobody asked for is a failure |
| any other `Err` | either | `Failed` — after a cancel, its cleanup failed |
| panics | either | `Failed`, with "the task panicked" |

A body that stops because it was cancelled returns
`Err(io::ErrorKind::Interrupted.into())`; that is the convention every kind
follows. A panic is caught because each body runs as a tokio task of its own and
the driver awaits its join handle, so a panicking body can never leave a task
`Running` forever.

A task can also end without its body ever running: cancelled while queued, or
failed at admission because a resource it requires is gone or unknown.

### Events

```rust
pub(crate) enum TaskEvent {
    Submitted(TaskInfo),
    StateChanged { id: TaskId, state: TaskState },
    Progressed { id: TaskId, progress: Progress },
}
```

| When | Event |
| --- | --- |
| `submit` returns | `Submitted`, with the task `Queued { waiting_on: None }` |
| It starts waiting on a resource, or stops | `StateChanged(Queued { .. })`, only when `waiting_on` changes |
| It is admitted | `StateChanged(Running)` |
| A cancel reaches it while running | `StateChanged(Cancelling)` |
| It settles | any pending `Progressed`, then `StateChanged` with the terminal state |

`Submitted` is sent under the registry lock before `submit` returns, so a task
is in `tasks()` and announced by the time its submitter has its handle.

## The tree

A task started through `TaskContext::spawn` is a child of the task that started
it; one started through `Runtime::submit` is a root. The tree is structured:

- **A child cannot outlive its parent.** When a body returns while children are
  still live, the driver cancels them and waits for every one to settle before
  the parent settles.
- **Cancelling a parent cancels its subtree.** A child's cancellation token is a
  child of its parent's.
- **A child's failure goes to its parent** through the child's handle. The parent
  decides what it means; the runtime does not fail the parent for it.
- **A child runs under its parent's admission.** It is not capped, so a parent
  holding the last place of its class can start children without deadlocking on
  itself.

Phases (`Progress::phase`) are for steps that are not independently cancellable,
observable or retryable; children are for steps that are.

## Resources

A resource is something tasks wait for — on the client an attached machine, on
the server later a session tab. The runtime knows only its state and an
optional typed value.

| State | A task requiring it |
| --- | --- |
| `Pending` | queues, `waiting_on` it |
| `Ready` | may be admitted; reads its value with `cx.resource::<R>(id)` |
| `Closing` | queues, as for `Pending` |
| `Gone` | fails at admission with `NotConnected` |
| unknown ID | fails at admission with `NotFound` |

`add_resource`, `set_resource_state` and `set_resource_ready(id, value)` change
them; leaving `Ready` drops the value. Changing a resource does not touch tasks
already running against it — taking a connection down cancels its tasks, and
that is the job of the kind that does it.

## Admission

A root task is admitted when every resource it requires is ready and its class
has room in its **scope**: the pair of its class and the first resource it
requires, or the class alone when it requires none. Two machines' transfers are
two scopes and do not share a cap.

| Class | Default cap per scope | For |
| --- | --- | --- |
| `Interactive` | unbounded | Listings and previews: what the user is waiting on |
| `Transfer` | 2 | Copies: bandwidth is shared |
| `Background` | 1 | Directory sizes, the demo: must not starve the rest |

**A capped scope is first in, first out.** A capped root joins its scope's queue,
ordered by submission, at the moment it is submitted; only the head may take a
free place. Waking every waiter and letting the fastest win would admit them in
whatever order the scheduler happened to pick. A task cancelled or failed while
queued leaves the queue, and the one behind it moves up. Unbounded classes and
children do not queue.

The caps are fields of `task::Config` and the server uses the defaults; nothing
reads them from configuration yet.

## Cancellation

`Runtime::cancel(id)` cancels a task and its subtree. It is idempotent, and a
no-op for an unknown or already settled task, so a caller holding an ID past its
task's end need not check first. `cancel_all` cancels every unsettled task, and
`idle()` resolves once none is left unsettled; a stopping server calls the one,
then waits on the other, so its clients see how each task ended before their
streams close.

| Cancelled while | Happens |
| --- | --- |
| Queued | The driver stops waiting and settles it `Cancelled`; the body never runs |
| Running | The state becomes `Cancelling`, the body sees `cx.cancelled()`, and the outcome table above decides how it settles |
| Blocked in `emit` | `emit` returns `Err(Stop)` |

Cancellation is cooperative: the runtime asks, the body stops. A body that never
looks at `cx.cancelled()` runs to its end.

## Handles

```rust
let mut handle = runtime.submit(kind);
handle.id();                       // TaskId
handle.next().await;               // Option<Vec<Item>>; None once it stops emitting
handle.outcome().await;            // Result<Output, TaskError>
```

A kind whose `Item` is `()` implements `IntoFuture`, so it is awaited like the
future it replaced: `runtime.submit(kind).await?`.

**Dropping a handle detaches.** The task keeps running in the runtime and
`cancel` still reaches it. A streaming task notices on its next `emit`, which
returns `Err(Stop)` because nothing receives its items, and should stop there.

`TaskError` is `Failed(Arc<io::Error>)` or `Cancelled`.

## Progress

`Progress` is two independent axes: an optional `phase` name and a `Measure` —
`Indeterminate`, `Items { done, total }` or `Bytes { done, total }`, each `total`
optional so a share can be shown when it is known.

A body may report progress as often as it likes. The runtime always stores the
latest value in the task's `TaskInfo`, and publishes `Progressed` at most once
per `progress_interval` (100 ms) per task: the first report goes out at once, a
report inside the interval schedules one flush at its end, and the latest value
is always sent before the task's terminal `StateChanged`.

## Retention

The runtime keeps every live task plus the most recent `retained_roots` (256)
settled roots, each with its whole subtree. When one more root settles, the
oldest retained root and its subtree are removed. A root settles only after its
children, so a subtree is wholly settled by the time it is evicted. Retention is
what lets an observer answer "which failed" without keeping its own history,
and its bound is what keeps it from becoming a log.

## Concurrency

- **One registry lock**, a `std::sync::Mutex`, so a synchronous caller — a render
  frame — can call `tasks()` or `cancel()`. No critical section holds an
  `await`. Events are sent while it is held, which keeps their order consistent
  with the registry's.
- **Snapshots, not borrows.** `tasks()` and `task(id)` return owned `TaskInfo`
  values, in submission order.
- **One `Notify` for every wait.** A driver waiting on admission or on its
  children registers interest before it checks its condition, then sleeps; any
  change that could satisfy a waiter — a resource changing, a place freeing, the
  head of a queue moving, a task settling — wakes them all, and each re-checks.
  Registering first is what keeps a change between the check and the sleep from
  being lost.
- **A lagging subscriber is dropped behind, not waited for.** The event channel
  holds `event_buffer` (1024) events; a subscriber further behind gets `Lagged`
  and recovers with a `tasks()` snapshot.

The runtime's tests run on a multi-threaded tokio runtime, as the server does.
Run single-threaded they hid both the unfair admission order and a lost wakeup
at the head of a queue; multi-threaded, they found both.

## Configuration

| `task::Config` field | Default | Is |
| --- | --- | --- |
| `interactive_cap`, `transfer_cap`, `background_cap` | `None`, `Some(2)`, `Some(1)` | Admission caps per scope |
| `retained_roots` | 256 | Settled roots kept with their subtrees |
| `progress_interval` | 100 ms | The shortest gap between two `Progressed` for one task |
| `item_buffer` | 16 | Batches a task may emit ahead of its reader |
| `event_buffer` | 1024 | Events a subscriber may fall behind before it lags |

## Not built yet

- **Summing children's progress into the parent's.** Deferred to step 10, where
  copies are its first consumer.
- **Configurable caps.** Hard-coded defaults until configuration exists; the
  proposal reloads them with `fpk server reload-config`.
- **Task identity across a retry**, still an open question in the proposal.
- **An abort backstop** for a body that ignores cancellation.
