# File Peeker API

This document describes the implemented interfaces between File Peeker's UI,
shared client, and server. File Peeker does not expose an HTTP API. Its public
application API is the Rust client library (also exported to Swift through
UniFFI); its server protocol is a private, versioned NDJSON protocol over Unix
domain sockets.

## Component boundaries

```mermaid
flowchart LR
    User[User]
    TUI[Ratatui UI]
    SwiftUI[SwiftUI app]
    Model[BrowserModel]
    Bindings[UniFFI Swift bindings]
    Client[Session\nRust client library]
    Control[Control socket]
    Operation[Operation socket]
    Server[file-peeker-server]
    Filesystem[Filesystem]

    User --> TUI
    User --> SwiftUI
    SwiftUI --> Model
    Model --> Bindings
    Bindings --> Client
    TUI --> Client
    Client --> Control
    Client --> Operation
    Control --> Server
    Operation --> Server
    Server --> Filesystem
```

| Component | Responsibility | Communicates with |
| --- | --- | --- |
| UI | Presents entries, accepts navigation, and displays loading or error state | Public client API only |
| Session | Owns one connection and server lifecycle, normalizes paths, performs handshakes, maps protocol data to UI-safe types, and supervises shutdown | UI and `State` objects through Rust/UniFFI; server through private sockets |
| State | Owns the browsing status for one fixed root path, including its expandable visible tree | UI and its owning `Session` |
| Server | Reads the filesystem and streams results | Client through the private wire protocol; local filesystem through OS APIs |

The UI never opens a socket or encodes protocol messages. The server never
contains rendering or navigation state. One `Session` owns one dedicated
server lifecycle and may support multiple independent `State` objects.

## Public client API

The `file-peeker-client` crate is the supported API for a UI. The same objects,
records, enums, errors, and asynchronous methods are exported through UniFFI.

API outline:

- `Client`
  - `new` creates the shared API entry point.
  - `connect` creates an independent local or SSH `Session`.
- `Session`
  - `target` and `current_root` describe the connection.
  - `start_listing` creates a pull-based `DirectoryListing`.
  - `open_state` creates an independent browsing `State`.
  - `open` and `metadata` operate on paths.
  - `close` shuts down the owned connection lifecycle.
- `State`
  - `snapshot` returns the current flattened rows.
  - `expand` freshly loads a visible directory.
  - `collapse` discards a directory's visible descendants.
- `DirectoryListing`
  - `next_entry` returns the next streamed directory entry.
- Shared values
  - `SessionConfig` and `SessionTarget` configure connections.
  - `DirectoryEntry`, `FileMetadata`, `StateRow`, and `StateSnapshot` carry results.
  - `EntryKind` identifies filesystem entry types.
  - `FilePeekerError` reports typed failures across Rust and UniFFI.

### Configuration

```rust
pub struct SessionConfig {
    pub target: SessionTarget,
}

pub enum SessionTarget {
    Local { server_executable_path: String },
    Ssh { destination: String },
}
```

- `Local` starts the executable at `server_executable_path` on the current
  machine.
- `Ssh` uses the named SSH destination, provisions a compatible remote server
  when necessary, and forwards a local Unix socket to the remote server.
- For both targets, the returned client has the same API and protocol behavior.

### Client and Session

```rust
impl Client {
    pub fn new() -> Arc<Client>;

    pub async fn connect(
        &self,
        config: SessionConfig,
    ) -> Result<Arc<Session>, FilePeekerError>;
}

impl Session {
    pub async fn start_listing(
        &self,
        path: String,
    ) -> Result<Arc<DirectoryListing>, FilePeekerError>;

    pub async fn open_state(
        &self,
        path: String,
    ) -> Result<Arc<State>, FilePeekerError>;

    pub fn target(&self) -> SessionTarget;

    pub async fn current_root(&self) -> Result<String, FilePeekerError>;

    pub async fn close(&self) -> Result<(), FilePeekerError>;

    pub async fn open(&self, path: String) -> Result<(), FilePeekerError>;

    pub async fn metadata(
        &self,
        path: String,
    ) -> Result<FileMetadata, FilePeekerError>;
}
```

