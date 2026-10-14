# File Peeker v2 Architecture

A proposal for rebuilding File Peeker from scratch around two ideas:

1. **Every machine runs a persistent File Peeker server.** It owns everything on
   its host — the tasks, the tabs being browsed, the filesystem access — and keeps
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
│   v                     v            │  bridge  │   tabs, trees             │
│ file-peeker server                   │          │   fs                      │
│   task runtime                       │          │                           │
│   tabs, trees                        │          └───────────────────────────┘
│   fs                                 │
└──────────────────────────────────────┘
```

- **One server per machine, and the server is the session**, as herdr runs one
  server per session. It is started on demand, runs detached, serves only its
  own host's filesystem, and holds the tabs being browsed directly: there is no
  session object inside it to hold them, because a server would only ever hold
  one.
- **Servers never talk to each other.** The client aggregates them, as herdr's
  0.9 does: the local server is one machine among several, not a hub.
- **The client renders the UI.** A server supplies views of its tabs; the
  client draws the chrome, holds the keybindings and theme, and turns input into
  actions sent to the selected server.

## Processes and binary

One binary, `file-peeker`, as herdr has one. Until cutover it is installed as
`fpk`, because the current crate's `file-peeker` occupies that name on the same
machines; the invocations below are written with the final name.

| Invocation | Does |
| --- | --- |
| `file-peeker` | Attach the TUI to the local server, starting one if none runs |
| `file-peeker gui` | Attach the desktop front end the same way |
| `file-peeker server` | Run headless as the server; what auto-start spawns |
| `file-peeker server stop` | Stop the running server through its API socket |
| `file-peeker status` | Client version, server status, protocol compatibility |
| `file-peeker machine add\|list\|remove …` | Manage saved SSH machines |
| `file-peeker --machine <label> <command>` | Run an API command against a saved machine |
| `file-peeker api schema\|snapshot` | Print the API schema, or a snapshot of the server's tabs |
| `file-peeker tab …`, `file-peeker task …`, `file-peeker ls …` | API commands over the socket |
| `file-peeker bridge` | Relay stdio to the local client socket; what SSH runs on a remote host |

Auto-start: the client connects to the socket; if nothing answers, it spawns
`file-peeker server` detached in its own session and process group, and waits for
the socket. A lock file in the machine's home directory makes a second
concurrent start lose rather than run two servers.

The desktop front end stays behind a cargo feature because `eframe` brings a
window system and a GPU backend. A remote install builds without it.

## The server

### Files

Per user and **per machine**: nothing is shared between two machines, even
when they share one home directory, as a cluster's nodes do over NFS. Two
places, both private:

```text
~/.file-peeker/hosts/<machine-id>/   this machine's lasting state, mode 0700
├── config.toml                      client settings
├── machines.toml                    saved SSH machines
├── server.lock                      single-instance lock
├── bin/fpk                          the installed binary, on a remote host
└── logs/
    ├── server.log                   structured
    └── client.log

~/.file-peeker/by-name/<hostname>    -> ../hosts/<machine-id>, for a person reading the tree

