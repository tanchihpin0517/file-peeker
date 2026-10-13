# Task Pipeline

One task, followed through every process and protocol it crosses: from
`fpk task demo` to a row in the terminal UI's sidebar, and out of it again.
[Task architecture](task-architecture.md) describes the runtime the task runs
in.

## Status

Implemented. Every stage below exists; the only kind that travels it so far is
`demo.count`.

## The path

![Task pipeline: the CLI, the server and the terminal UI, numbered by the sections below](assets/task-pipeline.svg)

Three processes, two sockets in `~/.file-peeker/`: the CLI talks to the server's
public API on `file-peeker.sock`, and the TUI holds a `Watch` stream on the
private client protocol, `file-peeker-client.sock`. The CLI and the TUI never
talk to each other; the server is the only thing both see.

## 1. Submission: the API

`fpk task demo --seconds 3` starts the server if none runs, then sends one line
to the API socket:

```text
-> {"id":1,"method":"task.demo","params":{"seconds":3,"fail":false}}
<- {"id":1,"result":{"id":"6f1c…"}}
```

Both params are optional; `seconds` defaults to 5. The server
([`server::api`](../src/server/api.rs)) builds a `Demo`, calls
`Runtime::submit`, answers with the task's ID, and drops the handle. Dropped, the
handle detaches: the task runs on in the runtime, and the API call returns at
once rather than waiting for it.

## 2. Running: the runtime

`submit` registers the task and sends `Submitted` before it returns. The driver
then takes it through admission:

- `demo.count` is `Background`, capped at one per scope, and requires no
  resource, so its scope is the class alone: **demos run one at a time, in the
  order they were submitted**, and the others wait `Queued { waiting_on: None }`.
- Admitted, it is `Running`. The body ([`server::demo`](../src/server/demo.rs))
  counts `seconds × 10` steps of 100 ms, reporting
  `Progress { phase: "counting", measure: Items { done, total } }` after each,
  and returns `Ok`. With `fail: true` it returns an error halfway.
- Cancelled, it returns `Interrupted` at once — it waits on cancellation and on
  its next step together — and settles `Cancelled`.

Each change goes out as a `TaskEvent` on the runtime's broadcast channel, with
progress coalesced to one event per 100 ms.

## 3. Forwarding: `Watch`

A client that attaches calls `Watch` and holds the stream open. The server
([`server::attach`](../src/server/attach.rs)) answers in this order:

1. **`Hello`**, so the client knows the stream is live.
2. **`TaskSnapshot`**: every task the runtime retains.
3. **`TaskChanged`**, once per runtime event, for as long as the stream is open.
4. On a stopping server, once its tasks have settled, a last **`TaskSnapshot`**,
   and the stream ends.

Three rules keep the client's copy right:

| Rule | Why |
| --- | --- |
| Subscribe to the runtime's events **before** taking the snapshot | Nothing that happens between the two is missed |
| Send each change as the task is **now**, looked up when it is sent, not as the event described it | An event queued behind a snapshot that already shows a later state would otherwise move the client's copy backwards |
| On `Lagged`, send a **fresh snapshot** | A watcher that fell more than 1024 events behind cannot be told what it missed one change at a time |

A change for a task evicted since its event was sent is skipped: there is
nothing current to say about it. The stream ends when the client lets go, or
when a stopping server closes it after its tasks have settled; closing it is also
what lets the transport's graceful shutdown finish.

## 4. The wire

[`client.proto`](../proto/file_peeker/client/v1/client.proto), client protocol
version 3:

```protobuf
message Task {
  string id = 1;
  string parent = 2;                    // empty for a root
  string kind = 3;                      // "demo.count"
  repeated Operand subject = 4;         // seconds = 3
  TaskState state = 5;                  // QUEUED … CANCELLED
  string error = 6;                     // set when FAILED
  bool waiting_on_resource = 7;
  Progress progress = 8;                // phase, and items or bytes
  int64 submitted_unix_millis = 9;
  optional int64 settled_unix_millis = 10;
}
```

[`server::tasks`](../src/server/tasks.rs) converts a runtime `TaskInfo` into
this; [`client::tasks`](../src/client/tasks.rs) converts it back into a
`TaskView`, the client's own type, which names nothing of the runtime. A client
and server whose protocol versions differ refuse to attach at the handshake, so
neither ever decodes a shape it does not know.

