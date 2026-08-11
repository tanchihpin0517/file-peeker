# File Peeker API

The application API is the `Client` and `Session` layer of the `file-peeker`
crate, reached as `crate::Client` and `crate::Session`. It exposes directory
listings one entry at a time.

The layer is crate-internal: the two front ends are modules of the same crate,
so nothing is exported beyond the three binary entry points. "Public" below
means the surface the `tui` and `cli` modules consume, not a published API.

## Public interface

The table is a caller-oriented inventory rather than a copy of Rust source.
Rustdoc remains authoritative for exact signatures.

| Type | Method | Result |
| --- | --- | --- |
| `Client` | `new` | Strongly referenced `Client` |
| `Client` | `start_session` | Started and retained `Session` |
| `Client` | `close_session` | Removes and closes one retained Session |
| `Client` | `close_all` | Removes and closes every retained Session |
| `Session` | `id` | Immutable Session UUID |
| `Session` | `target` | Immutable `SessionTarget` |
| `Session` | `op_resolve_path` | Absolute lexical selected-host path |
| `Session` | `op_list_dir` | Native `EntryStream` |
| `Session` | `op_walk_dir` | Native `WalkStream` |
| `Session` | `op_open_file` | Opens a selected-host regular file |
| `Session` | `op_copy_file_from` | Streams a file from another Session |
| `Session` | `close` | Idempotent Session shutdown |

The client re-exports core `EntryKind`, `DirectoryEntry`, and `WalkEntry` types.
`EntryStream` and `WalkStream` are native boxed streams. Operations report
`std::io::Error`; only the Session lifecycle uses typed errors, namely
`SessionStartError` and `SessionShutdownError`.

`Client.start_session` is asynchronous. A local target constructs an in-process
filesystem service without installing or starting a server. A remote target
provisions and starts the matching server over SSH, then authenticates a gRPC
health check. In both cases the core uses native filesystem APIs on its own
host: the client host for a local Session and the server host for a remote
Session. The Client strongly retains the returned Session, which owns either the
native service or the remote process and SSH transport, so a caller that drops
its handle without closing still leaves shutdown to the Client.
`Client.close_session` removes and gracefully closes one retained Session, and
`Client.close_all` drains the registry. Both succeed for unknown or already
closed IDs. Direct `Session.close()` remains idempotent but does not unregister
the Session. Dropping Client releases all retained sessions; unclosed remote
connections use their non-blocking fallback cleanup.

The Rust client uses its private `SessionBackend` trait as the unified primitive
interface for both targets. `Session` borrows the open backend through one
guarded accessor and invokes the same `resolve_path`, `list_dir`, `walk_dir`,
`read_file`, `write_file`, and `close` methods in either mode. Host-local
filesystem behavior is implemented by core `FsService`; selected-host dispatch
uses `SessionBackend`, with native execution locally and authenticated gRPC
remotely. Public callers therefore use one `Session` API without selecting a
transport-specific operation surface.

The private backend `read_file` operation supports `Session.op_open_file` but is
not exposed as a caller-facing byte stream. Both implementations return a demand-driven
`BoxStream<io::Result<Bytes>>`: successful items are ordered and non-empty, but
their sizes and boundaries are not part of the interface. The local adapter maps
core errors into `io::Error`; the remote adapter maps transport-bounded
`ReadChunk` messages directly into `Bytes`. Open and regular-file validation
failures occur before a stream is returned, while later filesystem, protocol, or
cancellation failures are one terminal stream item.

`Session::op_copy_file_from` is a destination-oriented operation. It accepts a
source Session/path, a destination directory and separate single-component
filename, plus `CopyConflict::{FailIfExists, Replace}`. It returns `CopyResult`
with the selected-host destination path and copied byte count. The source and
destination backend locks are released before streaming starts. See
[Copy File](operations/copy-file.md) for its routing and atomicity contract.

`Session.op_open_file` validates a regular file on the selected host. Local
files are opened at their existing resolved path. Remote files are fully and
atomically staged on the client host before the operating system is asked to
open them. The private client `FileService` implements the client-host
preparation and operating-system phase behind this Session API; it is not
caller-facing, does not replace `FsService`, and never accesses
`SessionBackend`. Failures it reports carry the operation context, such as
``cannot open file `/path`: ...``. See [Open File](operations/open-file.md) for
cache location, failure handling and retention.

`Session.op_resolve_path` expands `~` and environment variables using the
selected host's environment, makes the result absolute, and lexically removes
`.` and `..`. It does not require the target to exist or resolve symbolic links.
Resolving an already resolved path returns the same path.

`Session.op_list_dir` yields shared-core `DirectoryEntry` values in order. The
remote server batches entries only for gRPC transport, and the remote client
flattens those messages back into the same entry stream. The client does not
define a second listing model. Dropping the stream cancels unfinished work.

`Session.op_walk_dir` is a recursive traversal API. It returns a pull-based
`WalkStream` in pre-order depth-first order without collecting or sorting the
tree. The requested root is excluded, direct children have depth 1, and each
`WalkEntry` includes an opaque selected-host relative path plus its
final-component `DirectoryEntry`. Symlinks are emitted but never followed.
Earlier entries remain valid when a descendant, cancellation, transport, or
protocol error terminates the stream. Local walks execute in the client's `fs` layer;
remote walks execute once in the server's `fs` layer through the streaming Walk
RPC.
`list_dir` remains the separate one-level operation. Walk has no TUI surface.

## Testing against a Session

`test_support` provides fixtures for driving Session workflows without a real
filesystem, remote host, or operating-system opener: `FakeBackend` replays
scripted listings and records requests, `RecordingOpener` records open requests
and replays scripted failures, and `Session::for_test` and
`Session::closed_for_test` assemble a Session from them.

The module is compiled under `#[cfg(test)]`, so it cannot reach a release build
and every test module in the crate — including the TUI's — gets the fixtures
without a feature flag. Nothing outside the crate can test against a scripted
Session; the fixtures are internal scaffolding, not a supported surface.