/tmp/fpk-<uid>/                      this machine's connections, mode 0700
├── api.sock                         the public API socket
├── client.sock                      the private client protocol socket
└── ssh/%C                           shared SSH connections' control sockets
```

Nothing records the tabs: see [Lifetime](#lifetime).

**Nothing is shared between machines.** A shared home would otherwise break
the server it is meant to find. A unix socket connects only on the machine that
bound it, though every node sees its file; and `flock` on NFS either reaches
across machines — so a second node's server loses the lock to a server it
cannot connect to, and that node can never start one — or does not, so the
second node takes the lock, removes the first node's socket as stale, and binds
its own. One machine's list of SSH machines is not another's either: from a
compute node the jump hosts and addresses differ. And nodes need not share an
architecture or a C library, so neither a binary nor a dev build's target
directory can be shared.

**The machine ID** names the directory: `/etc/machine-id` on Linux, falling back
to `/var/lib/dbus/machine-id`, and `IOPlatformUUID` on macOS (`gethostuuid(2)`),
written as 32 lowercase hex digits. Not the hostname: macOS changes its
hostname with the network, and a key that moves would silently start this
machine over in an empty directory. A machine with no readable ID refuses to
start, saying why, rather than falling back to another key. The probe, the
install scripts and the bridge command are shell run over SSH; each computes the
same key with one shared snippet, tested against the Rust that computes it, and
computes it on every command, since one SSH target may reach a different node
each time.

**`by-name/` is for people.** An ID says nothing to a person looking for a
node's log, so the server links its hostname to its directory as it starts, as
`/dev/disk/by-id` names disks. Nothing in File Peeker follows these links, so a
stale or colliding one costs nothing:

- **Relative**, `../hosts/<machine-id>`, since nodes may mount one home at
  different paths.
- **Replaced atomically**: written under a temporary name and renamed over.
- **A renamed machine leaves no trail**: the server removes every other link to
  its own ID as it writes its current one.
- **Two machines with one hostname**: the later to start holds the link.

**Connections live in `/tmp`, everything else in home.** A unix socket's path is
at most 103 bytes on macOS and 107 on Linux, and OpenSSH needs 17 more while it
binds a new master; under a home directory and a machine ID, `ssh/%C` alone does
not fit. `/tmp/fpk-<uid>/` is short, local to the machine, and resolved from
`getuid()` rather than a variable, as tmux places `/tmp/tmux-<uid>/`. `/tmp` is
shared with every user, so the directory is refused unless this uid owns it and
its mode is `0700`; the host directory is checked the same way, which also
stops a `sudo` that kept `HOME` from writing root-owned files into it.

**The lock stays in home**, because it is what makes one server per machine,
and `/tmp` may be cleaned or split:

- **A cleaner removes a socket.** systemd-tmpfiles and its like delete what has
  not been touched. The server checks its sockets periodically and binds again
  when one is gone, as tmux recreates its own. The lock, in home, is never
  cleaned, so the next client cannot start a second server meanwhile.
- **A session has a `/tmp` of its own** — a container, a systemd service with
  `PrivateTmp`, `pam_namespace` set per session. A client there finds no socket
  and starts a server, which waits out the startup grace on the held lock and
  then says a server already runs on this machine but its socket is not visible
  from this session, rather than running a second one.

**This is where File Peeker departs from herdr**, which splits its files between
`~/.config/herdr/` and `~/.local/state/herdr/`. The XDG variables that would
place a split are often set only in an interactive shell's startup files, and a
remote server is reached by `ssh host fpk bridge`, a non-interactive command
that may not read them. A server started from a login shell and a bridge started
over SSH would then resolve different directories, and the client would start a
second server rather than find the first. `HOME`, the machine ID and the uid
resolve the same way from a login shell, a cron job and an SSH command; for the
same reason `/tmp/fpk-<uid>/` is not `$XDG_RUNTIME_DIR`.

`FILE_PEEKER_HOME` overrides both outright, for tests and development: the host
directory is that directory, and the connections are in its `run/`. A dev build
uses `~/.file-peeker-dev/` and `/tmp/fpk-dev-<uid>/` instead, on the client and
on every host, so a build run from a checkout never shares a binary, a server or
a socket with a release install.

Both sockets are mode `0600`. That replaces the current crate's bearer token: a
unix socket in a private directory is reachable only by its user, and a remote
socket is reached only through that user's SSH session.

### What the server owns

Everything about its own host:

- **The task runtime** and every task on this machine.
- **Tabs.** A tab holds a root path, its tree of listed directories, which are
  expanded, the selected path, the filter query and the sort. This is the
  browsing state the current crate keeps in each front end; here the server
  holds it directly, as herdr's server holds its workspaces and tabs.
- **Filesystem access**, through `fs`, for this host only.

It owns no UI: no layout, palette, keybindings or viewport size beyond what a
client tells it it is showing.

### Lifetime

Everything the server owns lives as long as the server, and no longer:

| Event | Tasks | Tabs and their trees |
| --- | --- | --- |
| Client detaches and reattaches | keep running | kept, live |
| Server or machine restarts | lost | lost: the server starts with none |

- **Nothing is saved.** Unlike herdr, which restores its sessions from a state
  file, a restarted server starts empty, and a client opens tabs again. Saving
  the tabs' shape — and re-listing what was expanded on restore — can come back
  later as a step of its own if restarts turn out to cost too much.
- **Tasks do not survive a server restart.** A copy that was in flight is gone;
  its destination was a hidden partial file published atomically, so nothing
  half-written remains under the real name.

A server keeps running with no client attached. That is the point: a copy
started from a laptop finishes after the lid closes.

## The task runtime

> **Nothing asynchronous happens except as a task.**

The runtime is the top of the server: a client's action becomes a change to a
tab, a tab's change starts tasks, and tasks use the filesystem. The client
runs the same runtime for the little work that is genuinely its own — reaching a
machine, and transfers that span two.

> **Only asynchronous, fallible work is a task.**

Moving the selection, expanding a row whose children are already listed,
typing into a filter and switching tabs are synchronous changes to the tabs. Making
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
| Server | `ListDir`, `WalkDir`, `ReadFile`, `WriteFile`, `LocalCopy`, `DirSize`, `Preview`, `Refresh`, later `Watch` |
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

A resource is something tasks wait for. On the server, a tab is one; on
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
it is going, never what it produced. A `ListDir`'s entries go to the tab that
submitted it, which is what owns the tree; the runtime does not keep them.

## Tabs and views

The server's tabs are its model of what a person is browsing, the equivalent of
herdr's workspaces, tabs and panes. They hang off the server itself: the server
is the session, as herdr's is, so nothing sits between it and its tabs.

```text
Server
└── Tab
    ├── root: PathString
    ├── tree: rows, expansions, the ListDir task each loading row waits on
    ├── selected: PathString          ← a path, not a row index
    ├── filter: String
    └── sort: SortOrder
