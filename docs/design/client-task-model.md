# Client Task Model

A proposal for making every unit of work the Client performs a *task*: a named,
observable, cancellable thing the Client owns, rather than a future or stream
whose lifecycle each frontend tracks for itself.

## Status

This is a **future possibility**, not current behavior. Today
[`Client`](../../src/client/mod.rs) owns a connection registry and nothing else:
each operation is an `async fn` returning a future or an owned stream, and
cancellation happens when the caller drops what it was given.
[Architecture](../architecture.md) and [Client API](../api.md) own what is
actually built. This document owns the proposed model only.

## The problem

Task handling exists today, three times, in three places, at three levels of
correctness.

| Where | What it tracks | State |
| --- | --- | --- |
| [`tui::app`](../../src/tui/app/mod.rs) | `start_tasks`, `path_tasks`, `close_tasks`, one transfer task, one quit timer | correct |
| `tui::browser_context` | one root listing task plus `listing_tasks` keyed by request ID | correct |
| [`gui::app`](../../src/gui/app.rs) | nothing: `spawn_listing` drops the `JoinHandle` | leaks |

Each grew the same four mechanisms independently — minting IDs, rejecting stale
results, aborting a superseded request, and joining or aborting at shutdown. The
GUI grew only the first. That is why collapsing a large directory there keeps
streaming to completion, which the [TODO](../../TODO.md) tracks as item 18.

The duplication is the defect, not the leak. A leak is one bug in one frontend;
duplicated lifecycle machinery guarantees the next frontend starts from zero and
gets a different subset right. The work is also about to grow: reporting
transfer progress, queueing transfers and cancelling them (item 10), and holding
several connections at once (item 9) each add tasks that need to be watched
rather than merely awaited.

## The idea

The Client does nothing except by launching a task. A task is the unit that is
launched, observed, cancelled and reported on; the registry that holds them is
the Client's central state.

One rule bounds it:

> **A task owns lifecycle, never payload.**

The registry stores what a task *is* and how it is *going*: its kind, its
operands, its connection, its state, its progress, its error. It never stores
what the task *produced*. Entries, bytes and resolved paths travel to the
caller through the task's own channel and are not retained.

That rule is what keeps this compatible with the boundary
[Architecture](../architecture.md) already states — the Client owns no browsing
state, snapshot, cache or tree. A registry that held listing entries so a
frontend could ask for them later would be exactly the cache that boundary
forbids, with an eviction policy attached.

## Terms

| Term | Meaning |
| --- | --- |
| Task | One unit of work the Client owns, from launch to a terminal state |
| Task kind | What the task does, carrying that operation's operands |
| Registry | The Client's collection of live and recently terminal tasks |
| Handle | The launching caller's typed view of one task |
| Terminal state | Succeeded, Failed or Cancelled; a task in one never changes again |
| Settle | A task reaching a terminal state |

## Types

A kind carries its own operands, so the Client never formats a string for
display and a frontend renders a task however it renders anything else.

```rust
pub(crate) struct TaskId(Uuid);

pub(crate) enum TaskKind {
    Connect     { location: ServerLocation },
    Disconnect,
    ResolvePath { path: HostPath },
    ListDir     { path: HostPath },
    WalkDir     { path: HostPath },
    ReadFile    { path: HostPath },
    CopyFile    { source: HostPath, destination_dir: HostPath, name: HostPath },
}

pub(crate) enum TaskState {
    Queued,
    Running,
    Succeeded,
    Failed(Arc<io::Error>),
    Cancelled,
}
```

`Failed` shares its error because several observers read one failure: the
launching caller, a task list, and a status line. `io::Error` is not `Clone`.

Progress is two independent axes. A copy is in a phase *and* has a byte count;
collapsing them into one enum forces every kind to the widest variant.

```rust
pub(crate) struct Progress {
    pub phase: Phase,
    pub measure: Measure,
}

pub(crate) enum Measure {
    Indeterminate,
    Items { done: u64 },
    Bytes { done: u64, total: Option<u64> },
}
```

`Bytes.total` is `Some` for a copy because an entry now carries its size, which
is what a determinate bar needs and what made item 10 unblocked.

A snapshot of one task is everything the Client knows about it:

```rust
pub(crate) struct TaskInfo {
    pub id: TaskId,
    pub kind: TaskKind,
    pub connection: Option<ConnectionId>,
    pub started: Instant,
    pub finished: Option<Instant>,
    pub state: TaskState,
    pub progress: Progress,
}
```

## Handles

Launching returns a typed handle. The registry keeps the task; the handle is a
view of it plus the channel its results arrive on.

```rust
/// The launching caller's view of one task. Dropping it detaches: the registry
/// still owns the task, and `Client::cancel` still reaches it by ID.
pub(crate) struct Task<T> {
    id: TaskId,
    outcome: oneshot::Receiver<io::Result<T>>,
}

/// A task that delivers items while it runs and settles when the stream ends.
pub(crate) struct Streaming<T> {
    id: TaskId,
    items: mpsc::Receiver<Vec<T>>,
    outcome: oneshot::Receiver<io::Result<()>>,
}
```