| Method | Behavior | Status |
| --- | --- | --- |
| `Client.connect` | Starts a local server or SSH transport, opens the control connection, negotiates protocol v1, and returns an owning session | Implemented |
| `start_listing` | Normalizes the supplied path, opens one operation connection, and returns a pull-based listing | Implemented |
| `open_state` | Fully loads one absolute root and returns a new independent browsing state | Implemented |
| `target` | Returns the immutable local or SSH target used to create the session | Implemented |
| `current_root` | Returns the server process's absolute current working directory | Implemented |
| `close` | Closes control, waits for the owned server/SSH process, and cleans up private endpoints | Implemented; idempotent |
| `open` | Opens a path with the macOS default application for local clients; succeeds without action for SSH clients | Implemented |
| `metadata` | Intended to return metadata for one path | Reserved; currently returns `FilePeekerError::NotImplemented` without contacting the server |

`start_listing` accepts absolute or relative UTF-8 paths. Relative paths are
resolved against the client process's current working directory. An empty or
non-UTF-8 path is rejected. The normalized absolute path is sent to the server.

Dropping the last reference to a `Session`, including references held by its
states, initiates shutdown. Calls that begin after the lifecycle has closed
return `ConnectionClosed`. Explicitly closing a session invalidates all of its
states.

### State

```rust
impl State {
    pub fn snapshot(&self) -> StateSnapshot;

    pub async fn expand(
        &self,
        path: String,
    ) -> Result<StateSnapshot, FilePeekerError>;

    pub fn collapse(
        &self,
        path: String,
    ) -> Result<StateSnapshot, FilePeekerError>;
}

pub struct StateSnapshot {
    pub path: String,
    pub rows: Vec<StateRow>,
}
```

A `State` has one fixed, normalized root path and owns the browsing status for
that root. `StateRow` carries the entry, parent path, depth, expansion state,
and optional listing error. Collapsing discards descendants, so every later
expansion performs a fresh listing. Per-directory revisions prevent stale
operations from repopulating a collapsed branch.

A session can create any number of states. Expanding one does not change the
others. Directory navigation creates and fully loads a new state; the UI can
keep displaying its old state until `open_state` succeeds and then swap the
reference atomically. A state retains its session, so it remains usable even if
the pane that originally created the session releases its own reference.

### DirectoryListing

```rust
impl DirectoryListing {
    pub async fn next_entry(
        &self,
    ) -> Result<Option<DirectoryEntry>, FilePeekerError>;
}
```

`DirectoryListing` is a pull-based asynchronous stream:

- `Ok(Some(entry))` returns the next direct child.
- `Ok(None)` means the listing completed successfully. Later calls also return
  `Ok(None)`.
- `Err(error)` means the listing failed. Entries returned before the error are
  still valid.
- Dropping the listing aborts its operation task and closes that operation's
  socket.

The internal queue holds up to 64 results, providing backpressure when a UI
consumes entries more slowly than the server produces them.

### Data types

```rust
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

pub struct DirectoryEntry {
    pub path: String,
    pub name: String,
    pub kind: EntryKind,
    pub navigable: bool,
}

pub struct FileMetadata {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub readonly: bool,
    pub modified: Option<String>,
}
```

`DirectoryEntry.path` is the absolute path used for later operations, while
`name` is the final path component intended for display. `navigable` is true
for directories and for symlinks whose current target is a directory.

`FileMetadata` is exported as part of the reserved metadata API, but no
implemented public operation currently returns it.

### Errors

```rust
pub enum FilePeekerError {
    NotImplemented { operation: String },
    InvalidPath { message: String },
    ServerStart { message: String },
    ServerExited { message: String },
    ConnectionClosed { message: String },
    Protocol { message: String },
    Io { message: String },
}
```

| Error | Meaning |
| --- | --- |
| `NotImplemented` | The public API exists but the operation is not implemented |
| `InvalidPath` | A path is empty, invalid, or cannot be represented as UTF-8 |
| `ServerStart` | The server or SSH transport could not be prepared or started |
| `ServerExited` | The owned process exited unexpectedly |
| `ConnectionClosed` | A required connection closed or an operation was cancelled |
| `Protocol` | Negotiation, framing, JSON, versioning, or message order was invalid |
| `Io` | A local or remote filesystem operation failed |