```

- **The tree lives on the server.** Listings stream into it as tasks emit, and
  stale results are rejected because a row names the `TaskId` it is waiting on.
  Collapsing a row cancels that task in the same place it was started, so the
  current crate's collapse leak has no place to exist.
- **Selection is a path.** An index would be invalidated by every sort and every
  insertion above it, and a tab outlives any one client's view of it.
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
tabs. Two front ends with different keys therefore share one behavior.

Several clients may attach to one server at once and see the same tabs and
selection, as two herdr clients share one focus.

## Machines

A machine is a saved SSH target with a server, as in herdr.

```toml
# machines.toml
[[machine]]
id = "m-7f3a"
label = "workbox"
ssh = "tanch@workbox"
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
| `tab` | list, snapshot, open, close, select, navigate |
| `ls` / `walk` | list a path once and print it, without touching a tab |
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
`[ui]` options such as `mouse_capture`, `[remote]` options. The server reads no UI configuration; it has none. That
answers TODO item 7 by construction. `file-peeker server reload-config` exists for
the few server settings there are, such as admission caps.

## Module structure

v2 is the package `file-peeker` at the repository root. It depends on nothing in
`old/`, the shipping crate's own workspace, renamed `file-peeker-old`.

```text
src/
├── task/          the runtime: ids, kinds, lifecycle, tree, admission, resources, events
├── fs/            host-local filesystem work; task-agnostic, cancelled by being dropped
├── path_string.rs lossless names and paths
├── home.rs        where a machine's files live: its home directory, its connections, its ID
├── browse/        tabs, trees, sorting, selection, filter, typed paths, views, actions
├── server/        the daemon: sockets, lock, auto-start, server kinds, API, client protocol service
├── protocol/      the client protocol schema and the API schema
├── client/        attaching, machines, SSH and the bridge, reconnects, aggregation, client kinds
├── cli/           API commands
├── tui/           the terminal front end: rendering and keys → actions
└── gui/           the desktop front end, feature-gated
```

| Module | May name |
| --- | --- |
| `task`, `path_string`, `home` | nothing else in the crate |
| `fs` | `path_string` |
| `protocol` | `task`, `path_string` |
| `browse` | `task`, `fs`, `path_string` |
| `server` | `task`, `fs`, `browse`, `protocol`, `path_string`, `home` |
| `client` | `task`, `protocol`, `path_string`, `home` — never `server` or `browse` |
| `cli`, `tui`, `gui` | `client`, `protocol`, `home`, `task`'s observer types |

