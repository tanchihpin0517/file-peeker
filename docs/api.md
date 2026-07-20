# File Peeker API

The public application API is the Rust `file-peeker-client` crate, exported to
Swift through UniFFI. The Rust API exposes directory listings as streams; the
Swift API wraps the same stream in an async `Listing.next()` adapter.

## Public interface

```rust
impl Client {
    pub fn new() -> Arc<Client>;

    pub async fn start_session(
        &self,
        config: SessionConfig,
    ) -> Result<Arc<Session>, ConnectError>;
}

impl Session {
    pub fn target(&self) -> SessionTarget;
    pub async fn op_list(&self, path: &str) -> io::Result<ListStream>;
    pub async fn op_list_uniffi(
        &self,
        path: String,
    ) -> Result<Arc<Listing>, ListError>;
    pub async fn close(&self) -> Result<(), CloseError>;
}

impl Listing {
    pub async fn next(&self) -> Result<Option<DirectoryEntry>, ListError>;
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
authenticate the control connection and return only when startup succeeds. The
returned `Session` owns the server process and, for remote targets, the SSH
transport. `Session.close()` performs an idempotent graceful shutdown. Dropping
an unclosed session uses a non-blocking fallback that closes its lifetime lease
and terminates its owned processes.

`Session.op_list` is the native Rust API. It opens an authenticated operation
connection and returns a `Stream` that yields listing entries in order. The
stream owns its connection, buffers at most the current server batch, and
cancels unfinished work when dropped.

`Session.op_list_uniffi` is the Swift-compatible adapter. It returns a `Listing`
object whose async `next()` method advances the same Rust stream. Completion is
idempotent, and terminal stream errors are repeated consistently.

## Swift usage

UniFFI exports `start_session` as `startSession` and the targets as `.local`
and `.remote`:

```swift
let session = try await Client().startSession(
    config: SessionConfig(
        target: .remote(destination: "example.test")
    )
)

let localSession = try await Client().startSession(
    config: SessionConfig(target: .local)
)

let listing = try await localSession.opListUniffi(path: "/tmp")
while let entry = try await listing.next() {
    print(entry.name)
}

try await localSession.close()
```

The SwiftUI application shell does not start a default session. A destination
selection flow will call this API in a later milestone.
