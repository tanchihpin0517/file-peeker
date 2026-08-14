# File Peeker API

The application API is the `Client` layer of the `file-peeker` crate, reached as
`crate::Client`. It exposes directory listings one entry at a time.

The layer is crate-internal: the front ends are modules of the same crate, so
nothing is exported beyond the four binary entry points. "Public" below means the
surface the `tui`, `gui` and `cli` modules consume, not a published API.

## Public interface

The table is a caller-oriented inventory rather than a copy of Rust source.
Rustdoc remains authoritative for exact signatures.

| Type | Method | Result |
| --- | --- | --- |
| `Client` | `new` | Strongly referenced `Client` |
| `Client` | `open` | `ConnectionId` for a connection opened at a `ServerLocation` |
| `Client` | `close` | Removes and closes one retained connection |
| `Client` | `close_all` | Removes and closes every retained connection |
| `Client` | `op_resolve_path` | Absolute lexical selected-host path |
| `Client` | `op_list_dir` | Native `EntryStream` |
| `Client` | `op_walk_dir` | Native `WalkStream` |
| `Client` | `op_copy_file` | Streams a file between two connections |

Every operation names its connection with the `ConnectionId` that `open`
returned. There is no connection handle to hold: an ID is a `Copy` value, and
the Client owns the connection it names.

The client re-exports core `EntryKind`, `DirectoryEntry`, `EntryMetadata`, and
`WalkEntry` types. A `DirectoryEntry` carries its name, kind and navigability, its
`EntryMetadata` when that could be read, and otherwise the reason it could not — which
is a property of that one entry and never ends a listing.

Names and paths are `HostPath`, not `String`. A Unix filename is any byte sequence but
`/` and NUL, so it is held as an invertible escape that is always valid UTF-8; see
[Remote Protocol](protocol.md) for the encoding. The type is what keeps the two forms
apart: `as_escaped` is what addresses a file and crosses the wire, `display` is lossy
and for a person to read, and neither can be passed where the other belongs by
accident.
`EntryStream` and `WalkStream` are native boxed streams. Every method reports
`std::io::Error`, lifecycle included: there is no separate typed error to
translate, so an `ErrorKind` set deep in the transport — `InvalidInput` for an
empty SSH destination, say — reaches the caller intact.

`Client.open` takes a `ServerLocation`, is asynchronous, and starts a server
either way. `Embedded` starts one inside this process on an ephemeral loopback
port. `Ssh` provisions and starts the matching server over SSH, and carries the
`force_install` flag that the CLI's provisioning subcommands set; front ends
that only browse pass `false`. Both then authenticate a
gRPC health check before the ID is returned. The core uses native filesystem
APIs on the host running that server: the client host for a local connection and
the server host for a remote one. The Client retains the connection, which owns
that server and its transport, so a caller that forgets the returned ID still
leaves shutdown to `close_all`. `Client.close` removes and gracefully closes one
retained connection, and `Client.close_all` drains the registry. Both succeed
for unknown or already closed IDs. Dropping Client releases every retained
connection; unclosed remote connections use their non-blocking fallback cleanup.

The Client holds each connection in its own `RwLock<Option<ServerConnection>>`
and borrows it through a guarded accessor that yields the closed-connection
error when the ID is unknown or the slot is empty. The registry lock is held
only long enough to find the connection, so one connection's in-flight request
never delays another's close. Each operation issues its request over that
connection; the server's `FsService` performs the host-local filesystem work.
Public callers therefore use one `Client` API, and so does the client code
underneath it — there is no transport-specific operation surface, and no backend
abstraction, to select.

The private `read_file` operation supports `Client.op_copy_file` but is not
exposed as a caller-facing byte stream. It returns a demand-driven
`BoxStream<io::Result<Bytes>>`: successful items are ordered and non-empty, but
their sizes and boundaries are not part of the interface. The adapter maps
transport-bounded `ReadChunk` messages directly into `Bytes`. Open and
regular-file validation failures occur before a stream is returned, while later
filesystem, protocol, or cancellation failures are one terminal stream item.

`Client::op_copy_file` accepts a source connection and path, a destination
connection, a destination directory and separate single-component filename, plus
`CopyConflict::{FailIfExists, Replace}`. It returns `CopyResult` with the
selected-host destination path and copied byte count. Both connections' locks
are released before streaming starts, so a copy may name one connection twice.
See [Copy File](operations/copy-file.md) for its routing and atomicity contract.

`Client.op_resolve_path` expands `~` and environment variables using the
selected host's environment, makes the result absolute, and lexically removes
`.` and `..`. It does not require the target to exist or resolve symbolic links.
Resolving an already resolved path returns the same path.

`Client.op_list_dir` yields shared-core `DirectoryEntry` values in order. An entry the
selected host could not inspect arrives as a degraded entry rather than as a stream
error, so one such entry does not hide the rest of the directory. The
remote server batches entries only for gRPC transport, and the remote client
flattens those messages back into the same entry stream. The client does not
define a second listing model. Dropping the stream cancels unfinished work.

`Client.op_walk_dir` is a recursive traversal API. It returns a pull-based
`WalkStream` in pre-order depth-first order without collecting or sorting the
tree. The requested root is excluded, direct children have depth 1, and each
`WalkEntry` includes an opaque selected-host relative path plus its
final-component `DirectoryEntry`. Symlinks are emitted but never followed.
Earlier entries remain valid when a descendant, cancellation, transport, or
protocol error terminates the stream. A walk executes once in the selected
host's `fs` layer through the streaming Walk RPC.
`list_dir` remains the separate one-level operation. Walk has no TUI surface.

## Testing against a connection

`test_support` provides fixtures for driving Client workflows without a real
filesystem or remote host. `ScriptedServer` is a real File Peeker server whose
filesystem adapter replays canned listings and records the paths it was asked
for; `ScriptedServer::connect` starts it on a loopback port and returns a live
`ServerConnection`. `Client::with_connection_for_test` registers that connection
and hands back the Client and its ID, and `Client::closed_for_test` registers an
ID whose slot is already empty, as it is after a completed close.

Everything below the canned response is genuine — the listener, the bearer
token, the health check, the channel, and every conversion in `client::rpc` — so
a scripted listing arrives having been batched, flattened and validated exactly
as a production one is.

The module is compiled under `#[cfg(test)]`, so it cannot reach a release build
and every test module in the crate — including the TUI's — gets the fixtures
without a feature flag. Nothing outside the crate can test against a scripted
server; the fixtures are internal scaffolding, not a supported surface.