`client` never naming `server` or `browse` is the structural form of "the server
owns the tabs": a front end can only reach browsing state through a view.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Approach | Rewrite as a new crate; no compatibility with the current one | A persistent server inverts where state lives, which no incremental step does cheaply |
| Reference | herdr, for every decision, until File Peeker needs otherwise | The same shape, already worked through |
| Servers | One per machine, persistent, auto-started; the server is the session | Work and state outlive the front end; as herdr, one server per session, so no session object inside it |
| Server-to-server | Never; the client aggregates | herdr 0.9's rule; machines stay independent and a dropped one affects no other |
| Browsing state | The server's tabs, held directly | Reattach returns to the same place; front ends stop owning the tree |
| Rendering | On the client, from server-supplied views | UI settings stay local; the server stays UI-free |
| Input | Semantic actions, keys mapped on the client | Two front ends, one behavior |
| What is a task | Asynchronous, fallible work only | Synchronous changes to the tabs have no duration and cannot fail |
| Kind set | Open trait, closed descriptor | Filesystem work is one family among others |
| Composition | Structured task tree, phases for non-independent steps | Batches, transfers and sizes need children |
| Persistence | None: tabs and tasks live as long as the server | Simplest; herdr restores its sessions, and saving can come back if restarts cost too much |
| Files | Per machine: `~/.file-peeker/hosts/<machine-id>/`, and sockets in `/tmp/fpk-<uid>/` | Machines sharing a home must not meet; a server and an SSH bridge must resolve the same place, which XDG variables may not; socket paths are short |
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
- **Shared selection.** Two clients on one server share one selection, as herdr
  shares focus. If that proves wrong for File Peeker, a per-client cursor over a
  shared tree is the alternative.
- **Named sessions.** herdr runs a second server for a second session name. If
  File Peeker wants one, it is the same: another server with its own sockets,
  lock and state, never a second session inside one server.
- **Machines with one ID.** A VM or container image cloned with its
  `/etc/machine-id` gives two machines one home directory, which is the shared
  case all over again. Left until it is met; regenerating the ID is the fix.
- **Bringing a machine's settings to another.** A new node starts with no
  configuration and no saved machines. A command to import them from another
  machine's directory can come if setting nodes up proves tedious.
- **Copies within one filesystem.** Two nodes sharing a filesystem copy between
  each other through the client, though the bytes never needed to leave it.
  Step 10 may detect it and copy on one side.
- **Task identity across a retry.** The leaning is a new task with
  `retry_of: TaskId`.
- **Servers that talk to each other.** Later operations will have one server
  reach another, which herdr never does. For now the client keeps owning SSH:
  each front end opens its own bridges, and the ControlMaster is OpenSSH's,
  shared by socket path. When server-to-server work arrives, the server running
  it opens its own connections — reusing a master a client already opened,
  since a server cannot answer a login — rather than the local server becoming
  every client's hub.

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
| 4b | The mouse in the TUI, as herdr's — see after step 4 | Clicks land on what was drawn |
| 5 | `PathString`: a name or path held as a string without losing a byte, `%`-escaped, with no `Display` | Any Unix name survives the wire and still opens its file |
| 6 | `fs` over `PathString`, cancelled by being dropped | The filesystem core, with its own tests |
| 6b | One home per machine — see after step 6: machine IDs, sockets in `/tmp`, the host-keyed install | Two nodes sharing a home each run their own server |
| 7 | Tabs and trees on the server, fed by `fs` task kinds, sorting and segmented rows, typed paths | Browsing state on the server |
| 8 | Views and actions in the client protocol; the TUI renders a tab through them | **Local walking skeleton** |
| 9 | The full API beyond `status` and `stop`, its schema, and the CLI over it | Scriptable, as herdr is |
| 10 | Copies: `LocalCopy`, client `Transfer`, batches, byte progress, the transfer admission class | TODO item 10 |
| 11 | `Preview`, `DirSize`, `Refresh` | TODO items 11, 12 and 13 |
| 12 | The desktop front end, as a second client | TODO items 6 and 9 |
| 13 | Cutover: `fpk` becomes `file-peeker`, and `old/` is deleted | One crate again |

Step 8 is the first one a person can use to browse. Machines come before
browsing so that everything after them is built against several machines from
the start, rather than against one and widened later.

