# File Peeker v2 Architecture

A proposal for rebuilding File Peeker from scratch around two ideas:

1. **Every machine runs a persistent File Peeker server.** It owns everything on
   its host — the tasks, the browsing session, the filesystem access — and keeps
   running when no front end is attached. A front end is a client that attaches,
   renders and detaches.
2. **Task management is the top layer of that server.** Every unit of
   asynchronous work is a task with one lifecycle — submitted, admitted, run,
   updated, settled. Filesystem access is one family of task kinds, not the
   architecture.

## Status

This is a **future possibility**, not current behavior. What the shipping crate
does is described by [Architecture](../../old/docs/architecture.md) and the
documents beside it. What v2 has built so far is described under
[docs/](../README.md) — [task architecture](../task-architecture.md) and
[task pipeline](../task-pipeline.md) — and those documents, not this one, own
it.

v2 is a **rewrite, not a migration**. It lives at the repository root and the
current crate lives in `old/`, a workspace of its own. v2 carries no
compatibility with the current crate's wire protocol, layering or document
boundaries, and replaces the current crate when it does everything that crate
does. The current crate is a behavior reference: code may be copied from it where
it fits, never preserved because it exists.

**Every design decision defaults to how [herdr](https://herdr.dev) does it**,
and changes only where File Peeker needs something herdr does not. herdr is a
terminal workspace manager with the same shape this proposal wants — a
persistent server per machine, clients that attach to several of them over SSH,
a public socket API beside a private client protocol. Where this document says
"as herdr does", the behavior is taken from herdr's
[session state](https://herdr.dev/docs/session-state/) and
[connecting machines](https://herdr.dev/docs/connecting-machines/)
documentation and its [0.9 design notes](https://herdr.dev/blog/connecting-the-machines/).

## Why rewrite

The current crate has three problems that are one problem.

- **Task handling exists three times.** `tui::app`, `tui::browser_context` and
  `gui::app` each grew their own request IDs, stale-result rejection, aborts and
  shutdown joins, at three levels of correctness; the GUI's collapse leaks a
  listing (TODO item 18).
- **All state dies with the front end.** The local server runs inside the front
  end's process and a remote server lives on the SSH session's stdin. Closing the
  terminal ends every listing and every copy, and the next start begins from an
  empty screen.
- **The front ends are fat.** They own the tree, the connections, the transfer
  and every task, so each new front end starts from zero and gets a different
  subset right.

Fixing these in place means keeping two front ends alive through every
intermediate step. A persistent server inverts where state lives, which no
incremental step can do cheaply.

## Overview

```text
 your machine                                       remote host
┌──────────────────────────────────────┐          ┌───────────────────────────┐
│ TUI · GUI · CLI   (clients)          │          │                           │
│   │ attach              │ api        │   SSH    │ file-peeker server        │
│   │                     │            │ ───────> │   task runtime            │
│   v                     v            │  bridge  │   sessions (tabs, trees)  │
│ file-peeker server  (default session)│          │   fs                      │
│   task runtime                       │          │                           │
│   sessions (tabs, trees)             │          └───────────────────────────┘
│   fs                                 │
└──────────────────────────────────────┘
```

- **One server per machine per session name**, as herdr does. It is started on
  demand, runs detached, and serves only its own host's filesystem.
- **Servers never talk to each other.** The client aggregates them, as herdr's
  0.9 does: the local server is one machine among several, not a hub.
- **The client renders the UI.** A server supplies views of its sessions; the
  client draws the chrome, holds the keybindings and theme, and turns input into
  actions sent to the selected server.

## Processes and binary

One binary, `file-peeker`, as herdr has one. Until cutover it is installed as
`fpk`, because the current crate's `file-peeker` occupies that name on the same
machines; the invocations below are written with the final name.

| Invocation | Does |
| --- | --- |
| `file-peeker` | Attach the TUI to the local server, starting one if none runs |
| `file-peeker --session <name>` | The same, for a named session |
| `file-peeker gui` | Attach the desktop front end the same way |
| `file-peeker server` | Run headless as the server; what auto-start spawns |
| `file-peeker server stop` | Stop the running server through its API socket |
| `file-peeker status` | Client version, server status, protocol compatibility |
| `file-peeker machine add\|list\|remove …` | Manage saved SSH machines |
| `file-peeker --machine <label> <command>` | Run an API command against a saved machine |
| `file-peeker api schema\|snapshot` | Print the API schema, or the live session snapshot |
| `file-peeker tab …`, `file-peeker task …`, `file-peeker ls …` | API commands over the socket |
| `file-peeker bridge` | Relay stdio to the local client socket; what SSH runs on a remote host |

Auto-start: the client connects to the socket; if nothing answers, it spawns
`file-peeker server` detached in its own session and process group, and waits for
the socket. A lock file beside the socket makes a second concurrent start lose
rather than run two servers.

The desktop front end stays behind a cargo feature because `eframe` brings a
window system and a GPU backend. A remote install builds without it.

## The server

### Files

Per user, in one directory, `~/.file-peeker/`, mode `0700`:

```text
~/.file-peeker/
├── config.toml              client settings
├── machines.toml            saved SSH machines
├── server.lock              single-instance lock
├── file-peeker.sock         the public API socket
├── file-peeker-client.sock  the private client protocol socket
├── logs/
│   ├── server.log           structured
│   └── client.log
└── sessions/
    ├── default.json         the default session's saved shape
    ├── <name>.json          a named session's
    ├── snapshots/           recovery snapshots
    └── backups/             unreadable session files, preserved
```

**This is where File Peeker departs from herdr**, which splits its files between
`~/.config/herdr/` and `~/.local/state/herdr/`. The XDG variables that would
place a split are often set only in an interactive shell's startup files, and a
remote server is reached by `ssh host fpk bridge`, a non-interactive command
that may not read them. A server started from a login shell and a bridge started
over SSH would then resolve different directories, and the client would start a
second server rather than find the first. One directory resolved from `HOME`
alone resolves the same way from a login shell, a cron job and an SSH command.
`FILE_PEEKER_HOME` overrides it outright, for tests and development. A dev
build uses `~/.file-peeker-dev/` instead, on the client and on every host, so a
build run from a checkout never shares a binary, a server or a socket with a
release install.

Both sockets are mode `0600`. That replaces the current crate's bearer token: a
unix socket in a private directory is reachable only by its user, and a remote
socket is reached only through that user's SSH session.

### What the server owns

Everything about its own host:

- **The task runtime** and every task on this machine.
- **Sessions.** A session holds tabs; a tab holds a root path, its tree of
  listed directories, which are expanded, the selected path, the filter query
  and the sort. This is the browsing state the current crate keeps in each front
  end.
- **Filesystem access**, through `fs`, for this host only.

It owns no UI: no layout, palette, keybindings or viewport size beyond what a
client tells it it is showing.

### Lifetime and persistence

As herdr does, persistence has two strengths:

| Event | Tasks | Session shape | Listed contents |
| --- | --- | --- | --- |
| Client detaches and reattaches | keep running | kept | kept, live |
| Server restarts | lost | restored from `sessions/default.json` | re-listed on restore |
| Machine restarts | lost | restored from `sessions/default.json` or a snapshot | re-listed on restore |

- **The session shape is saved, the contents are not.** A session file holds
  tabs, root paths, expanded paths, the selection, filter and sort — never
  directory entries. Restoring re-lists what was expanded, so a restored tab
  shows the filesystem as it is now. A path that no longer exists restores as
  that tab's error, not as a silent move to the home directory, as herdr keeps a
  pane whose directory is gone.
- **Saves are debounced** on change and on shutdown.
- **Snapshots**: up to 48 in `sessions/snapshots/`, at most one per 15 minutes. An
  unreadable session file is moved to `sessions/backups/`, keeping three, and is
  never restored automatically.
- **Tasks do not survive a server restart.** A copy that was in flight is gone;
  its destination was a hidden partial file published atomically, so nothing
  half-written remains under the real name.

A server keeps running with no client attached. That is the point: a copy
started from a laptop finishes after the lid closes.

## The task runtime

> **Nothing asynchronous happens except as a task.**

The runtime is the top of the server: a client's action becomes a session
change, a session change starts tasks, and tasks use the filesystem. The client
runs the same runtime for the little work that is genuinely its own — reaching a
machine, and transfers that span two.

> **Only asynchronous, fallible work is a task.**

Moving the selection, expanding a row whose children are already listed,
typing into a filter and switching tabs are synchronous session changes. Making
every keystroke a task would buy a lifecycle for work with no duration that
cannot fail.

### Lifecycle

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

- **Queued** — registered, waiting on a resource or on admission.
- **Running** — its body is executing. Progress updates arrive only here.
- **Cancelling** — cancellation was requested and the body is cleaning up, as a
  copy removing its partial file does. A state, because the time is real and a
  client shows it.
- **Succeeded**, **Failed**, **Cancelled** — terminal; never changes again.

### Kinds

```rust
pub(crate) struct TaskId(Uuid);

/// What a task does. The kind set is open: the runtime names no filesystem
/// type, and each layer implements the kinds it owns.
pub(crate) trait TaskKind: Send + 'static {
    /// Delivered while the task runs: entries, chunks. `()` for none.
    type Item: Send + 'static;
    /// Delivered once, on success.
    type Output: Send + 'static;

    fn describe(&self) -> TaskDescriptor;
    fn requires(&self) -> Vec<ResourceId>;

    async fn run(self, cx: TaskContext<Self::Item>) -> io::Result<Self::Output>;
}

/// What a task is, for every observer that did not submit it — and the shape
/// the API and the client protocol both carry.
pub(crate) struct TaskDescriptor {
    pub kind: KindName,          // stable, such as `fs.list_dir`
    pub subject: Vec<Operand>,   // typed operands: paths, tabs, machines
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

pub(crate) struct Progress {
    pub phase: Option<PhaseName>,
    pub measure: Measure,        // Indeterminate | Items { done, total } | Bytes { done, total }
}
```

A trait rather than an enum is what makes filesystem work "one kind of task": a
new family is a new implementation rather than a variant every match must learn.
The descriptor is the one closed shape, so a task list renders any kind without
knowing it.

| Runs on | Kinds |
| --- | --- |
| Server | `ListDir`, `WalkDir`, `ReadFile`, `WriteFile`, `LocalCopy`, `DirSize`, `Preview`, `Refresh`, `Restore`, later `Watch` |
| Client | `AttachMachine` (SSH, bridge, install with approval), `Transfer` (a copy between two machines) |

### The task context

A kind's body sees only its context; that is the whole contract between a kind
and the runtime.

```rust
impl<I> TaskContext<I> {
    fn cancelled(&self) -> WaitForCancellationFuture<'_>;
    fn progress(&self, progress: Progress);              // coalesced
    async fn emit(&self, items: Vec<I>) -> Result<(), Detached>;  // bounded: backpressure
    fn spawn<K: TaskKind>(&self, kind: K) -> Handle<K>;  // a child
    fn resource<R: Resource>(&self, id: ResourceId) -> ResourceGuard<R>;
}
```

### The task tree

Tasks form a structured tree: a child started through `TaskContext::spawn`
cannot outlive its parent, cancelling a parent cancels its subtree, and a parent
does not settle until its children have. A child's failure comes back to the
parent through the child's handle; the parent decides whether that fails it. A
parent's progress may be declared as the sum of its children's.

| Work | Shape |
| --- | --- |
| Copy within one machine | server `LocalCopy`, whose bytes never leave the host |
| Copy between machines | client `Transfer` → server `ReadFile` on the source, server `WriteFile` on the destination |
| Copy a selection | a parent per batch, one child per entry, admitted under `Transfer` |
| Directory size | server `DirSize` → `WalkDir`, aggregated on the server |
| Restore a session | server `Restore` → one `ListDir` per expanded path |
| Attach a machine | client `AttachMachine`, with phases connect → probe → install → start rather than children |

A step that is not independently cancellable, observable or retryable is a
phase, not a child. There is no general dependency graph: a parent awaiting its
children is ordinary Rust.

How a task ends, as step 3 built it:

- **A body that returns while children still run** has them cancelled, and
  settles only once they have.
- **After a cancel**, a body that returns an `Interrupted` error settles
  `Cancelled`; one that returns any other error settles `Failed`, because its
  cleanup failed; and one that returns success settles `Succeeded`, because it
  finished the work and saying otherwise would be untrue.
- **A body that panics** settles `Failed`. Each body runs as a tokio task of its
  own, so a panic reaches the driver as a join error rather than leaving a task
  `Running` forever.
- **Summing children's progress into the parent's** is deferred to step 10,
  where copies are its first consumer.

A copy between machines is the one place bytes pass through a client. Servers do
not talk to each other, so the client relays the source server's read stream into
the destination server's write. That follows herdr's rule that the client
aggregates and servers stay independent.

### Resources and admission

A resource is something tasks wait for. On the server, a session tab is one; on
the client, an attached machine is one, with the states
`Pending → Ready → Closing → Gone`. A task that requires a `Pending` resource is
`Queued` with `waiting_on` set, so a client can submit work against a machine
that is still attaching.

Each kind declares an admission class, and the runtime holds a cap per class:

| Class | Default cap | Why |
| --- | --- | --- |
| `Interactive` | unbounded | Listings and previews are what the user is waiting on |
| `Transfer` | 2 per machine | Bandwidth is shared; ten copies finish no sooner |
| `Background` | 1 per machine | Directory sizes must not starve a listing |

The numbers are policy; the `Queued` state is the mechanism, and it exists from
the start.

Step 3 settled what the table leaves open:

- **A cap counts per scope**: a task's class and the first resource it
  requires, or the class alone for a task that requires none. Two machines'
  transfers do not share a cap.
- **A capped queue is first in, first out.** Roots join their scope's queue
  when they are submitted, and only the head may take a free place. A queue the
  user watches has to drain in the order it filled; waking every waiter and
  letting the fastest win does not guarantee that. A task cancelled while
  queued leaves the line, and the one behind it moves up.
- **Only roots are capped.** A child runs under its parent's admission, so a
  parent holding the last place of its class can still start children rather
  than deadlock waiting for itself.

### Observation and retention

```rust
pub(crate) enum TaskEvent {
    Submitted(TaskInfo),
    StateChanged { id: TaskId, state: TaskState },
    Progressed { id: TaskId, progress: Progress },
}
```

Events go out on a broadcast channel, `Progressed` coalesced per task: at most
one per interval, and the latest value always sent before the task settles. The
registry lock is `std::sync` so a synchronous caller can inspect it, and
`tasks()` returns owned snapshots. The runtime keeps live tasks plus a bounded
ring of recently settled roots, which answers "which failed" without becoming a
log.

**The runtime owns lifecycle, never payload.** It stores what a task is and how
it is going, never what it produced. A `ListDir`'s entries go to the session that
submitted it, which is what owns the tree; the runtime does not keep them.

## Sessions and views

A session is the server's model of what a person is browsing, the equivalent of
herdr's workspaces, tabs and panes.

```text
Session
└── Tab
    ├── root: HostPath
    ├── tree: rows, expansions, the ListDir task each loading row waits on
    ├── selected: HostPath          ← a path, not a row index
    ├── filter: String
    └── sort: SortOrder
```

- **The tree lives on the server.** Listings stream into it as tasks emit, and
  stale results are rejected because a row names the `TaskId` it is waiting on.
  Collapsing a row cancels that task in the same place it was started, so the
  current crate's collapse leak has no place to exist.
- **Selection is a path.** An index would be invalidated by every sort and every
  insertion above it, and the session outlives any one client's view of it.
- **Sorting and segmented storage are designed in** (TODO items 4 and 5): a
  listing appends while it streams and sorts when it settles, and rows are stored
  so that an expansion does not move the rows below it on every group.

### Views

A client never receives a whole tree. It subscribes to a **view**: one tab, and
the window of rows it is showing.

```text
client                          server
  │  subscribe(tab, offset, height) │
  │ ──────────────────────────────> │
  │  view: rows[offset..+height],   │
  │        row count, selection,    │
  │        tab status               │
  │ <────────────────────────────── │
  │  view delta  (on every change)  │
  │ <────────────────────────────── │
  │  action: SelectNext             │
  │ ──────────────────────────────> │
```

As herdr streams screens only for the selected machine and sends everything else
as summaries, a client gets **one full view** — the tab it is showing — and
**summaries** for every other tab and machine: title, root, loading state, a
failure, task counts. A hundred-thousand-entry directory costs the client what
fits on screen.

### Actions

Input becomes semantic actions, not keystrokes: `SelectNext`, `SelectPath`,
`Toggle`, `Enter`, `Up`, `SetFilter`, `SetSort`, `OpenTab`, `CloseTab`,
`Refresh`, `Copy`, `Cancel(TaskId)`. The client owns the keybindings, as herdr's
client does, and maps keys to actions; the server owns what an action does to the
session. Two front ends with different keys therefore share one behavior.

Several clients may attach to one session at once and see the same selection,
as two herdr clients share one focus.

## Machines

A machine is a saved SSH target with a server, as in herdr.

```toml
# machines.toml
[[machine]]
id = "m-7f3a"
label = "workbox"
ssh = "tanch@workbox"
session = "default"
enabled = true
```

- **Transport is SSH.** With `remote.manage_ssh_config = true` the client keeps
  one OpenSSH ControlMaster connection per machine (`ControlPersist 600`) and
  shares it across attaching, the API and background reconnects. With it off,
  the user's own SSH configuration is used as is.
- **The bridge.** The client runs `file-peeker bridge` over that SSH connection.
  The bridge starts the remote server if none runs, then relays stdio to the
  remote `file-peeker-client.sock`. There is no listening TCP port anywhere and no
  socket path forwarding to configure.
- **Install is approval-based.** `file-peeker machine add` probes for an
  installed binary and, if none is found or it is incompatible, offers to install
  one, built without the desktop front end. **Background reconnects only discover
  an installed server; they never install, update or restart one.**
- **Versions are independent.** Updating a client does not replace a running
  remote server; `file-peeker status` reports whether each server's protocol is
  compatible, and an incompatible one shows as needing attention rather than
  being replaced behind the user's back.
- **Disconnects are expected.** A machine that drops keeps its last views
  visible but dimmed, with input disabled until a fresh view arrives. Reconnects
  back off up to two minutes. A bridge idle for a minute is closed, and the
  remote server and its tasks keep running.
- **Nothing is copied to a remote host** beyond the binary the user approved:
  not `config.toml`, not keybindings, not credentials.

Local is a machine like any other, except that it needs no SSH and opens
immediately at startup without waiting for the remote ones.

## Protocols

Two surfaces, as herdr has two sockets.

### The client protocol

Private, versioned by one integer, between a front end and a server, on
`file-peeker-client.sock` or through the bridge. It carries attach and detach,
view subscriptions and deltas, actions, task events, and the byte streams of a
cross-machine transfer.

It is gRPC over the stream the socket or the bridge provides. That is a choice on
the merits rather than an inheritance: several concurrent streams per
connection, per-stream flow control for the transfer relay and for large view
deltas, and typed messages from one schema. tonic serves a unix socket directly,
and a bridge is any duplex byte stream.

A client and a server whose client-protocol versions differ do not attach; the
client says why and what `status` reports.

### The API

Public, documented, on `file-peeker.sock`, and the same surface the CLI drives —
as herdr's CLI and socket API are one surface. Requests and responses are
newline-delimited JSON with a published schema (`file-peeker api schema`), so a
script or another tool can drive File Peeker without linking it.

| Area | Operations |
| --- | --- |
| `server` | status, stop, reload-config |
| `session` | list, snapshot, stop |
| `tab` | list, open, close, select, navigate |
| `ls` / `walk` | list a path once and print it, without touching a session |
| `task` | list, show, cancel, wait |
| `machine` | through `--machine <label>`, any of the above on a saved machine |

`task wait` blocks until a task settles, with no default timeout, as herdr's wait
commands do. Errors are JSON on stderr with exit status 1; invalid syntax exits
with status 2.

The API carries a generation number of its own, separate from the client
protocol's version, so the private protocol can change freely while the public
one stays stable.

## Configuration

Client-side only, in `config.toml`: theme and palette, layout, keybindings,
`[remote]` options. The server reads no UI configuration; it has none. That
answers TODO item 7 by construction. `file-peeker server reload-config` exists for
the few server settings there are — admission caps and the save debounce.

## Module structure

v2 is the package `file-peeker` at the repository root. It depends on nothing in
`old/`, the shipping crate's own workspace, renamed `file-peeker-old`.

```text
src/
├── task/          the runtime: ids, kinds, lifecycle, tree, admission, resources, events
├── fs/            host-local filesystem work; task-agnostic, takes a CancellationToken
├── host_path.rs   lossless names and paths
├── home.rs        where a server's files live: sockets, lock, log
├── session/       tabs, trees, sorting, selection, filter, views, actions, persistence
├── server/        the daemon: sockets, lock, auto-start, server kinds, API, client protocol service
├── protocol/      the client protocol schema and the API schema
├── client/        attaching, machines, SSH and the bridge, reconnects, aggregation, client kinds
├── cli/           API commands
├── tui/           the terminal front end: rendering and keys → actions
└── gui/           the desktop front end, feature-gated
```

| Module | May name |
| --- | --- |
| `task`, `fs`, `host_path`, `home` | nothing else in the crate |
| `protocol` | `task`, `host_path` |
| `session` | `task`, `fs`, `host_path` |
| `server` | `task`, `fs`, `session`, `protocol`, `host_path`, `home` |
| `client` | `task`, `protocol`, `host_path`, `home` — never `server` or `session` |
| `cli`, `tui`, `gui` | `client`, `protocol`, `home`, `task`'s observer types |

`client` never naming `server` is the structural form of "the server owns the
session": a front end can only reach browsing state through a view.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Approach | Rewrite as a new crate; no compatibility with the current one | A persistent server inverts where state lives, which no incremental step does cheaply |
| Reference | herdr, for every decision, until File Peeker needs otherwise | The same shape, already worked through |
| Servers | One per machine per session name, persistent, auto-started | Work and state outlive the front end |
| Server-to-server | Never; the client aggregates | herdr 0.9's rule; machines stay independent and a dropped one affects no other |
| Browsing state | Owned by the server's session | Reattach returns to the same place; front ends stop owning the tree |
| Rendering | On the client, from server-supplied views | UI settings stay local; the server stays UI-free |
| Input | Semantic actions, keys mapped on the client | Two front ends, one behavior |
| What is a task | Asynchronous, fallible work only | Synchronous session changes have no duration and cannot fail |
| Kind set | Open trait, closed descriptor | Filesystem work is one family among others |
| Composition | Structured task tree, phases for non-independent steps | Batches, transfers, sizes and restore need children |
| Persistence | Session shape saved, contents and tasks not | Restores reflect the filesystem as it is now |
| Files | One directory, `~/.file-peeker/`, from `HOME` alone | A server and an SSH bridge must resolve the same place; XDG variables may differ between them |
| Local transport | Unix sockets, mode `0600` | Replaces the bearer token with the filesystem's own access control |
| Remote transport | SSH, ControlMaster, a stdio bridge | No listening port, no socket forwarding to configure |
| Remote install | With approval only; background reconnects never install | A server on someone's host is theirs to agree to |
| Surfaces | Private client protocol plus public JSON API | The client protocol can change; the API stays scriptable |
| Client protocol | gRPC over the socket or bridge | Concurrent streams with flow control, for views and transfer relay |
| Cross-machine copy | Relayed through the client | The cost of independent servers, paid only when two machines are involved |
| Platforms | Linux and macOS | As herdr, and as the current crate's CI already is |

## Open questions

- **Latency of round-tripped input.** Selection movement goes to the server and
  back. On a local socket that is negligible; on a slow SSH link it may need the
  client to move the cursor optimistically and reconcile.
- **Shared selection.** Two clients on one session share one selection, as herdr
  shares focus. If that proves wrong for File Peeker, a per-client cursor over a
  shared tree is the alternative.
- **Task identity across a retry.** The leaning is a new task with
  `retry_of: TaskId`.

## Build order

Each step is something v2 can do that it could not before. Nothing in the current
crate changes along the way.

| Step | Lands | Proves |
| --- | --- | --- |
| 0 | v2 at the repository root and the shipping crate in `old/`, both under `make verify` and CI | Neither crate can rot |
| 1 | **An empty server** — see below | **A server that outlives its client** |
| 2 | **An empty TUI client** — see below | **A front end that attaches, detaches and survives its server** |
| 3 | The task runtime, with test kinds only | Lifecycle, tree, cancellation, admission, events |
| 4 | **Machines** — see below: releases, approval-based install, the bridge, ControlMaster, several machines in the TUI, `--machine` | **Several machines in one client** |
| 5 | `fs` and `host_path` | The filesystem core, with its own tests |
| 6 | Sessions: tabs and trees fed by server `ListDir`, sorting and segmented rows | Browsing state on the server |
| 7 | Views and actions in the client protocol; the TUI renders a tab through them | **Local walking skeleton** |
| 8 | Persistence: `sessions/default.json`, snapshots, backups, restore | Reattach and restart |
| 9 | The full API beyond `status` and `stop`, its schema, and the CLI over it | Scriptable, as herdr is |
| 10 | Copies: `LocalCopy`, client `Transfer`, batches, byte progress, the transfer admission class | TODO item 10 |
| 11 | `Preview`, `DirSize`, `Refresh` | TODO items 11, 12 and 13 |
| 12 | The desktop front end, as a second client | TODO items 6 and 9 |
| 13 | Cutover: `fpk` becomes `file-peeker`, and `old/` is deleted | One crate again |

Step 7 is the first one a person can use to browse. Machines come before
browsing so that everything after them is built against several machines from
the start, rather than against one and widened later.

### Step 1: an empty server

The first milestone is the process model with nothing inside it: a server that
starts, is found, answers, and stops, holding no tasks and no sessions. Everything
after it is a feature added to a server that already runs.

- `file-peeker server` runs headless: takes `server.lock`, creates
  `~/.file-peeker/` mode `0700`, binds `file-peeker.sock` and
  `file-peeker-client.sock` mode `0600`, writes `logs/server.log`, and removes both
  sockets on exit.
- `file-peeker` with no server running spawns `current_exe() server` detached in
  its own session, waits for the client socket, and attaches. With one running,
  it attaches to it. A second concurrent start loses the lock and attaches to the
  winner instead of running two servers.
- **Attach is a handshake and nothing more**: one client-protocol call exchanging
  versions. A mismatch refuses to attach and says why.
- The API socket answers `server.status` and `server.stop`, which back
  `file-peeker status` and `file-peeker server stop`.
- A stale socket left by a crashed server is detected — the lock is free but the
  socket exists — and replaced rather than refused.
- A lock held with no socket is a server that has not bound yet or is exiting;
  a starting server retries for a second rather than losing to it, which is what
  makes `fpk server stop && fpk` start a fresh server.
- `FILE_PEEKER_HOME` overrides the directory, so tests and development run a
  server that cannot meet the user's real one.
- `SIGTERM` and `server stop` both shut down cleanly; a client detaching does not.

It proves the claim the whole design rests on: closing the client leaves the
server running, and the next `file-peeker` finds it.

### Step 2: an empty TUI client

The front end's process model with nothing inside it: `fpk` opens a full-screen
terminal UI attached to the server, shows that it is attached, and leaves the
server running when it quits. Every later front-end step draws into a shell that
already attaches, detaches and reconnects.

- **`fpk` opens the TUI.** It attaches as step 1 does — starting a server if none
  runs — then takes the terminal: alternate screen, raw mode, hidden cursor. The
  screen is the chrome a browser will later fill: a title bar with the attached
  server's version and pid, a sidebar, an empty body that says there is nothing
  to browse yet, and a status line with the keys.
- **The sidebar lists machines, as herdr's does.** It holds `local` and only
  `local` until step 4 reads `machines.toml`; the list is a list from the start,
  so adding machines adds rows rather than a layout. Each row shows the machine's
  label and a state — attaching, attached, disconnected — and the selected row is
  the one the body and the title describe. `j`/`k` and the arrow keys move the
  selection, which with one machine is a no-op but is the binding later steps
  inherit.
- **Quitting detaches.** `q` and `Ctrl-C` leave the TUI; the server keeps running
  and the next `fpk` attaches to it again. The terminal is restored on every exit
  path, a panic included.
- **The attachment is live.** After the handshake the client holds one
  server-streaming call, `Watch`, open for as long as it is attached. Step 2's
  server sends nothing on it but its opening event; what matters is that its end
  is how a client learns the server went away.
- **A lost server shows, as herdr shows a lost machine.** When `Watch` ends, the
  machine's row and the body dim, its state reads `disconnected`, input other than quitting is off,
  and the client re-attaches with backoff — doubling from one second, capped at
  two minutes. **Reconnecting never starts a server**, as herdr's background
  reconnects never install or restart one: a server that was stopped on purpose
  stays stopped until the user presses `s` to start it.
- **Without a terminal, `fpk` refuses** and says to use `fpk server start`, a new
  command that starts the server if none runs, prints what `status` prints, and
  exits. That is what scripts and the end-to-end tests use in place of a bare
  `fpk`.
- Resizing redraws. Rendering is a pure function of the client's state, so it is
  tested against a test backend rather than a real terminal.

After step 3, the sidebar gained its lower half, as herdr splits its sidebar:

- **The sidebar's lower half lists the selected machine's tasks** in the order
  they were submitted, top down, as a queue reads: a new task joins the end.
  Nothing decides what to leave out: settled tasks leave on their own, so the
  list stays short, and what does not fit is cut at the pane's bottom edge.
- **A settled task stays five seconds, then leaves the pane.** Long enough to
  see how it ended; the server still retains it. The five seconds run on the
  client's clock from when the client saw it settle, not on the server's settle
  time, because a remote server's clock need not agree. A snapshot's settled
  task the client never saw end settled longer ago than it can tell, and is not
  shown.
- **The key hints sit under the main pane alone**; the sidebar runs the full
  height beside them. Each row is a glyph
  for the state, the kind, and a share when the task knows its total, a count
  when it does not, its state otherwise. A disconnected machine's
  tasks stay on screen, dimmed, until the next snapshot replaces them.
- **`Watch` carries the server's tasks**: a snapshot after `Hello`, then every
  change. A change is sent as the task is *now*, looked up when it is sent, so a
  client applying them in order never moves a task backwards past a snapshot.
  A watcher that falls behind the runtime's event buffer gets a fresh snapshot.
- **The server holds a task runtime** from here on, and cancels every task when
  it stops. Its one kind so far is `demo.count`, started by `fpk task demo`
  through the API's `task.demo`: it counts for a while, reports progress, and
  fails halfway when asked, so the pane has something to show before step 6's
  real kinds. It is `Background`, so a second one queues behind the first.

### Step 4: machines

Several machines in one client, as herdr's 0.9 has them. Nothing browses yet, so
what a remote machine shows is its own server's tasks — which is enough to prove
the connection, the reconnects and the aggregation everything later rides on.

- **Releases, as herdr publishes them.** Pushing a `v*` tag builds `fpk` for
  Linux and macOS on x86_64 and aarch64 and publishes the four binaries to
  GitHub releases as `fpk-<os>-<arch>`, with a `SHA256SUMS`, as herdr publishes
  `herdr-<os>-<arch>`. The Linux builds are static (musl), so one binary runs on
  any distribution's C library, old cluster nodes included. A tag that does not
  match the crate's version fails the release. Releases are only what release
  builds install; nothing in development waits on one.
- **Install is approval-based, and follows how the client was built.**
  `fpk machine add <label> <ssh-target>` probes the host for
  `~/.file-peeker/bin/fpk` and what it is. When none is there, or it is not the
  build this client needs, setup says what it found and asks before installing.
  Declined or failed, the machine is not saved. Running `machine add` again on a
  saved label probes again and offers to update. Where the binary comes from is
  decided by the client's own build profile, which Rust fixes at compile time:

  | | A release build | A dev build |
  | --- | --- | --- |
  | Is | `cargo build --release`, and every published binary | `cargo build`, `cargo run` |
  | Installs by | Downloading the release of **this client's version** from GitHub, as herdr does | Uploading **the source tree this binary was built from** and building it on the host |
  | The host needs | Outbound HTTPS, and `curl` or `wget` | A Rust toolchain |
  | Reinstalls when | The host's version differs | The source differs: a hash of the uploaded files, recorded beside the binary |

  A **release** install runs a short script on the host over SSH: `uname` picks
  the asset, the download is checked against `SHA256SUMS`, and the binary lands
  at `~/.file-peeker/bin/fpk` — never on the host's `PATH`, never anywhere else.
  A version with no release cannot be installed this way, and setup says so
  rather than guessing at another; a local `--release` build of an unreleased
  version is the one place that bites.

  A **dev** install uploads the files `git ls-files` lists under the directory
  the binary was compiled in — recorded at build time from
  `CARGO_MANIFEST_DIR` — and runs `cargo install --debug --locked` on them into
  `~/.file-peeker/bin/`, debug because it builds far faster and a dev host is
  for trying things. The client hashes what it uploads, the host records that
  hash once the build succeeds, and a later probe that finds a different hash
  offers to rebuild. Editing the source and running `machine add` again is how
  a remote host catches up. This is the shipping crate's source install, made
  the rule for dev builds rather than a flag.

  Either way one host holds one `fpk`, and the probe says which it is:
  `release v0.1.0`, or `dev, source a1b2c3d4`.
- **`machines.toml`** holds what `machine add` saved: an ID, the label, the SSH
  target, and whether it is enabled. `fpk machine list` shows them and
  `fpk machine remove <label>` forgets one, leaving its server and tasks
  running.
- **The bridge.** The client runs `ssh <target> ~/.file-peeker/bin/fpk bridge`,
  and speaks the client protocol over that process's standard input and
  output; on the host, the bridge relays them to `file-peeker-client.sock`. No
  port listens anywhere and no socket path is forwarded. **A bridge never
  installs and never starts a server**: starting one is `fpk server start` over
  the same SSH, which setup runs and `s` asks for.
- **SSH is shared, as herdr shares it.** With `[remote] manage_ssh_config =
  true` in `config.toml` — the default — the client adds `ControlMaster=auto`, a
  `ControlPath` under `~/.file-peeker/ssh/` and `ControlPersist=600`, so setup,
  every reconnect and `--machine` ride one authenticated connection. Set it to
  `false` to use your own SSH configuration unchanged. This is the first thing
  v2 reads from `config.toml`.
- **A reconnect never prompts.** Setup and `fpk machine login <label>` run SSH
  interactively, so a password or a keyboard-interactive login can be answered
  there. Background reconnects run it with `BatchMode=yes`; a host that wants an
  answer shows as needing a login until the user runs `machine login`.
- **The TUI lists every machine.** `local` first, then each enabled saved
  machine in the order it was added, each with its own connection and backoff.
  `local` opens at once rather than waiting for the remote ones. Selecting a
  machine shows its tasks in the lower half. Besides attaching, attached and
  disconnected, a machine can **need attention** — not installed, incompatible,
  or needing a login — and the main pane says which and what to run. `s` on a
  disconnected remote machine starts its server over SSH.
- **`--machine <label>` runs a command on a saved machine**: `fpk --machine
  workbox task demo` runs `~/.file-peeker/bin/fpk task demo` there over the
  shared connection and passes its output through.

Left for later: closing an idle bridge, named sessions, and herdr's version
negotiation — here a host whose protocol differs is updated, not negotiated
with.

## What this replaces at cutover

Every document under `old/docs/` describes the current crate and is replaced,
not amended. v2 grows its own documents as each step lands: architecture, the
task runtime, sessions and views, machines, the client protocol and the API
reference.
