# File Peeker API

The public application API is the Rust `file-peeker-client` crate, exported to
Swift through UniFFI. Both APIs expose directory listings one entry at a time.

## Public interface

```rust
pub type EntryStream = BoxStream<'static, io::Result<DirectoryEntry>>;

impl Client {
    pub fn new() -> Arc<Client>;

    pub async fn start_session(
        &self,
        target: SessionTarget,
    ) -> Result<String, SessionStartError>;

    pub async fn get_session(&self, id: String) -> Option<Arc<Session>>;
    pub async fn close_session(&self, id: String) -> Result<(), ClientCloseSessionError>;
}

impl Session {
    pub fn id(&self) -> String;
    pub fn target(&self) -> SessionTarget;
    pub async fn op_resolve_path(&self, path: &str) -> io::Result<String>;
    pub async fn op_resolve_path_uniffi(
        &self,
        path: String,
    ) -> Result<String, ResolvePathError>;
    pub async fn op_list_dir(&self, path: &str) -> io::Result<EntryStream>;
    pub async fn op_list_dir_uniffi(
        &self,
        path: String,
    ) -> Result<Arc<Listing>, ListError>;
    pub async fn close(&self) -> Result<(), SessionShutdownError>;
}

impl Listing {
    pub async fn next_entry(&self) -> Result<Option<DirectoryEntry>, ListError>;
}

pub enum SessionTarget {
    Local,
    Remote { destination: String },
}

pub enum SessionStartError {
    Backend { message: String },
}

pub enum SessionShutdownError {
    Backend { message: String },
}

pub enum ClientCloseSessionError {
    NotFound { id: String },
    Backend { message: String },
}

pub enum ResolvePathError {
    Operation { message: String },
}

pub use file_peeker_core::{DirectoryEntry, EntryKind};
```

`Client.start_session` is asynchronous. A local target constructs an in-process
filesystem service without installing or starting a server. A remote target
provisions and starts the matching server over SSH, then authenticates a gRPC
health check. In both cases the core uses native filesystem APIs on the machine
where it runs: the client machine for a local Session and the server machine for
a remote Session. The Client strongly retains the Session, which owns either the
native service or the remote process and SSH transport. `Client.close_session`
removes and gracefully closes the retained Session. Direct Rust
`Session.close()` and Swift `Session.closeUniffi()` remain idempotent but do not
unregister it. Dropping Client releases all retained sessions; unclosed remote
connections use their non-blocking fallback cleanup.

The Rust client uses its private `SessionBackend` trait as the unified operation
interface for both targets. `Session` invokes the same `resolve_path`, `list_dir`,
and `close` methods in either mode. Filesystem operations use `FsService`
locally and authenticated gRPC remotely. Public callers therefore use one
`Session` API without selecting a transport-specific operation surface.

The backend trait also has a `read_file` operation reserved for later `Session`
exposure. Both implementations return a demand-driven
`BoxStream<io::Result<Bytes>>`: successful items are ordered and non-empty, but
their sizes and boundaries are not part of the interface. The local adapter maps
core errors into `io::Error`; the remote adapter maps transport-bounded
`ReadChunk` messages directly into `Bytes`. Open and regular-file validation
failures occur before a stream is returned, while later filesystem, protocol, or
cancellation failures are one terminal stream item.

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