At the wire boundary, `invalid_path` maps to `InvalidPath`;
`not_found`, `permission_denied`, `not_directory`, and `io` map to `Io`; and
`unsupported_version` maps to `Protocol`.

### Swift API names

UniFFI exposes the same API using Swift naming conventions. The SwiftUI app
currently uses:

```swift
let client = Client()
let session = try await client.connect(
    config: SessionConfig(
        target: .local(serverExecutablePath: serverURL.path)
    )
)

let state = try await session.openState(path: path)
let initial = state.snapshot()
let expanded = try await state.expand(path: "/tmp/example")
let collapsed = try state.collapse(path: "/tmp/example")

let root = try await session.currentRoot()
let target = session.target()
try await session.open(path: "/tmp/example/report.txt")
try await session.close()
```

The metadata call is exposed as `metadata(path:)`, subject to the same
not-implemented behavior as Rust.

## UI integration

The UIs are consumers of the client API; they do not expose a network or
library API of their own.

### SwiftUI

`ContentView` owns a main-actor `BrowserModel`. When the view task starts:

1. `BrowserModel.start()` locates the bundled `file-peeker-server` executable.
2. It calls `Client.connect` with a local target.
3. It calls `openState` for the user's home directory and publishes its snapshot.
4. Disclosure controls call `State.expand` or `State.collapse` and replace that snapshot.
5. Double-clicking an entry or choosing `Open` from its right-click menu opens
   it: navigable entries start a new listing, while other entries call
   `Session.open`.

The model uses a generation counter and cancels the previous Swift task when a
new directory is opened. It leaves the old state visible until the new state is
fully loaded, and ignores results from older generations.
Tree insertion, recursive collapse, depths, errors, and stale-result protection
are maintained by `State`. The model only tracks presentation state such as
loading paths and renders recursively indented rows without changing
`currentPath`. Collapsing removes descendants, so later expansion always reloads.
`ContentView` observes the visible rows, loading paths, and root loading/error
state on the main actor.

### Ratatui

The terminal UI locates a sibling `file-peeker-server`, starts a local
`Session`, and spawns Tokio tasks for state creation, expansion, and file
opening. Those tasks forward complete state-maintained tree snapshots to the
main loop, which owns only presentation state and rendering.

The terminal UI accepts an optional starting path. Arrow keys or `j`/`k` move
the selection, `h`/`l` select a visible parent/child, `o` toggles directory
expansion, Enter navigates into directories or opens non-navigable entries with
`Session.open`, and `q` or Escape exits. Its `--smoke [PATH]` mode
consumes one listing without interactive rendering and is intended for
verification.

## Client-server wire API

The wire API is private to the shared client and dedicated server. External UIs
should not depend on it directly.

### Transport and framing

- Transport: Unix domain stream socket. Remote operation uses SSH Unix-socket
  forwarding; it does not expose a TCP listener.
- Encoding: one UTF-8 JSON object followed by `\n` (NDJSON).
- Protocol version: `1`.
- Maximum message payload: 1 MiB, excluding the newline delimiter.
- Paths: absolute UTF-8 strings.
- One client owns one server and one private, owner-only socket directory.
- There is no authentication token or request ID because the endpoint is
  private and each operation has its own connection.

### Connection model

The first accepted connection must be the long-lived control connection. It
performs a handshake and then carries no more messages. Closing it is the
shutdown signal for the dedicated server.

Every filesystem operation opens a separate connection, performs an operation
handshake, sends exactly one request, receives its responses, and closes.
Multiple operation connections may run concurrently.

```mermaid
sequenceDiagram
    participant UI
    participant API as Client
    participant Client as Session
    participant Server
    participant FS as Filesystem

    UI->>API: connect(config)
    API->>Client: create session
    Client->>Server: launch process / SSH transport
    Client->>Server: hello(version=1, role=control)
    Server-->>Client: hello_ok(version=1)
    Client-->>UI: Session

    UI->>Client: start_listing(path)
    Client->>Server: open operation socket
    Client->>Server: hello(version=1, role=operation)
    Server-->>Client: hello_ok(version=1)
    Client->>Server: list(path)
    Server->>FS: read_dir(path)
    loop each child
        FS-->>Server: directory entry
        Server-->>Client: entry(...)
        Client-->>UI: next_entry() = Some(entry)
    end
    Server-->>Client: done
    Client-->>UI: next_entry() = None

    UI->>Client: close() or drop
    Client--xServer: close control socket
    Server-->>Client: process exits
```

