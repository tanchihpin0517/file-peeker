# File Peeker API

The public application API is the Rust `file-peeker-client` crate, exported to
Swift through UniFFI. UIs never encode protocol messages or open sockets.

## Component boundary

```text
Ratatui UI ─┐
            ├─> Session / Listing -> private protocol -> server -> filesystem
SwiftUI UI ─┘
```

The client owns connection startup, SSH provisioning, path normalization,
handshakes, framing, protocol validation, and conversion to typed entries. Each
UI owns only the tree it currently displays: sorting, expansion, selection,
loading indicators, and partial errors.

## Public objects

```rust
impl Client {
    pub fn new() -> Arc<Client>;
    pub async fn connect(
        &self,
        config: SessionConfig,
    ) -> Result<Arc<Session>, FilePeekerError>;
}

impl Session {
    pub fn target(&self) -> SessionTarget;
    pub async fn list(
        &self,
        path: String,
    ) -> Result<Arc<Listing>, FilePeekerError>;
    pub async fn current_root(&self) -> Result<String, FilePeekerError>;
    pub async fn open(&self, path: String) -> Result<(), FilePeekerError>;
    pub async fn metadata(&self, path: String)
        -> Result<FileMetadata, FilePeekerError>;
    pub async fn close(&self) -> Result<(), FilePeekerError>;
}

impl Listing {
    pub async fn next_batch(
        &self,
    ) -> Result<Option<Vec<DirectoryEntry>>, FilePeekerError>;
}
```

`Session.list` validates and normalizes the path, opens an operation connection,
completes its handshake, sends `list`, and returns before listing results arrive.
Relative UTF-8 paths are resolved against the client process's current working
directory.

`Listing.next_batch` returns each non-empty typed batch, then `None` after
explicit successful completion. Server, connection, framing, and protocol
errors are returned as `FilePeekerError`. Completion is idempotent; a failed
listing repeats its terminal error. Concurrent calls on one listing are invalid.

A listing retains its session so the server remains alive while results are
being consumed. Dropping the listing closes its operation socket and cancels
server work.

## Values and errors

```rust
pub struct DirectoryEntry {
    pub path: String,
    pub name: String,
    pub kind: EntryKind,
    pub navigable: bool,
}

pub enum EntryKind { File, Directory, Symlink, Other }
```

`DirectoryEntry.path` is reconstructed and validated by Rust; UIs key identity
and selection by this full path. `name` is for display.

Errors are `NotImplemented`, `InvalidPath`, `ServerStart`, `ServerExited`,
`ConnectionClosed`, `Protocol`, and `Io`. A listing error does not contain or
discard previously returned batches; the UI decides how to present its partial
display state.

## Swift usage

```swift
let session = try await Client().connect(config: config)
let listing = try await session.list(path: path)

while let batch = try await listing.nextBatch() {
    displayState.apply(batch)
}
```

The SwiftUI model runs this loop in a task on its main-actor model. Ratatui runs
the same pull loop in a Tokio task and forwards batches to its application event
loop.
