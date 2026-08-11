# File Peeker v1 Architecture

File Peeker is a read-only browser whose filesystem behavior lives in a
reusable, transport-neutral module. A shared client exposes one typed
asynchronous API to the Ratatui frontend and to the command-line interface.
All of it is one crate producing three binaries.

See [Code Structure](code-structure.md) for the current module plan, operation
placement rules, and the expected structure for future features.

## Terms

| Term | Meaning |
| --- | --- |
| Client host | The machine running the File Peeker client and frontend |
| Server host | The machine running `file-peeker-server` for a remote Session |
| Selected host | The operation target: the client host for a local Session or the server host for a remote Session |
| Local Session | A Session whose server runs inside the client process, on the client host |
| Remote Session | A Session whose server runs on a remote host, launched over SSH |
| `FsService` | The transport-independent service that performs filesystem operations on its own host |

```text
User -> Ratatui ─┐                  ┌-> gRPC -> embedded server -> client-host filesystem
                 ├-> Rust client --┤
User -> CLI     ─┘                  └-> SSH/gRPC -> remote server -> server-host filesystem
```

Both paths are the same path. A client always speaks authenticated gRPC to a
File Peeker server, and that server always calls the same core filesystem
implementation on its own host. “Local” names where the server runs, not a
second way of reaching a filesystem: a local Session's server runs in the client
process and browses the client host, while a remote Session's server runs on the
remote host and browses that one. Filesystem data itself is not redirected
through a client-side virtual filesystem.

## Core, backends, and transport