### Handshake messages

Control request:

```json
{"type":"hello","version":1,"role":"control"}
```

Operation request:

```json
{"type":"hello","version":1,"role":"operation"}
```

Success:

```json
{"type":"hello_ok","version":1}
```

Unsupported version:

```json
{"type":"error","code":"unsupported_version","message":"Unsupported protocol version"}
```

No operation request may be sent before `hello_ok`.

### Current-root operation

Request:

```json
{"type":"current_root"}
```

Successful response:

```json
{"type":"current_root","path":"/home/example"}
```

The response is terminal; there is no following `done` message.

### Directory-listing operation

Request:

```json
{"type":"list","path":"/tmp/example"}
```

The server sends zero or more entries in filesystem enumeration order:

```json
{"type":"entry","path":"/tmp/example/docs","name":"docs","kind":"directory","navigable":true}
```

Success terminates with:

```json
{"type":"done"}
```

Failure terminates with an error and may occur after entries have already been
sent:

```json
{"type":"error","code":"permission_denied","message":"Permission denied"}
```

Entry `kind` is `file`, `directory`, `symlink`, or `other`.

### Wire errors

| Code | Meaning |
| --- | --- |
| `not_found` | The path does not exist |
| `permission_denied` | OS permissions rejected the operation |
| `not_directory` | A listing path is not a directory |
| `invalid_path` | The supplied path is invalid |
| `io` | Another filesystem I/O error occurred |
| `unsupported_version` | Protocol negotiation failed |

An operation error is terminal. Malformed JSON, an oversized message, or an
invalid message sequence causes a protocol failure and connection closure.

### Reserved metadata messages

The shared protocol schema declares these messages:

```json
{"type":"get_metadata","path":"/tmp/example/docs"}
```

```json
{"type":"metadata","path":"/tmp/example/docs","kind":"directory","size":96,"readonly":false,"modified":"2026-07-16T12:10:00Z"}
```

They are not operational in the current implementation. The server accepts
only `list` and `current_root` after an operation handshake, while the public
client `metadata` method immediately returns `NotImplemented`.

## Process command-line interfaces

These CLIs are process entry points, separate from the client library API.

### Server

```text
file-peeker-server serve --socket PATH [--remove-parent-on-exit]
file-peeker-server version --format json
file-peeker-server --version
```

- `serve` requires an absolute, unused socket path no longer than 100 bytes.
  The parent must already exist, be a directory, and have no group or other
  permission bits.
- `--remove-parent-on-exit` removes the socket's parent directory after the
  socket is removed. It is used for an owned remote runtime directory.
- `version --format json` writes
  `{"server_version":"<package-version>","protocol_versions":[1]}`.

The server CLI is normally invoked by the client, not by a UI or end user.

### Client diagnostics

```text
file-peeker-client connect SSH_DESTINATION
file-peeker-client install SSH_DESTINATION
file-peeker-client open PATH
```

- `connect` ensures a compatible remote server is installed, starts it through
  SSH, prints its current root to stdout, and closes it.
- `install` overwrites and verifies the versioned remote server installation,
  then prints its remote executable path to stdout.
- `open` starts the sibling local server, opens `PATH` with the macOS default
  application through `Session.open`, and shuts the server down.
- Progress and diagnostics are written to stderr.

### Terminal UI

```text
file-peeker-tui [PATH]
file-peeker-tui --smoke [PATH]
```

The first form opens the interactive browser. The second performs a
non-interactive listing smoke test.

## Stability and extension rules

- UI consumers should depend on the public `file-peeker-client` types, not the
  private wire schema.
- A breaking wire change requires a new protocol version.
- Adding an operation requires coordinated protocol, server, client, UniFFI,
  and UI work.
- The current API is read-only: it exposes no write, delete, rename, upload, or
  download operation.
- The current UI chooses only local targets; SSH is available through the
  public client API and diagnostic CLI but is not selectable in either UI.
