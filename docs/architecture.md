# File Peeker v1 Architecture

File Peeker is a read-only browser whose filesystem behavior lives in a reusable
Rust core. A shared client exposes the same typed asynchronous API to Ratatui
and to SwiftUI through UniFFI.

See [Code Structure](code-structure.md) for the current module plan, operation
placement rules, and the expected structure for future features.

## Terms

| Term | Meaning |
| --- | --- |
| Client host | The machine running the File Peeker client and frontend |
| Server host | The machine running `file-peeker-server` for a remote Session |
| Selected host | The operation target: the client host for a local Session or the server host for a remote Session |
| Local Session | A Session backed by an in-process client-host `FsService` |
| Remote Session | A Session backed by SSH, gRPC and a server-host `FsService` |
| `FsService` | The transport-independent service that performs filesystem operations on its own host |
| `FileService` | The private client-host workflow facade for staging files and invoking operating-system integration |

```text
User -> Ratatui ─┐                  ┌-> native core -> client-host filesystem
                 ├-> Rust client --┤
User -> SwiftUI ─┘                  └-> SSH/gRPC -> server -> remote core -> remote host filesystem
```

Both paths use the same core filesystem implementation. “Local” is relative to
the process running that core: a local Session accesses the client-host
filesystem, while a remote Session asks the server-host core to access its local
filesystem. Filesystem data itself is not redirected through a client-side
virtual filesystem.

## Core, backends, and transport

`file-peeker-core` owns absolute lexical path resolution, directory enumeration,
entry classification, backpressure, cancellable file reading, and filesystem
errors. It has no protobuf or UniFFI dependency.
Both backends use its `DirectoryEntry` and `EntryKind` value types. The client
exposes those types to Swift with UniFFI remote-type metadata instead of
defining client-owned copies. Each core instance calls the native filesystem
APIs of its own host.

`Client` strongly retains its Sessions by UUID. A local Session owns an
in-process core service and starts no executable or network connection. A
remote Session owns one SSH/server lifecycle and one authenticated tonic HTTP/2
channel. Remote channels connect through an OpenSSH SOCKS transport; tonic can
open a replacement SOCKS stream when its channel reconnects.

Core `FsService` executes host-local filesystem primitives on the machine that
owns that core instance. The Rust client obtains a unified selected-host
interface through its private async `SessionBackend` trait. `Session` delegates
the same `resolve_path`, `list_dir`, `walk_dir`, `read_file`, and `close`
primitives through that trait, regardless of its target. `read_file` is not exposed as a
caller-facing native read operation; it supports the Session open-file
workflow. `FsService` implements the local backend with native core calls;
`RemoteBackend` is constructed with an active remote connection and implements
the same primitives with gRPC. Session retains the selected implementation in an
`RwLock<Option<Box<dyn SessionBackend>>>`: a present backend is open, and a
consumed backend is closing or closed. Backend shutdown consumes the selected
adapter, so no separate lifecycle state is required.

Native Session workflows are grouped by filesystem domain: path operations live
under `session/path`, while directory operations and transport-neutral directory
result types live under `session/directory`. File opening lives under
`session/file`: Session resolves and starts a selected-host read, then releases
the backend read lock before passing the owned path and stream to the private
client `FileService`. `FileService` never accesses a backend. It uses the
resolved path directly for local Sessions or atomically stages the owned stream
for remote Sessions, then invokes the client operating-system opener. Releasing
the lock before entering `FileService` lets Session shutdown cancel an active
remote stream without waiting for the whole copy. UniFFI adapters remain
separate from those native workflows under `session/ffi`.

```text
Session::op_open_file
    -> SessionBackend::resolve_path
    -> SessionBackend::read_file
    -> release backend read lock
    -> FileService
         |-- local: use resolved path
         `-- remote: FileStager::stage_download
         -> FileOpener