`crate::fs` owns absolute lexical path resolution, directory enumeration, entry
classification, backpressure, cancellable file reading, and filesystem errors.
It names no protobuf types. That separation is now a placement rule rather than
a manifest boundary; see [Code Structure](code-structure.md#dependency-direction).
Both backends use its `DirectoryEntry` and `EntryKind` value types, which the
Session layer re-exports instead of defining copies of its own. Each `FsService`
instance calls the native filesystem APIs of its own host.

`Client` strongly retains its Sessions by UUID. Every Session owns one server
lifecycle and one authenticated tonic HTTP/2 channel to that server's ephemeral
loopback port. A local Session starts its server on a task inside this process
and connects straight to the port. A remote Session starts one over SSH and
connects through an OpenSSH SOCKS transport; tonic can open a replacement SOCKS
stream when its channel reconnects. The bearer token minted at startup is what
protects a local server's loopback port from other processes on the host.

Core `FsService` executes host-local filesystem primitives on the machine that
owns that core instance, and only the server ever calls it. The Rust client
obtains its selected-host interface through the private async `SessionBackend`
trait. `Session` borrows the open backend through one guarded accessor that
yields the closed-session error when the slot is empty, then delegates
`resolve_path`, `list_dir`, `walk_dir`, `read_file`, `write_file`, and `close`
through that trait. `read_file` is not exposed as a caller-facing native read
operation; it supports the Session copy workflow. There is one implementation,
`ServerBackend`, which turns each primitive into an authenticated RPC;
`ServerConnection` behind it owns the server lifecycle and the channel, and
differs between targets only in which process it started and how a stream
reaches that process. Session retains the backend in an
`RwLock<Option<Box<dyn SessionBackend>>>`: a present backend is open, and a
consumed backend is closing or closed. Backend shutdown consumes the adapter,
so no separate lifecycle state is required.

Native Session workflows are grouped by filesystem domain: path operations live
under `session/path`, directory operations and transport-neutral directory
result types live under `session/directory`, and Session-to-Session file
transfer lives under `session/file`. See
[Copy File](operations/copy-file.md) for that workflow's routing and atomicity
contract.

`SessionBackend` is a private capability seam, not a required one-to-one model
of public Session workflows. `ServerBackend`'s operation modules are private
gRPC request, response, stream, and error-conversion details. Recursive
traversal is the distinct `walk_dir` capability with dedicated entry and stream
types, rather than a `recursive: bool` mode on `list_dir`. An options type
should be added only when traversal has a real policy choice to express.

![SessionBackend flow chart](assets/session-backend-flow.svg)

The caller sees the same operation and result types for either target. The
target changes where the server runs, not the public `Session` interface and not
the client code that reaches it.

The core lists one directory level with `tokio::fs::read_dir` and yields each
classified entry from a pull-based stream. Enumeration advances only when the
consumer polls for another entry, so stream polling provides backpressure
without a producer task or channel. The server applies wire batching, and the
client flattens those transport batches before exposing them to callers. This is
now true of a local Session too: its entries are batched by its own embedded
server and flattened again in the same process. A non-UTF-8 entry terminates the listing with an invalid-argument
error. The exact transport limits are owned by
[Remote Protocol](protocol.md).

Listing success is normal gRPC stream completion. A terminal gRPC status may
follow valid entries, allowing UIs to retain partial results and show an
incomplete state. Neither core nor the server sorts because global sorting would
require collecting the entire directory.

The core walks trees using an explicit depth-first stack with one open
directory frame per active depth. It emits each directory before opening its
descendants, excludes the requested root, and assigns depth 1 to direct
children. Traversal is pull-based, pre-order, and unsorted. Symlinks and special
files are emitted, but only actual directories are descended into, so directory
symlinks are never followed.

A walk is one streaming Walk RPC executing in the selected host's
`FsService::walk_dir`; the client does not recursively issue List RPCs. The
server applies the protocol's Walk batch limits. Valid entries remain visible before terminal filesystem,
cancellation, transport, or malformed-protocol errors. The returned owned stream
does not retain the Session backend lock, so Session close can cancel an active
traversal.

Core file reads expose a demand-driven `ReadStream<Bytes>` without loading the
complete file. A read validates that the opened handle is a regular file before
returning the stream. Successful core chunks are ordered and non-empty, but have
no promised maximum size or semantically stable boundaries; concatenating them
reconstructs the file from byte zero. The server splits core items into
protocol-bounded `ReadChunk` messages, and the backend maps those messages back
into the client byte-stream shape without an intermediate reader. Dropping a
stream releases or cancels only that read.

All clones of one `FsService` share a terminal cancellation state. Cancellation
is idempotent, rejects every later operation, terminates active listings and
walks with a `Cancelled` error, and terminates active read streams with one
error item. Service cancellation is never reported as successful stream
completion or file EOF. Closing a Session cancels its server's `FsService`
before that server stops accepting, so an in-flight stream reports cancellation
rather than a transport failure.

See [Session Lifecycle](session-lifecycle.md) for startup, reconnection and
shutdown behavior.

## Client

The client owns backend and transport concerns:

- Embedded-server lifecycle, and remote SSH startup, installation, and shutdown.
- Sensitive bearer-token metadata and standard gRPC health checks.
- One reconnectable tonic channel per Session.
- One gRPC response stream per active listing, walk, or file read.
- Mapping wire errors to public typed errors.
- Cancelling listing, walk, and read operations when their returned handles are
  dropped.

The UI-facing listing flow is:

```text
Session.op_resolve_path(path) -> absolute lexical selected-host path
Session.op_list_dir(path) -> EntryStream
EntryStream.try_next() -> Some(entry)
EntryStream.try_next() -> Some(entry)
EntryStream.try_next() -> None or error
```

The client deliberately has no browsing `State`, snapshot, cache, or tree. A
native EntryStream retains only the active transport stream and does not retain
entries already returned to the caller.

## UI display ownership

The TUI owns its display and task state independently of the client. It appends
entries in selected-host stream order, retains partial results after a terminal
error, and keeps the Session alive after an individual listing completes. The
client deliberately owns no browsing state, so a second frontend would own its
own tree, selection, and expansion state.

See [Client and UI State Ownership](state-ownership.md) for the ownership matrix
and [TUI Implementation](tui.md) for concrete event routing, navigation, refresh
and shutdown behavior.
