# File Peeker API

The public application API is the Rust `file-peeker-client` crate, exported to
Swift through UniFFI. Both APIs expose directory listings one entry at a time.

## Public interface

The table is a caller-oriented inventory rather than a copy of Rust source.
Rustdoc remains authoritative for exact signatures.

| Type | Method | Availability | Result |
| --- | --- | --- | --- |
| `Client` | `new` | Rust and Swift | Strongly referenced `Client` |
| `Client` | `start_session` | Rust and Swift | Retained Session UUID |
| `Client` | `get_session` | Rust and Swift | Retained `Session`, when found |
| `Client` | `close_session` | Rust and Swift | Removes and closes a retained Session |
| `Session` | `id` | Rust and Swift | Immutable Session UUID |
| `Session` | `target` | Rust and Swift | Immutable `SessionTarget` |
| `Session` | `op_resolve_path` | Rust | Absolute lexical selected-host path |
| `Session` | `op_resolve_path_uniffi` | Swift adapter | Absolute lexical selected-host path |
| `Session` | `op_list_dir` | Rust | Native `EntryStream` |
| `Session` | `op_list_dir_uniffi` | Swift adapter | `Listing` object |
| `Session` | `op_walk_dir` | Rust only | Native `WalkStream` |
| `Session` | `op_open_file` | Rust | Opens a selected-host regular file |
| `Session` | `op_open_file_uniffi` | Swift adapter | Opens a selected-host regular file |
| `Session` | `close` | Rust | Idempotent Session shutdown |
| `Session` | `close_uniffi` | Swift adapter | Idempotent Session shutdown |
| `Listing` | `next_entry` | Rust and Swift | Next `DirectoryEntry`, completion, or sticky error |

The client re-exports core `EntryKind`, `DirectoryEntry`, and `WalkEntry` types.
`EntryStream` and `WalkStream` are native boxed streams. UniFFI exposes
`SessionTarget`, `SessionStartError`, `SessionShutdownError`,
`ClientCloseSessionError`, `ResolvePathError`, `ListError`, and `OpenFileError`
at the corresponding Swift boundary.

`Client.start_session` is asynchronous. A local target constructs an in-process
filesystem service without installing or starting a server. A remote target
provisions and starts the matching server over SSH, then authenticates a gRPC
health check. In both cases the core uses native filesystem APIs on its own
host: the client host for a local Session and the server host for a remote
Session. The Client strongly retains the Session, which owns either the native
service or the remote process and SSH transport. `Client.close_session` removes
and gracefully closes the retained Session. Direct Rust
`Session.close()` and Swift `Session.closeUniffi()` remain idempotent but do not
unregister it. Dropping Client releases all retained sessions; unclosed remote
connections use their non-blocking fallback cleanup.

The Rust client uses its private `SessionBackend` trait as the unified primitive
interface for both targets. `Session` invokes the same `resolve_path`,
`list_dir`, `walk_dir`, `read_file`, and `close` methods in either mode. Host-local
filesystem behavior is implemented by core `FsService`; selected-host
dispatch uses `SessionBackend`, with native execution locally and authenticated
gRPC remotely. Public callers therefore use one `Session` API without selecting
a transport-specific operation surface.

The private backend `read_file` operation supports `Session.op_open_file` but is
not exposed as a caller-facing byte stream. Both implementations return a demand-driven
`BoxStream<io::Result<Bytes>>`: successful items are ordered and non-empty, but
their sizes and boundaries are not part of the interface. The local adapter maps
core errors into `io::Error`; the remote adapter maps transport-bounded
`ReadChunk` messages directly into `Bytes`. Open and regular-file validation
failures occur before a stream is returned, while later filesystem, protocol, or
cancellation failures are one terminal stream item.

`Session.op_open_file` validates a regular file on the selected host. Local
files are opened at their existing resolved path. Remote files are fully and
atomically staged on the client host before the operating system is asked to
open them. Its UniFFI equivalent is `opOpenFileUniffi`, which reports
`OpenFileError`. The private client `FileService` implements the client-host
preparation and operating-system phase behind this Session API; it is not
caller-facing, does not replace `FsService`, and never accesses
`SessionBackend`. See [Open File](operations/open-file.md) for cache location,
failure handling and retention.

`Session.op_resolve_path` expands `~` and environment variables using the
selected host's environment, makes the result absolute, and lexically removes
`.` and `..`. It does not require the target to exist or resolve symbolic links.
Resolving an already resolved path returns the same path. The UniFFI adapter
provides the same operation through `opResolvePathUniffi`.

`Session.op_list_dir` is the native Rust API. Its `EntryStream` yields shared-core
`DirectoryEntry` values in order. The remote server batches entries only for
gRPC transport, and the remote client flattens those messages back into the same
entry stream. The client does not define a second listing model. Dropping the
stream cancels unfinished work.

`Session.op_list_dir_uniffi` wraps that native stream for Swift. Its async
`nextEntry()` method returns one entry. Completion is idempotent, and
terminal stream errors are repeated consistently.

`Session.op_walk_dir` is a native Rust-only recursive traversal API. It returns
a pull-based `WalkStream` in pre-order depth-first order without collecting or
sorting the tree. The requested root is excluded, direct children have depth
1, and each `WalkEntry` includes an opaque selected-host relative path plus its
final-component `DirectoryEntry`. Symlinks are emitted but never followed.
Earlier entries remain valid when a descendant, cancellation, transport, or
protocol error terminates the stream. Local walks execute in client core;
remote walks execute once in server core through the streaming Walk RPC.
`list_dir` remains the separate one-level operation. Walk has no UniFFI, Swift,
or TUI surface.

## Swift usage

UniFFI exports `start_session` as `startSession` and the targets as `.local`
and `.remote`:

```swift
let client = Client()
let sessionID = try await client.startSession(
    target: .remote(destination: "example.test")
)
guard let session = await client.getSession(id: sessionID) else {
    fatalError("started session was not retained")
}

let localSessionID = try await client.startSession(target: .local)
guard let localSession = await client.getSession(id: localSessionID) else {
    fatalError("started session was not retained")
}

let listing = try await localSession.opListDirUniffi(path: "/tmp")
while let entry = try await listing.nextEntry() {
    print(entry.name)
}

try await client.closeSession(id: localSessionID)
```

The SwiftUI application starts a local Session when its browser appears,
resolves `"~"` through `opResolvePathUniffi()` as Home, and consumes listing
entries incrementally. Rows are display-only. The retained Session stays alive
after listing completes and closes when the browser disappears.