```

`FileService` is a client-host workflow facade, not another filesystem
backend and not a replacement for core `FsService`. It owns client-host staging
and operating-system integration only after Session has completed the
selected-host phase.

See [Open File](operations/open-file.md) for the detailed local-validation,
remote-staging, cache-location, failure and retention contract.

`SessionBackend` is a private capability seam, not a required one-to-one model
of public Session workflows. The remote adapter's operation modules are private
gRPC request, response, stream, and error-conversion details. Recursive
traversal is the distinct `walk_dir` capability with dedicated entry and stream
types, rather than a `recursive: bool` mode on `list_dir`. An options type
should be added only when traversal has a real policy choice to express.

![SessionBackend flow chart](assets/session-backend-flow.svg)

The caller sees the same operation and result types on both branches. Backend
selection changes where the core executes, not the public `Session` interface.

The core lists one directory level with `tokio::fs::read_dir` and yields each
classified entry from a pull-based stream. Enumeration advances only when the
consumer polls for another entry, so stream polling provides backpressure
without a producer task or channel. The gRPC server alone applies wire batching,
and the remote client flattens those transport batches before exposing them to
callers. A non-UTF-8 entry terminates the listing with an invalid-argument
error. The exact transport limits are owned by
[Remote Protocol](protocol.md).

Local listing success is normal native stream completion; remote listing success
is normal gRPC stream completion. A terminal local error or gRPC status may
follow valid entries, allowing UIs to retain partial results and show an
incomplete state. Neither core nor the server sorts because global sorting would
require collecting the entire directory.

The core walks trees using an explicit depth-first stack with one open
directory frame per active depth. It emits each directory before opening its
descendants, excludes the requested root, and assigns depth 1 to direct
children. Traversal is pull-based, pre-order, and unsorted. Symlinks and special
files are emitted, but only actual directories are descended into, so directory
symlinks are never followed.

Local walks execute in client-host `FsService::walk_dir`. Remote walks use one
streaming Walk RPC and execute in server-host `FsService::walk_dir`; the client
does not recursively issue List RPCs. The server applies the protocol's Walk
batch limits. Valid entries remain visible before terminal filesystem,
cancellation, transport, or malformed-protocol errors. The returned owned stream
does not retain the Session backend lock, so Session close can cancel an active
traversal.

Core file reads expose a demand-driven `ReadStream<Bytes>` without loading the
complete file. A read validates that the opened handle is a regular file before
returning the stream. Successful core chunks are ordered and non-empty, but have
no promised maximum size or semantically stable boundaries; concatenating them
reconstructs the file from byte zero. The local backend maps core stream errors
directly. The gRPC server splits core items into protocol-bounded `ReadChunk`
messages, and the remote backend maps those messages back into the same client
byte-stream shape without an intermediate reader. Dropping a stream releases or
cancels only that read.

All clones of one `FsService` share a terminal cancellation state. Cancellation
is idempotent, rejects every later operation, terminates active listings and
walks with a `Cancelled` error, and terminates active read streams with one
error item. Service cancellation is never reported as successful stream
completion or file EOF.

See [Session Lifecycle](session-lifecycle.md) for startup, reconnection and
shutdown behavior.

## Client

The client owns backend and transport concerns:

- Native-local lifecycle and remote SSH startup, installation, and shutdown.
- Sensitive bearer-token metadata and standard gRPC health checks.
- One reconnectable tonic channel per remote Session.
- One gRPC response stream per active remote listing, walk, or file read.
- Mapping wire errors to public typed errors.
- Cancelling listing, walk, and read operations when their returned handles are
  dropped.

The UI-facing listing flow is:

```text
Session.op_resolve_path(path) -> absolute lexical selected-host path
Session.op_resolve_path_uniffi(path) -> UniFFI error-mapping adapter
Session.op_list_dir(path) -> EntryStream
EntryStream.try_next() -> Some(entry)
EntryStream.try_next() -> Some(entry)
EntryStream.try_next() -> None or error
```

The client deliberately has no browsing `State`, snapshot, cache, or tree. A
native EntryStream retains only the active transport stream and does not retain
entries already returned to the caller.

## UI display ownership

SwiftUI and the TUI consume the same Session API but own their display and task
state independently. Both append entries in selected-host stream order, retain
partial results after a terminal error, and keep the Session alive after an
individual listing completes.

See [Client and UI State Ownership](state-ownership.md) for the cross-frontend
ownership matrix and [TUI Implementation](tui.md) for concrete event routing,
navigation, refresh and shutdown behavior.