### Step 1: an empty server

The first milestone is the process model with nothing inside it: a server that
starts, is found, answers, and stops, holding no tasks and no tabs. Everything
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
  fails halfway when asked, so the pane has something to show before step 7's
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

Left for later: closing an idle bridge, a second server for a named session,
and herdr's version negotiation — here a host whose protocol differs is updated, not negotiated
with.

After step 4, the TUI took the mouse, as herdr's is mouse-first:

- **Clicking a machine selects it**, and the wheel over the machine list moves
  the selection as `j` and `k` do.
- **A key hint under the main pane is a button**: clicking `q detach` or
  `s start the server` presses that key. A hint draws only when its key does
  something, so a button is never there to press for nothing.
- **Drawing and hit testing share one layout**, so a click lands on what was
  drawn under it, at any terminal size.
- **`[ui] mouse_capture`**, default `true` as herdr's is, turns it off for
  keyboard-only use, keeping the terminal's own selection and scrollback. The
  mouse is given back on every exit path, a panic included.

Clicking a task, and herdr's right-click menu, wait for something to act on:
there is nothing to do to a task until cancelling one, and the menu's actions
are machine operations the CLI holds for now.

### Step 5: `PathString`

A Unix name is any bytes but `/` and NUL, so it need not be UTF-8, while a Rust
`String` and a protobuf `string` must be. `PathString` is a name or path held as
a string without losing a byte: the shipping crate's `HostPath`, renamed,
because "host" reads as a machine and this has nothing to do with one.

| The name has | `PathString` holds |
| --- | --- |
| A valid UTF-8 run | It, verbatim |
| A literal `%` | `%%` |
| A byte outside UTF-8 | `%XX`, uppercase hex |

- **It is the file's identity, not its label.** It goes over the wire, keys
  maps, and resolves back to the exact bytes; it is not what a person reads.
  For an ordinary name the two look the same, which is why they are kept apart.
- **No `Display`, as `std::path::Path` has none.** `format!("{path}")` would have
  to pick one meaning, and either is wrong somewhere: the escaped text shows a
  person `%%`, and the readable text, `caf�.txt`, has lost the byte that finds
  the file. Without it a caller says which: `display()` for a person,
  `as_str()` for a program, `to_path_buf()` for the filesystem — exact, with
  nothing expanded; `fs`'s `resolve_path` adds `~` on top.
- **The escape never produces `/`, and is canonical**: joining and prefix tests
  work on the escaped form unchanged, and one file has one spelling.
- **A name is a one-component path**, so one type holds both; `is_name()` tells
  one from a longer path, as a destination filename must be.
- **Linux and macOS only**, so the conversions are Unix's bytes; the shipping
  crate's non-Unix arms are gone.

Ported from `old/src/host_path.rs` with its tests, in
[`path_string`](../../src/path_string.rs); nothing else uses it until step 6.
Constructors with no caller yet — a breadcrumb's fragment, an unvalidated
fixture — come back with what needs them.

### Step 6: `fs`

This host's filesystem, and the only module that touches it: the shipping
crate's `fs`, over `PathString`.

| Operation | Does |
| --- | --- |
| `list_dir` | One directory's entries, streamed, each with one `lstat`'s metadata |
| `walk_dir` | Everything below a directory, pre-order depth first, symlinks emitted but never followed |
| `read_file` | A regular file's bytes, streamed |
| `write_file` | A new file from a stream, through a hidden partial file, published whole or not at all |

- **Cancelling is dropping.** Every operation runs inside a task, and the task
  owns cancelling it, as it owns progress and state: a body selects on its
  context's `cancelled()` and drops the operation. So `fs` holds no token and
  the shipping crate's `FsService` is gone. Each `.await` is a point it stops
  at, and a stream does no work until pulled. A write's partial file is
  unlinked synchronously as it drops, so it is gone before the task reports
  itself cancelled — not left to a spawned cleanup, as the shipping crate did.
