# File Peeker API

The public application API is the Rust `file-peeker-client` crate, exported to
Swift through UniFFI. The Rust API exposes directory listings as streams; the
Swift API exposes the same server batches through `Listing.nextBatch()`.

## Public interface

```rust
pub type ListStream =
    BoxStream<'static, io::Result<Vec<DirectoryEntry>>>;

impl Client {
    pub fn new() -> Arc<Client>;

    pub async fn start_session(
        &self,
        config: SessionConfig,
    ) -> Result<String, ConnectError>;

    pub async fn get_session(&self, id: String) -> Option<Arc<Session>>;
    pub async fn close_session(&self, id: String) -> Result<(), CloseSessionError>;
}

impl Session {
    pub fn id(&self) -> String;
    pub fn target(&self) -> SessionTarget;
    pub async fn op_current_root(&self) -> io::Result<String>;
    pub async fn op_current_root_uniffi(&self) -> Result<String, CurrentRootError>;
    pub async fn op_list(&self, path: &str) -> io::Result<ListStream>;
    pub async fn op_list_uniffi(
        &self,
        path: String,
    ) -> Result<Arc<Listing>, ListError>;
    pub async fn close(&self) -> Result<(), CloseError>;
}

impl Listing {
    pub async fn next_batch(&self) -> Result<Option<Vec<DirectoryEntry>>, ListError>;
}

pub struct SessionConfig {
    pub target: SessionTarget,
}

pub enum SessionTarget {
    Local,
    Remote { destination: String },
}

pub enum ConnectError {
    ServerStart { message: String },
}

pub enum CloseError {
    ServerShutdown { message: String },
}

pub enum CloseSessionError {
    NotFound { id: String },
    ServerShutdown { message: String },
}

pub struct DirectoryEntry {
    pub name: String,
    pub kind: EntryKind,
    pub navigable: bool,
}

pub enum EntryKind { File, Directory, Symlink, Other }
```

`Client.start_session` is asynchronous. A local target reuses or installs the
matching server below `~/.file-peeker/servers/VERSION`, then starts it directly.
A remote target provisions and starts the matching server over SSH. Both paths
authenticate a gRPC health check and return a UUID only when startup
succeeds. The Client strongly retains the Session, which owns the server process
and, for remote targets, the SSH transport. `Client.close_session` removes and
gracefully closes the retained Session. Direct Rust `Session.close()` and Swift
`Session.closeUniffi()` remain idempotent but do not unregister it. Dropping Client releases all retained
sessions; unclosed connections use their non-blocking fallback cleanup.

`Session.op_list` is the native Rust API. Its `ListStream` yields
`Vec<DirectoryEntry>` batches from the server-streaming RPC in order. Batching
is the default list semantic; there is no flattened native listing API.
Dropping the stream cancels unfinished work.

`Session.op_list_uniffi` wraps that native stream for Swift. Its async
`nextBatch()` method returns one native batch. Completion is idempotent, and
terminal stream errors are repeated consistently.

## Swift usage

UniFFI exports `start_session` as `startSession` and the targets as `.local`
and `.remote`:

```swift
let client = Client()
let sessionID = try await client.startSession(
    config: SessionConfig(
        target: .remote(destination: "example.test")
    )
)
guard let session = await client.getSession(id: sessionID) else {
    fatalError("started session was not retained")
}

let localSessionID = try await client.startSession(
    config: SessionConfig(target: .local)
)
guard let localSession = await client.getSession(id: localSessionID) else {
    fatalError("started session was not retained")
}

let listing = try await localSession.opListUniffi(path: "/tmp")
while let batch = try await listing.nextBatch() {
    for entry in batch {
        print(entry.name)
    }
}

try await client.closeSession(id: localSessionID)
```

The SwiftUI application starts a local Session when its browser appears, uses
`opCurrentRootUniffi()` as Home, and consumes listing batches incrementally.
Rows are display-only. The retained Session stays alive after listing completes
and closes when the browser disappears.
