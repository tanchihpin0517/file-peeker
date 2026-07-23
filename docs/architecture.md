# File Peeker v1 Architecture

File Peeker is a read-only browser whose filesystem behavior lives in a reusable
Rust core. A shared client exposes the same typed asynchronous API to Ratatui
and to SwiftUI through UniFFI.

See [Code Structure](code-structure.md) for the current module plan, operation
placement rules, and the expected structure for future features.

```text
User -> Ratatui ─┐                  ┌-> native core -> app host filesystem
                 ├-> Rust client --┤
User -> SwiftUI ─┘                  └-> SSH/gRPC -> server -> remote core -> remote host filesystem
```

Both paths use the same core filesystem implementation. “Local” is relative to
the process running that core: a native Session accesses the filesystem of the
machine running the client application, while a remote Session asks the server's
core to access the filesystem of the remote machine. Filesystem data itself is
not redirected through a client-side virtual filesystem.

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

The Rust client obtains a unified backend interface through its private async
`SessionBackend` trait. `Session` always delegates the same `resolve_path`,
`list_dir`, and `close` operations through that trait, regardless of its target.
The trait also provides a backend-only `read_file` operation that is not yet exposed
through `Session` or UniFFI.
`FsService` implements the interface with native core calls; `RemoteBackend` is
constructed with an active remote connection and implements it with gRPC.
Session retains the selected implementation in an
`RwLock<Option<Box<dyn SessionBackend>>>`: a present backend is open, and a
consumed backend is closing or closed. Backend shutdown consumes the selected
adapter, so no separate lifecycle state is required.

Native Session workflows are grouped by filesystem domain: path operations live
under `session/path`, while directory operations and transport-neutral directory
result types live under `session/directory`. UniFFI adapters are separate from
those native types under `session/ffi`. The backend-only read stream remains at
the backend seam until a native Session file operation exposes it.

`SessionBackend` is a private capability seam, not a required one-to-one model
of public Session workflows. The remote adapter's operation modules are private
gRPC request, response, stream, and error-conversion details. Future recursive
traversal should be a distinct `walk_dir` capability with dedicated entry and
stream types, rather than a `recursive: bool` mode on `list_dir`. An options
type should be added only when traversal has a real policy choice to express.

![SessionBackend flow chart](assets/session-backend-flow.svg)

The caller sees the same operation and result types on both branches. Backend
selection changes where the core executes, not the public `Session` interface.

The core lists one directory level with `tokio::fs::read_dir` and yields each
classified entry from a pull-based stream. Enumeration advances only when the
consumer polls for another entry, so stream polling provides backpressure
without a producer task or channel. The gRPC server alone buffers up to 1024
entries and splits encoded protobuf messages at 1 MiB. The remote client
flattens those transport batches before exposing them to callers. A non-UTF-8
entry terminates the listing with an invalid-argument error.

Listing success is normal gRPC stream completion. A terminal gRPC status may
follow valid entries, allowing UIs to retain partial results and show an
incomplete state. The server never sorts because global sorting would require
collecting the entire directory.

Core file reads expose a demand-driven `ReadStream<Bytes>` without loading the
complete file. A read validates that the opened handle is a regular file before
returning the stream. Successful core chunks are ordered and non-empty, but have
no promised maximum size or semantically stable boundaries; concatenating them
reconstructs the file from byte zero. The local backend maps core stream errors
directly. The gRPC server splits core items into `ReadChunk` messages of at most
64 KiB, and the remote backend maps those messages back into the same client
byte-stream shape without an intermediate reader. Dropping a stream releases or
cancels only that read.

All clones of one `FsService` share a terminal cancellation state. Cancellation
is idempotent, rejects every later operation, terminates active listings with a
`Cancelled` error, and terminates active read streams with one error item.
Service cancellation is never reported as successful listing completion or
file EOF.

## Client

The client owns backend and transport concerns:

- Native-local lifecycle and remote SSH startup, installation, and shutdown.
- Sensitive bearer-token metadata and standard gRPC health checks.
- One reconnectable tonic channel per Session.
- One gRPC response stream per active listing or remote file read.
- Mapping wire errors to public typed errors.
- Cancelling listing and read operations when their returned handles are dropped.

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

SwiftUI starts one local Session and discovers its current root. The TUI always
creates its App and Client, but starts a local Session only when invoked with a
path; without one, the shared Ratatui loop displays an in-app help screen. The
TUI resolves its startup path through the selected Session before creating its
BrowserContext. Both UIs append received entries to a flat list in arrival
order and do not add a separate coalescing layer. The TUI keeps a selection and
mutable path per BrowserContext: `l` enters the selected navigable entry, `h`
changes to the lexical parent, and `R` refreshes the current path. Every
replacement listing uses a new generation so delayed events from the cancelled
stream cannot repopulate the cleared list.

The TUI owns one Client plus a map of `BrowserContext` values. Each context has
an independent UUID and retains its resolved Session, current path, accumulated
entries, Listing Status, generation, and listing task. Multiple contexts may
share one Client-owned Session and list concurrently. A context owns refresh,
cancellation, stale-event rejection, partial results, and terminal selection.
Bounded context events carry listing results to the UI loop; App routes them by
context UUID without interpreting listing transitions. Rendering, navigation,
and `R` refresh target only the active context. `main` only coordinates the
terminal and event loop.

If listing fails, entries received earlier remain visible and the global status
shows the terminal error. Listing completion or failure does not close the
Session; the UI retains it until the window closes or the TUI exits.

See [TUI Implementation](tui.md) for the concrete ownership, event-routing,
refresh, and shutdown design.