`Task<T>` implements `IntoFuture`, so a caller that wants only the value writes
what it writes today:

```rust
let path = client.resolve_path(&connection, &path).await?;
```

This is what makes wrapping *every* operation affordable rather than only the
long ones. Without it, a request-response operation like path resolution would
cost its caller a launch, a poll and a lookup in place of one `await`, which is
a state machine bought for no observable benefit. With it, the call site is
unchanged and the operation is still registered, listed and cancellable while it
runs.

`Streaming<T>` uses a bounded channel, and that bound is the backpressure
contract. Core enumeration is pull-based today, so polling the stream is what
paces the selected host; once a task drains the stream on its own, the channel
capacity takes that role. Both frontends already broke the pull-based property
locally by chunking, so this makes an existing accident into one stated
crate-wide rule.

Dropping a `Streaming<T>` receiver ends its task, because the task's next send
fails. This is what makes item 18 unreachable rather than fixed: a frontend that
loses interest in a listing cannot leave it streaming even if it forgets to
cancel.

## Registry API

```rust
impl Client {
    // Launch. One typed constructor per operation, all funnelling into one
    // internal launch path.
    fn connect(&self, location: ServerLocation) -> (ConnectionId, Task<()>);
    fn disconnect(&self, id: ConnectionId) -> Task<()>;
    fn resolve_path(&self, c: &ConnectionId, p: &HostPath) -> Task<HostPath>;
    fn list_dir(&self, c: &ConnectionId, p: &HostPath) -> Streaming<DirectoryEntry>;
    fn walk_dir(&self, c: &ConnectionId, p: &HostPath) -> Streaming<WalkEntry>;
    fn read_file(&self, c: &ConnectionId, p: &HostPath) -> Streaming<Bytes>;
    fn copy_file(&self, /* source, destination, conflict */) -> Task<CopyResult>;

    // Manage.
    fn tasks(&self) -> Vec<TaskInfo>;
    fn task(&self, id: TaskId) -> Option<TaskInfo>;
    fn cancel(&self, id: TaskId);
    fn cancel_where(&self, predicate: impl Fn(&TaskInfo) -> bool);
    fn subscribe(&self) -> broadcast::Receiver<TaskEvent>;
}
```

Typed constructors rather than one `launch(TaskSpec)` entry point: the operands
differ per operation and a single spec enum would move that difference from the
type system into a runtime match. They all reach one internal launch path, so
registration, cancellation and reporting stay written once.

`cancel` is idempotent and succeeds for an unknown or already terminal ID, for
the same reason `close` does today: a caller holding an ID past its task's end
must not have to check first.

## Observation

```rust
pub(crate) enum TaskEvent {
    Launched(TaskInfo),
    Progressed { id: TaskId, progress: Progress },
    Settled { id: TaskId, state: TaskState },
}
```

A broadcast channel is what keeps the model frontend-agnostic. A select-driven
loop awaits the receiver, a frame-driven loop drains it and requests a repaint,
and a command-line tool ignores it or draws a bar. None of them is the
privileged consumer, and a lagging one is dropped rather than blocking the task.

`Progressed` is coalesced — rate-limited per task, not emitted per entry or per
chunk. A hundred-thousand-entry listing that announced every entry would spend
more on the bus than on the listing.

Two consequences of being frontend-agnostic are load-bearing:

- **The registry lock is `std::sync`, not `tokio::sync`.** `tasks()` and
  `cancel()` must be callable from a synchronous render frame. `Client`'s
  connection registry is a `tokio::sync::RwLock` today, which is exactly why a
  frame-driven frontend cannot inspect it. Critical sections stay short and hold
  no `await`.
- **Snapshots, not borrows.** `tasks()` returns owned `TaskInfo` values. A
  frontend that held a guard across its own rendering would block every launch.

## Connections as tasks

`connect` returns the `ConnectionId` immediately, paired with the task that
brings the connection up.

That is the model's largest change and its largest win. A connection exists in
state `Connecting` from the moment it is asked for, so a frontend can name it,
list it and show it as starting without inventing a parallel record with a
parallel status — which is precisely what `tui::app` maintains today alongside
`ConnectionId`. Tasks launched against a connecting connection are `Queued`
rather than rejected.

It requires an explicit connection state machine:

```text
Connecting -> Ready -> Closing -> Closed
     |                              ^
     +------------------------------+   (startup failure)
```

[Architecture](../architecture.md) currently states the opposite — that shutdown
consumes the connection, so no separate lifecycle state is required. Adopting
this model amends that sentence. The `Arc<RwLock<Option<ServerConnection>>>`
slot stays; what changes is that the slot's states are named and observable
rather than inferred from presence.

## Cancellation

Each task holds a `CancellationToken`, with `JoinHandle::abort` as the backstop.
The token gives graceful cancellation points; abort covers a task wedged
elsewhere. This needs `tokio-util`'s `rt` feature, which the crate does not
enable today.

Three cancellation routes exist, and they compose:

| Route | Effect |
| --- | --- |
| `Client::cancel(id)` | Cancels one task by ID |
| Dropping a `Streaming<T>` receiver | Ends that task when its next send fails |
| `disconnect` | Cancels every task on that connection before closing it |

The third is the structural one. Connection close already cancels the server's
`FsService`, so an in-flight stream reports cancellation rather than a transport
failure; cancelling the connection's tasks first makes the client side of that
deliberate rather than a race the server wins.

Cancelling a copy is already safe: the destination writes a hidden partial file
and publishes it atomically, and task aborts remove the partial. See
[Copy file](../operations/copy-file.md).

## Queueing

`TaskState::Queued` exists in the state machine from the start, with a
per-connection concurrency cap deciding admission. That is item 10's transfer
queue, expressed as a scheduling policy over tasks rather than a queue a
frontend owns beside them.

The cap is a policy, not a mechanism: the default may well be unbounded for
listings and small for transfers. What matters is that the state exists, so
adding the policy later does not add a state every observer must relearn.

## Composition

A copy resolves, reads and writes. A connection start may install a server, then
start it, then health-check it. Both are **one task with phases**, not a task
tree. There is no dependency graph and no parent-child relation.

A task tree would be the right answer if a frontend needed to expand a task into
its constituents, or if a task's failure had to be attributed to a specific
child. Neither is asked for by anything in the TODO, and both are addable later
without changing what already exists — a phase is a field, a child is a new
relation.

A long-lived task — a filesystem watch, if item 12 ever grows one — simply never
reaches a terminal state on its own. It needs no special case.

## What the registry retains

Live tasks, plus a bounded ring of recently settled ones.

Retaining settled tasks is what answers "which failed" and "which completed"
without a frontend keeping its own history. The bound is what keeps it from
being a log: a session that lists ten thousand directories must not accumulate
ten thousand `TaskInfo` values. A settled task is evicted by age within the ring,
oldest first, and its launching caller has already received the outcome through
its handle regardless.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Handle drop | Detaches | The registry is the owner; a task-list pane holds views, and a view's drop must not cancel work |
| Short operations | Also tasks, via `IntoFuture` | Universal coverage with an unchanged call site |
| Terminal retention | Bounded ring | Answers "which failed" without becoming a log |
| Composition | Phases, no task tree | Nothing yet needs attribution to a child |
| Progress delivery | Coalesced push | Per-entry events cost more than the work |
| Registry lock | `std::sync` | A render frame must be able to inspect it |
| Connection ID | Minted before the connection exists | Deletes the frontend's parallel connection record |
| Queueing | `TaskState::Queued` from the start | Adding the state later makes every observer relearn the machine |

## Open questions

- **Task identity across a retry.** Re-expanding a directory that failed is a
  retry. Whether that is a new task or the same task returning to `Running`
  decides whether a frontend keying rows by `TaskId` sees one row or two.
- **Whether `read_file` becomes public here.** It is private today and serves
  copying only. Item 11 wants a bounded, cancellable prefix read; this model
  gives it a natural shape, but the size ceiling and binary detection are that
  item's decisions, not this one's.
- **What a frontend keys stale results on.** `TaskId` can replace the ad-hoc
  request IDs both frontends mint, but only if the tree model stores it, which
  reaches item 5's segment work.
- **Whether `Instant` is the right clock** for a `finished` timestamp a frontend
  wants to display as a wall-clock time.

## Placement

Per [Code structure](../code-structure.md)'s placement rules:

```text
client/
├── task/
│   ├── mod.rs        TaskId, TaskKind, TaskState, Progress, TaskInfo, TaskEvent
│   ├── registry.rs   the collection, its lock, launch, cancel, retention
│   └── handle.rs     Task<T>, Streaming<T>
```

The existing `op_*` bodies become the task bodies unchanged, and `client/rpc`
is untouched. Nothing here reaches the wire: the server already cancels a stream
per request, so this is client-side only and costs no protocol change.

## Build order

1. `task/` types and the registry, with `list_dir` as the only launched
   operation. Two frontends and the test CLI keep working through a thin
   compatibility wrapper.
2. `walk_dir`, `resolve_path`, `read_file`, `copy_file`. The wrapper goes away.
3. `connect` and `disconnect`, with the connection state machine. This is the
   step that amends [Architecture](../architecture.md) and
   [State ownership](../state-ownership.md).
4. Progress and `TaskEvent`, which is what item 10's transfer track reads.
5. `Queued` admission, once there is a transfer queue to admit into.

Steps 1 and 2 retire item 18 by construction. Step 4 is item 10's carrier.

## What this changes elsewhere

| Document | Change |
| --- | --- |
| [Architecture](../architecture.md) | The connection lifecycle sentence, and the Client responsibility list |
| [State ownership](../state-ownership.md) | Task ownership moves from each frontend's column to the Client's |
| [Client API](../api.md) | Every operation's signature |
| [Connection lifecycle](../connection-lifecycle.md) | Startup becomes observable rather than a single `await` |
| [TUI implementation](../tui.md) | `start_tasks`, `path_tasks`, `close_tasks`, the transfer task and the listing tasks all go |