- **A path is absolute and opened exactly as given; nothing is interpreted.**
  Reading what a person typed is the job of whatever took it in, as step 7's
  `browse` does for a tab:
  - **Relative paths are refused.** They are relative to something only the
    caller knows, such as a tab's directory, and the server's working
    directory is wherever the client that started it happened to be.
  - **`~` is a name like any other.** Expanding it moves to `browse`, on
    the same server with the same `HOME`, so `~` typed in a remote machine's
    tab is that machine's.
  - **`.` and `..` go to the kernel**, which follows symlinks on the way:
    `/a/link/..` is the parent of where `link` points, which folding the text
    — as the shipping crate did — gets wrong.
  - **No `$VARIABLE`**, here or in `browse`: it would read the server's
    environment, the starting client's, not the user's, and it broke real
    names holding a `$`, such as `Foo$Inner.class`. If typing one is wanted,
    the client expands it from its own environment before sending.
    `shellexpand` is not a dependency.
- **One entry's failure is that entry's.** An entry whose `lstat` is refused
  comes back degraded, with why, and the listing carries on.
- **Errors carry a kind** — invalid argument, already exists, not found,
  permission denied, not a directory, not a file, internal — and a message
  naming the path. There is no cancelled kind: a dropped operation never
  returns.
- **Linux and macOS only**: an entry's mode is a `u32`, not an `Option`.

Ported with its tests in [`fs`](../../src/fs/mod.rs). Names outside UTF-8 are
tested where the filesystem holds them; APFS refuses them, so on macOS only the
literal-marker round trip runs, and Linux CI covers the rest.

### Step 6b: one home per machine

Steps 1 and 4 kept everything in one `~/.file-peeker/`, which machines sharing
a home directory break. This step moves to the [files](#files) the server
section describes; nothing else about the server or the machines changes.

- **`Home` becomes two places**: the host directory,
  `~/.file-peeker/hosts/<machine-id>/`, and the connections directory,
  `/tmp/fpk-<uid>/`. `FILE_PEEKER_HOME` overrides both, the connections going
  in its `run/`.
- **The machine ID** is read once at start, as Linux's `machine-id` or macOS's
  `gethostuuid(2)`, in 32 lowercase hex digits, and a machine without one
  refuses to start. The server links `by-name/<hostname>` to its directory.
- **Both directories are checked**: owned by this uid and mode `0700`, or
  refused, saying which and why.
- **Sockets are renamed** `api.sock` and `client.sock`, and the ControlMaster
  sockets move to `ssh/` beside them. The path-length checks stay, now against
  the shorter paths.
- **The server binds a socket again** when a cleaner removes it, checked
  periodically. A start that loses the lock to a server whose socket this
  session cannot see says so.
- **The host side keys by machine**: the probe, both install scripts and the
  bridge and `--machine` commands compute the ID in shell before naming
  `hosts/<machine-id>/bin/fpk`, with one snippet tested against the Rust. A dev
  install's `src/`, `build/` and `cargo/` move under the host directory too, so
  nodes with different C libraries never share a build. Installs write a
  partial file named for their process and rename it in place.
- `docs/machines.md`, the README and `home.rs`'s documentation follow.

Proven by tests that resolve both places from a given `HOME`, ID and uid; that
refuse a directory with the wrong owner or mode; that replace a renamed machine's
`by-name/` link; that rebind a removed socket;
and that two homes with different IDs under one `HOME` run two servers side by
side.

### Step 7: tabs and trees

The browsing state, on the server and held by it directly, in `browse`. Three
parts, each tested and committed on its own:

| Part | Lands | Proven by |
| --- | --- | --- |
| 7a | `fs` task kinds — `fs.list_dir` first — replacing `demo.count` | A real listing task in the sidebar, started through the API |
| 7b | The tree: the shipping crate's `browser::Tree`, a flat pre-order list grown in place as listings stream in | Unit tests: streamed insertion, collapse cancelling, stale results dropped |
| 7c | Tabs: root, selection by path, filter, sort, and typed paths — `~` and a relative path joined to the tab's directory before `fs` sees them | Unit tests, and a tab driven through the API |

Nothing is drawn yet: step 8's views carry the rows to a client.

## What this replaces at cutover

Every document under `old/docs/` describes the current crate and is replaced,
not amended. v2 grows its own documents as each step lands: architecture, the
task runtime, tabs and views, machines, the client protocol and the API
reference.
