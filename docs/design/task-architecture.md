# Task Architecture

A proposal for making task management the top layer of File Peeker, on both
sides of the wire. Every unit of asynchronous work — opening a connection,
listing a directory, copying a file — is a *task*: submitted, admitted, run,
updated and settled through one lifecycle that one runtime owns. Filesystem
access stops being the architecture and becomes one family of task kinds among
others.

## Status

This is a **future possibility**, not current behavior. Today
[`Client`](../../src/client/mod.rs) owns a connection registry and nothing else:
each operation is an `async fn` returning a future or an owned stream, and
cancellation happens when the caller drops what it was given. The server is a
tonic service whose handlers call [`FsService`](../../src/fs/fs_service.rs)
directly. [Architecture](../architecture.md), [Client API](../api.md) and
[Protocol](../protocol.md) own what is actually built. This document owns the
proposed model only.

It is **not** a migration of that code. The new architecture starts as a new
crate beside the current one, and features are ported into it one at a time. The
current crate keeps shipping, unchanged, until the new one carries everything it
does; see [How it is built](#how-it-is-built).

It supersedes an earlier, narrower proposal that put a task registry *inside*
`Client`. That version kept the Client as the center and tasks as its
bookkeeping. This one inverts the layering: the runtime is the center, and the
Client's operations are tasks it runs.

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

The server has the same gap from the other side. A request is visible to the
server only as a handler future inside tonic; nothing on the server can say what
it is doing, and nothing on the client can ask it. A copy between two
connections is two unrelated RPCs on two servers, and the client is the only
place that knows they are one transfer.

The duplication is the defect, not the leak. The work is also about to grow:
transfer progress and queueing (item 10), several connections at once (item 9),
recursive directory sizes (item 13) and preview reads (item 11) each add work
that needs to be watched rather than merely awaited.

## The idea

> **Nothing asynchronous happens except as a task.**

A task is the unit that is submitted, admitted, run, observed, cancelled and
reported on. The runtime that holds them is the top of each process: a front end
talks to the runtime, the runtime runs task kinds, and task kinds use
connections and the filesystem.

```text
front ends (TUI · GUI · CLI)   submit · observe · cancel · render
        │
crate::task  Runtime           registry · admission · lifecycle · tree · events · retention
        │ runs
task kinds                     connection: Connect · Disconnect
                               fs:         ResolvePath · ListDir · WalkDir · ReadFile · CopyFile
                               later:      TransferBatch · DirSize · Preview · Watch
        │ use
resources and transport        ServerConnection · gRPC · crate::fs   (unchanged)
```

The server runs the same runtime. Each RPC it serves is a server task, and the
client sees it as the remote child of the client task that issued it.

Two rules bound it.

> **A task owns lifecycle, never payload.**

The runtime stores what a task *is* and how it is *going*: its kind, its
operands, its resources, its state, its progress, its error. It never stores
what the task *produced*. Entries, bytes and resolved paths travel to the
submitter through the task's own typed channel and are not retained. That keeps
the boundary [Architecture](../architecture.md) already states — the Client
layer owns no browsing state, snapshot, cache or tree — and it keeps
backpressure where it is today: in a bounded channel and in gRPC flow control,
not in an event bus.

> **Only asynchronous, fallible work is a task.**

Expanding a row, moving the selection, typing into a filter and switching tabs
are synchronous changes to front-end state. They stay what they are. Making
every keystroke a task would buy a lifecycle for work that has no duration and
cannot fail.

## Terms

| Term | Meaning |
| --- | --- |
| Task | One unit of asynchronous work, from submission to a terminal state |
| Task kind | The type that says what a task does and carries its operands |
| Runtime | The per-process owner of every task: registry, admission, tree, events |
| Resource | Something tasks run against and wait for, such as a connection |
| Handle | The submitter's typed view of one task |
| Root task | A task with no parent; the runtime itself owns it |
| Remote child | The server task that serves one RPC a client task issued |
| Settle | Reaching a terminal state: Succeeded, Failed or Cancelled |

## Lifecycle

```text
submit ──> Queued ──> Running ──> Succeeded
             │         │  ^  │
             │         └──┘  └──> Failed
             │       progress
             │         │
             │ cancel  │ cancel
             v         v
         Cancelled <── Cancelling ──> Failed   (cleanup itself failed)
```

- **Queued** — submitted and registered, waiting on a resource or on admission.
  A task submitted against a connection that is still connecting is queued, not
  rejected.
- **Running** — its body is executing. Progress updates arrive only here.
- **Cancelling** — cancellation was requested and the body is cleaning up. A copy
  removing its partial destination file spends visible time here.
- **Succeeded**, **Failed**, **Cancelled** — terminal. A task in one never
  changes again.

`Cancelling` is a state rather than an instant because the time between asking
and settling is real and a front end shows it.

## Types

```rust
pub(crate) struct TaskId(Uuid);

/// What a task does. Implemented once per kind, by the layer that owns the
/// work: `client` for the client-side kinds, `server` for the server-side ones.
pub(crate) trait TaskKind: Send + 'static {
    /// Delivered while the task runs: entries, chunks. `()` for none.
    type Item: Send + 'static;
    /// Delivered once, on success.
    type Output: Send + 'static;

    fn describe(&self) -> TaskDescriptor;
    fn requires(&self) -> Vec<ResourceId>;

    async fn run(self, cx: TaskContext<Self::Item>) -> io::Result<Self::Output>;
}

/// What a task is, for everyone who did not submit it.
pub(crate) struct TaskDescriptor {
    pub kind: KindName,          // a stable name, such as `fs.list_dir`
    pub subject: Vec<Operand>,   // typed operands: paths, connections, names
    pub admission: AdmissionClass,
}

pub(crate) enum TaskState {
    Queued { waiting_on: Option<ResourceId> },
    Running,
    Cancelling,
    Succeeded,
    Failed(Arc<io::Error>),
    Cancelled,
}
```

The kind set is **open**. A trait rather than an enum is what makes filesystem
work "one kind of task": the runtime names no filesystem type, a new family of
kinds is a new implementation rather than a new variant every match must learn,
and the server can register kinds the client never runs. The observer-facing
`TaskDescriptor` is the one closed shape, so a task list can render any kind
without knowing it.

`Failed` shares its error because several observers read one failure: the
submitter, a task list, and a status line. `io::Error` is not `Clone`.

Progress is two independent axes. A copy is in a phase *and* has a byte count;
collapsing them into one enum forces every kind to the widest variant.

```rust
pub(crate) struct Progress {
    pub phase: Option<PhaseName>,
    pub measure: Measure,
}

pub(crate) enum Measure {
    Indeterminate,
    Items { done: u64 },
    Bytes { done: u64, total: Option<u64> },
}
```

A snapshot of one task is everything the runtime knows about it:

```rust
pub(crate) struct TaskInfo {
    pub id: TaskId,
    pub parent: Option<TaskId>,
    pub descriptor: TaskDescriptor,
    pub origin: Origin,          // Local, or Remote { connection }
    pub submitted: SystemTime,
    pub settled: Option<SystemTime>,
    pub state: TaskState,
    pub progress: Progress,
}
```

`SystemTime`, not `Instant`, because a front end displays these as wall-clock
times and because a remote task's times cross the wire.

## The task context

A kind's body sees only its context. That is the whole contract between a kind
and the runtime.

```rust
impl<I> TaskContext<I> {
    /// Resolves when cancellation is requested. Bodies select on it at their
    /// natural cancellation points.
    fn cancelled(&self) -> WaitForCancellationFuture<'_>;
    /// Coalesced: the runtime rate-limits what it publishes.
    fn progress(&self, progress: Progress);
    /// Sends to the submitter's bounded channel. Awaiting it is backpressure;
    /// an error means the submitter is gone and the task should end.
    async fn emit(&self, items: Vec<I>) -> Result<(), Detached>;
    /// Starts a child. It is cancelled with this task and cannot outlive it.
    fn spawn<K: TaskKind>(&self, kind: K) -> Handle<K>;
    /// Borrows a resource this task declared in `requires`.
    fn resource<R: Resource>(&self, id: ResourceId) -> ResourceGuard<R>;
}
```

The existing `op_*` bodies on the client and the handler bodies on the server
become `run` bodies with almost no change: they already take a connection or an
`FsService`, already stream, and already observe a cancellation token.

## Handles and submission

```rust
impl Runtime {
    fn submit<K: TaskKind>(&self, kind: K) -> Handle<K>;

    fn tasks(&self, filter: TaskFilter) -> Vec<TaskInfo>;
    fn task(&self, id: TaskId) -> Option<TaskInfo>;
    fn cancel(&self, id: TaskId);
    fn subscribe(&self, filter: TaskFilter) -> broadcast::Receiver<TaskEvent>;
}

/// The submitter's view of one task. Dropping it detaches: the runtime still
/// owns the task, and `Runtime::cancel` still reaches it by ID.
pub(crate) struct Handle<K: TaskKind> {
    id: TaskId,
    items: mpsc::Receiver<Vec<K::Item>>,
    outcome: oneshot::Receiver<io::Result<K::Output>>,
}
```

`Handle<K>` implements `IntoFuture` when `K::Item` is `()`, so a caller that
wants only the value writes what it writes today:

```rust
let path = runtime.submit(ResolvePath::new(connection, path)).await?;
```

That is what makes wrapping *every* operation affordable rather than only the
long ones. A request-response operation would otherwise cost its caller a
submit, a poll and a lookup in place of one `await`.

Dropping the item receiver ends a streaming task, because its next `emit` fails.
A front end that loses interest in a listing cannot leave it streaming even if it
forgets to cancel. That is what makes item 18 unreachable rather than fixed.

`cancel` is idempotent and succeeds for an unknown or already terminal ID, for
the same reason `close` does today: a caller holding an ID past its task's end
must not have to check first.

## Resources

A connection is not owned by a front end or by a `Client` struct. It is a
resource in the runtime, brought up by a `Connect` task and taken down by a
`Disconnect` task.

```rust
pub(crate) enum ResourceState {
    Pending,   // its producing task is running
    Ready,
    Closing,   // its consuming task is running
    Gone,      // closed, or its producing task failed
}
```

`Runtime::submit(Connect::new(location))` returns a handle whose `ResourceId` is
known **before** the connection exists. A front end can name the connection,
list it and show it as connecting at once, without the parallel record
[`tui::app::connection`](../../src/tui/app/connection.rs) keeps today beside
`ConnectionId`. A task that requires a `Pending` resource is `Queued` with
`waiting_on` set; one that requires a `Gone` resource fails at admission.

```text
Pending -> Ready -> Closing -> Gone
   |                            ^
   +----------------------------+   (Connect failed)
```

`Disconnect` cancels every task that requires the connection before it closes
it. Connection close already cancels the server's `FsService`, so an in-flight
stream reports cancellation rather than a transport failure; cancelling the
client tasks first makes that deliberate rather than a race the server wins.

The resource table is generic. A connection is the only resource today; a
destination directory under a write lock, or a rate-limited remote host, would
be another.

## Admission

Every kind declares an `AdmissionClass`, and the runtime holds a concurrency cap
per class per resource. That is item 10's transfer queue, expressed as a
scheduling policy rather than a queue a front end owns beside the tasks.

| Class | Default cap per connection | Why |
| --- | --- | --- |
| `Interactive` | unbounded | Path resolution and listings are what the user is waiting on |
| `Transfer` | 2 | Bandwidth is shared; ten parallel copies finish no sooner |
| `Background` | 1 | Directory sizes and watches must not starve a listing |

The numbers are policy, not mechanism. What matters is that `Queued` exists from
the start, so adding a cap later does not add a state every observer must
relearn.

## The task tree

Tasks form a tree. A task started through `TaskContext::spawn` is a child of the
task that started it; a task started through `Runtime::submit` is a root.

The tree is **structured**:

- A child cannot outlive its parent. Cancelling a parent cancels its subtree,
  and a parent does not settle until its children have.
- A child's failure is returned to the parent through the child's handle, like
  any other result. The parent decides whether that fails it; the runtime does
  not decide for it. A directory copy skips an unreadable file and reports it,
  while a single-file copy fails.
- A parent's progress may be declared as the sum of its children's, so a
  directory copy's byte bar is derived rather than reimplemented.

What the tree buys, concretely:

| Work | Shape |
| --- | --- |
| Copy one file | `CopyFile` → remote `Read` on the source server, remote `Write` on the destination server |
| Copy a selection | `TransferBatch` → one `CopyFile` per entry, admitted under `Transfer` |
| Directory size | `DirSize` → remote `Walk`, aggregated |
| Open a connection | `Connect`, with phases install → start → health rather than children |

Phases still exist. A step that is not independently cancellable, observable or
retryable is a phase of its task, not a child. Connection startup is the example:
its steps cannot be retried separately, so it reports phases.

There is no general dependency graph. Ordering inside a subtree is the parent's
code awaiting its children, which is ordinary Rust; a scheduler that resolved
declared dependencies would be a second way to express what `await` already
does.

## Across the wire

The server runs the same `crate::task` runtime. Each RPC it serves is a server
task, and every server task knows which client task asked for it.

### Identity

The client mints the `TaskId` of a remote child and sends it with the request, in
request metadata:

```text
x-file-peeker-task: <uuid>          the remote child's ID
x-file-peeker-parent: <uuid>        the client task that issued it
```

One ID then names one piece of work on both sides, so neither side translates
IDs and a client can cancel a server task it has not yet heard about. The server
rejects a duplicate ID with `ALREADY_EXISTS` rather than trusting the client to
mint UUIDs well. A request without the header still works and gets a
server-minted ID, so a data RPC stays callable without the task machinery — from
a test, or from `grpcurl`.

### The `Tasks` service

```protobuf
service Tasks {
  rpc Watch(WatchRequest) returns (stream TaskEvent);
  rpc Cancel(CancelRequest) returns (CancelResponse);
}
```

- **`Watch`** streams the server's task events, coalesced as the client's are.
  The client runtime holds one `Watch` per connection and folds its events into
  the remote children it registered, so a task list shows server-side progress —
  a walk's directory count, a write's bytes flushed to disk — without any data
  RPC changing.
- **`Cancel`** cancels a server task and its subtree by ID. Dropping the data
  stream still cancels it as well; `Cancel` exists for work whose stream the
  client does not hold, which is exactly the server-originated work below.

The data RPCs carry payload; `Tasks` carries lifecycle. That split is the
"lifecycle, never payload" rule drawn on the wire, and it is why `List`, `Walk`,
`Read` and `Write` keep their typed messages and their flow control rather than
becoming one generic `Submit` returning a stream of tagged events.

### Server-originated work

Once the server can run and report tasks of its own, two things become possible
that the request-per-RPC model cannot express:

- **Same-host copy.** When a copy's source and destination are the same
  connection, the client submits one server task, `LocalCopy`, and the bytes
  never cross the wire. Today they make a round trip through the client.
- **Server-side aggregation.** `DirSize` walks and sums on the server and reports
  progress through `Watch`, sending the client one number instead of every entry.

A server task's lifetime is bounded by its connection. The server already exits
when its stdin lease closes; a task that should survive a dropped SSH session is
a different product — a daemon — and is out of scope.

### Compatibility

There is none to keep. The new crate carries its own copy of the proto, and
nothing the current crate builds talks to a new server, so the wire may change
freely until cutover. The data RPCs keep their typed shape for flow control, not
for compatibility. What needs care is the remote install: a new server must not
overwrite the `file-peeker-server` the current client provisions on the same
host, so until cutover it installs under its own name.

## Observation

```rust
pub(crate) enum TaskEvent {
    Submitted(TaskInfo),
    StateChanged { id: TaskId, state: TaskState },
    Progressed { id: TaskId, progress: Progress },
}
```

A broadcast channel keeps the model front-end-agnostic. A select-driven loop
awaits the receiver, a frame-driven loop drains it and requests a repaint, and a
command-line tool ignores it or draws a bar. None of them is the privileged
consumer, and a lagging one is dropped rather than blocking a task — it recovers
with a `tasks()` snapshot.

`Progressed` is coalesced, rate-limited per task rather than emitted per entry or
per chunk. A hundred-thousand-entry listing that announced every entry would
spend more on the bus than on the listing.

Two consequences of being front-end-agnostic are load-bearing:

- **The registry lock is `std::sync`, not `tokio::sync`.** `tasks()` and
  `cancel()` must be callable from a synchronous render frame. `Client`'s
  connection registry is a `tokio::sync::RwLock` today, which is exactly why a
  frame-driven front end cannot inspect it. Critical sections stay short and
  hold no `await`.
- **Snapshots, not borrows.** `tasks()` returns owned `TaskInfo` values. A front
  end that held a guard across its own rendering would block every submission.

## What the runtime retains

Live tasks, plus a bounded ring of recently settled roots with their subtrees.

Retaining settled tasks is what answers "which failed" without a front end
keeping its own history. The bound is what keeps it from being a log: a session
that lists ten thousand directories must not accumulate ten thousand `TaskInfo`
values. The submitter has already received the outcome through its handle
regardless.

## Front ends

A front end holds a `Runtime`, not a `Client`. What it keeps is front-end state
only: rows, selection, scroll, filter, tabs, layout.

- **Stale results are rejected by `TaskId`.** The request IDs both front ends
  mint today, and the root generation counter in `tui::browser_context`, become
  the `TaskId` of the listing a row is waiting on. A group arriving from a task
  the row no longer names is dropped.
- **The tree stores that `TaskId`.** `browser::Tree::collapse` returns the IDs it
  dropped, as it returns request IDs today, and the front end calls
  `Runtime::cancel` on each. That reaches item 5's segment work, which decides
  where an in-flight expansion is stored.
- **A task list is a view, not a model.** A transfer track, an inspector queue or
  a status line is `subscribe` plus `tasks()`, rendered. No front end owns a
  transfer task any more.

Ported this way, the TUI arrives without `start_tasks`, `path_tasks`,
`close_tasks`, its transfer task or its listing task tables, and its connection
rows are views of connection resources. The GUI arrives with the cancellation it
never had. Neither is rewritten in place: each is ported onto the runtime once
the kinds it needs exist.

## Module placement

The new crate lives at `crates/next/` as a second workspace member. `next` is a
working name; it becomes `file-peeker` at cutover.

```text
crates/next/src/
├── task/              the runtime — written new
│   ├── mod.rs         TaskId, TaskKind, TaskState, Progress, TaskInfo, TaskEvent
│   ├── runtime.rs     Runtime: registry, lock, submit, cancel, retention
│   ├── admission.rs   classes, caps, the queue
│   ├── resource.rs    ResourceId, ResourceState, the resource table
│   ├── context.rs     TaskContext
│   ├── handle.rs      Handle<K>
│   └── remote.rs      the metadata headers and TaskEvent ⇄ protobuf conversion
├── fs/                ported almost verbatim
├── host_path.rs       ported verbatim
├── protocol.rs        ported, then extended with the `Tasks` service
├── server/            ported; handlers become server kinds
├── client/            ported; operations become client kinds, connections resources
├── browser/           ported, with items 4 and 5 designed in
├── cli/  tui/  gui/   ported last, onto the runtime
```

| Module | May name |
| --- | --- |
| `task` | `protocol`, and nothing else in the crate |
| `fs` | nothing else in the crate — it stays task-agnostic and keeps taking a `CancellationToken` |
| `server` | `fs`, `protocol`, `task` |
| `client` | `fs`, `protocol`, `server`, `task` |
| front ends | `client`'s kinds, `browser`, `task` |

`task` sits beside `fs` and `protocol` at the bottom of the dependency order even
though it is the top of the runtime order. That is not a contradiction: it is
the mechanism every layer is run by, so every layer may name it, and it names
none of them. `fs` does not implement `TaskKind`; `server` and `client` wrap it.
Both the server and the client builds enable it, so the slim server build
carries it.

There is no `Client` struct. The client's kinds are `client::kind::{Connect,
Disconnect, ResolvePath, ListDir, WalkDir, ReadFile, CopyFile}`, and the
connection registry is the connection resource.

The new crate does not depend on the current one. A dependency would make the
current crate's layering part of the new one's, which is the thing being
replaced, and would keep the current crate alive past cutover.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| How it is built | A new crate, features ported one at a time | Migrating in place spends most of the work on keeping both front ends alive through every step |
| Top of each process | The task runtime | Every front end and both binaries share one lifecycle instead of reimplementing it |
| Scope | Client and server | The server's work becomes observable and cancellable from the client, and server-originated work becomes possible |
| What is a task | Asynchronous, fallible work only | Synchronous UI state changes have no duration and cannot fail |
| Kind set | Open trait, closed descriptor | Filesystem work is one family among others; observers still render any kind |
| Payload | Typed channels and typed RPCs, never the runtime | Keeps backpressure, and keeps the Client from becoming a cache |
| Composition | Structured task tree, phases for non-independent steps | Batches, directory copy and sizes need children; startup does not |
| Dependency graph | None | `await` inside a parent already orders its children |
| Handle drop | Detaches; dropping the item receiver ends a stream | The runtime is the owner; a forgotten listing still stops |
| Short operations | Also tasks, via `IntoFuture` | Universal coverage with an unchanged call site |
| Remote identity | Client-minted ID in request metadata | One ID on both sides; cancel before the server has reported |
| Wire change | Additive: metadata plus a `Tasks` service | Data RPCs keep their shape and their flow control |
| Server task lifetime | Bounded by its connection | Surviving a dropped session is a daemon, not this |
| Queueing | `Queued` and admission classes from the start | Adding a state later makes every observer relearn the machine |
| Registry lock | `std::sync` | A render frame must be able to inspect it |
| Clock | `SystemTime` | Displayed as wall-clock time, and crosses the wire |
| Retention | Bounded ring of settled roots | Answers "which failed" without becoming a log |

## Open questions

- **Task identity across a retry.** Re-expanding a directory that failed is a
  retry. The leaning is a new task carrying `retry_of: TaskId`, so a settled task
  stays settled; the cost is that a front end keying rows by `TaskId` re-keys on
  retry.
- **Whether `ReadFile` becomes a public kind.** It is private today and serves
  copying only. Item 11 wants a bounded, cancellable prefix read; this model
  gives it a natural shape, but the size ceiling and binary detection are that
  item's decisions.
- **How much of `Watch` to send.** Every server task, or only those with a
  parent the client registered. The first lets one client see another's work on
  a shared server, which nothing asks for yet.
- **The first milestone.** Either the runtime alone, finished and tested, or a
  walking skeleton: the runtime, a local connection resource and `ListDir`,
  driven end to end by one CLI subcommand.
- **The new crate's binary names until cutover**, and so the remote install
  path's name for a new server.
- **Whether the CLI keeps a facade.** The test CLI's subcommands are one
  operation each; a thin synchronous facade over `Runtime` may read better there
  than handles.

## How it is built

The current crate is the reference, not the starting point. It keeps shipping
and receives bug fixes only; a feature that the new architecture changes the
shape of is built once, in the new crate, when its area is ported.

**Porting a feature** means bringing its behavior and its tests across, then
reshaping it onto the runtime. Tests are what make a port verifiable: a feature
counts as ported when the current crate's tests for it pass against the new
code, adapted for the new call shape but asserting the same behavior. Code that
the runtime does not touch — `fs`, `host_path` — is copied rather than rewritten.

**Cutover** happens once every row of the porting order is done: the new crate
takes the `file-peeker` name and its binaries' names, the current crate is
deleted, and the documents under `docs/` are replaced by the new crate's.

## Porting order

Each row is something the new crate can do on its own that it could not before.

| Step | What lands | Ported from | Proves |
| --- | --- | --- | --- |
| 0 | `crates/next` as a workspace member, covered by `make check` and CI | — | The second crate cannot rot |
| 1 | The task runtime: lifecycle, tree, cancellation, events, admission, retention, resources | new | The runtime on its own, with test kinds only |
| 2 | `fs` and `host_path`, with their tests | `src/fs`, `src/host_path.rs` | The copied core passes its own suite |
| 3 | The proto, the server's listener and authentication, handlers as server kinds, the `Tasks` service | `proto/`, `src/server`, `build.rs` | A server whose every request is a task |
| 4 | The local connection as a `Connect` resource; `ResolvePath`, `ListDir`, `WalkDir` as client kinds | `src/client/connection/embedded.rs`, `src/client/rpc`, `src/client/directory` | Remote children mirrored through `Watch` |
| 5 | The CLI's `list` and `walk` subcommands | `src/cli` | The first end-to-end path a person can run |
| 6 | The SSH connection, `ensure-server.sh` and remote install, under the new server's name | `src/client/connection/ssh.rs`, `src/cli/install.rs` | Remote browsing |
| 7 | `ReadFile`, `Write`, `CopyFile` with remote children and byte progress; `TransferBatch`; `LocalCopy` | `src/client/file/copy.rs`, `src/server/ops/write.rs` | Item 10 |
| 8 | The browser model, with sorting (item 4) and segmented storage (item 5) designed in, keyed by `TaskId` | `src/browser` | A tree the front ends can share |
| 9 | The TUI on the runtime | `src/tui` | Feature parity in the terminal |
| 10 | The GUI on the runtime, holding several connections | `src/gui` | Item 9; item 18 gone by construction |
| 11 | Cutover | — | One crate again |

Steps 1 and 2 do not depend on each other. Everything after 3 depends on both.

## What this changes elsewhere

Nothing until cutover. The current documents describe the current crate, and
stay true of it while it ships. The new crate grows its own documents as each
step lands; at cutover they replace these:

| Document | What its replacement has to say |
| --- | --- |
| [Architecture](../architecture.md) | The runtime as the top layer; connections as resources |
| [Code structure](../code-structure.md) | `task/` and its row in the dependency table; no `Client` struct |
| [State ownership](../state-ownership.md) | Task and connection ownership in the runtime's column, not each front end's |
| [Client API](../api.md) | Every operation as a kind submitted to the runtime |
| [Protocol](../protocol.md) | The metadata headers and the `Tasks` service |
| [Connection lifecycle](../connection-lifecycle.md) | Startup as an observable `Connect` task with phases |
| [TUI implementation](../tui.md) | Without the task tables and the connection record's state |