## 5. Receiving: the client

[`tui::connection`](../src/tui/connection.rs) holds the attachment. While it is
attached it reads `Watching::next()`, which yields
`TaskUpdate::Snapshot(Vec<TaskView>)` or `TaskUpdate::Changed(TaskView)`, and
hands each to the event loop as `ConnectionEvent::Tasks`. When the stream ends it
reports the machine disconnected and reconnects with backoff; it never starts a
server on its own.

[`tui::app`](../src/tui/app.rs) applies them to the machine's task list:

| Update | Effect |
| --- | --- |
| `Changed`, task live | Inserted or replaced; any linger time cleared |
| `Changed`, task settled | Inserted or replaced; the moment this client saw it settle is recorded, unless already recorded |
| `Snapshot` | Replaces the list. A live task is kept. A settled task is kept only if this client saw it: one already lingering keeps its time; one seen running and found settled now starts lingering now; one never seen is dropped, because it settled longer ago than this client can tell |

The list is kept in submission order. A disconnect keeps it, as herdr keeps a
lost machine's last state on screen; the next snapshot replaces it.

## 6. Showing, and leaving

[`tui::view`](../src/tui/view.rs) draws the selected machine's tasks in the
sidebar's lower half, top down in submission order, a new task joining the end:

```text
 Tasks───────────────────┤
 ✓ demo.count  done      │
 ✗ demo.count  failed    │
 ◐ demo.count  30%       │
 ○ demo.count  queued    │
```

| State | Glyph | Text |
| --- | --- | --- |
| Queued on a resource | `○` | `waiting` |
| Queued on admission | `○` | `queued` |
| Running | `◐` | a share when the total is known, a count when it is not, else the phase, else `running` |
| Cancelling | `◌` | `cancelling` |
| Succeeded | `✓` | `done` |
| Failed | `✗` | `failed` |
| Cancelled | `–` | `cancelled` |

A child's row is indented two columns, before its glyph. Kinds longer than eleven characters
are cut. With no tasks the pane says `No tasks`; a disconnected machine's tasks
are dimmed.

**A settled task leaves the pane five seconds after this client saw it settle.**
The event loop calls `App::expire` on every tick, every 250 ms. The five seconds
run on the client's own clock, never on the server's settle time, because a
remote server's clock need not agree with this one. Each task's five seconds are
its own, so one finishing between two running tasks leaves alone and they keep
their places. Nothing else removes a row: since settled tasks leave on their own
the list stays short, and whatever does not fit is cut at the pane's bottom edge.

The server keeps the task regardless, in its retained roots, for any later
watcher's snapshot — where this client, having seen it end, would not show it
again.

## When the server stops

`fpk server stop`, or `SIGTERM`, takes the same tasks down another way, inside
one five-second budget:

1. The server calls `Runtime::cancel_all`. A running task becomes `Cancelling`,
   then `Cancelled`; a queued one settles `Cancelled` without running. These
   changes reach open `Watch` streams like any other.
2. It waits, with `Runtime::idle`, for every task to settle. A body that ignores
   cancellation is waited for only until the budget runs out.
3. Then it closes the `Watch` streams. Each one's last event is a fresh
   **snapshot**: every task as it ended, whether or not the forwarder had drained
   the changes still queued for it. The rest of the budget goes to clients
   letting go.
4. Each TUI applies that snapshot — a task it saw running now shows
   `cancelled`, and starts its five seconds — then sees the stream end, dims
   the machine, and backs off reconnecting from one second, doubling up to two
   minutes. It starts a server again only when the user presses `s`.

```text
 fpk · local · disconnected
 Tasks───────────────────┤
 – demo.count  cancelled │
```

Tasks do not survive a server restart: a new server starts with an empty
runtime, and its first snapshot is empty.

## Timings

| What | Value | Where |
| --- | --- | --- |
| Demo step | 100 ms | `server::demo` |
| Progress coalescing | 100 ms per task | `task::Config::progress_interval` |
| Event buffer before a watcher lags | 1024 events | `task::Config::event_buffer` |
| TUI redraw and expiry tick | 250 ms | `tui` |
| Settled task lingers | 5 s | `tui::app::SETTLED_LINGER` |
| Stop budget: tasks settling, then clients letting go | 5 s in all | `server` |
| Reconnect backoff | 1 s doubling to 120 s | `tui::connection::Backoff` |
