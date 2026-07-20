# File Peeker v1 Architecture

File Peeker is a read-only browser whose filesystem work runs in a dedicated
server process. A shared Rust client exposes the same typed asynchronous API to
the native Ratatui UI and to SwiftUI through UniFFI.

```text
User -> Ratatui ─┐
                 ├-> Rust client -> authenticated TCP operations -> server -> filesystem
User -> SwiftUI ─┘
```

## Server and transport

Each `Session` owns one local or remote server lifecycle. Launcher stdin defines
that lifecycle; every filesystem operation uses a separate authenticated TCP
stream. Remote streams traverse one OpenSSH SOCKS transport. Independent
operations therefore require neither request IDs nor application multiplexing.

The shared protocol I/O layer reads and writes newline-delimited request and
response frames. Clients retain responses until completion.

The server lists one directory level with `tokio::fs::read_dir`. It accumulates
one bounded batch, writes it, then resumes enumeration. This makes transport
backpressure bound server memory while still reducing time to first visible
entry and avoiding one message per file.

Listing success is an explicit `list_end`; EOF is failure. Errors may follow
valid batches, allowing UIs to retain partial results and show an incomplete
state. The server never sorts because global sorting would require collecting
the entire directory.

## Client

The client owns all transport concerns:

- Local and SSH startup, installation, and shutdown.
- Protocol v2 token handshakes and newline-delimited JSON parsing.
- Heartbeat health checks and fatal-session state.
- One persistent buffered reader per streamed `Listing`.
- Mapping wire errors to public typed errors.
- Validating names and reconstructing full child paths.
- Cancelling an operation when its `Listing` is dropped.

The UI-facing listing flow is:

```text
Session.list(path) -> Listing
Listing.next_batch() -> Some(entries)
Listing.next_batch() -> Some(entries)
Listing.next_batch() -> None or error
```

The client deliberately has no browsing `State`, snapshot, cache, or tree. It
does not retain entries already returned to the caller.

## UI display ownership

Both UIs maintain local display rows containing entry, parent, depth,
expanded/loading state, and an optional error.

Root navigation clears the previous tree immediately and consumes a new
listing. Expansion marks the parent open immediately and inserts child batches
as they arrive. Collapse cancels that branch's listing and removes every
descendant. Generation tokens reject late events after navigation or collapse.

Selection is keyed by full path. Duplicate paths replace existing display
entries. Swift applies its selected name/kind sort; Ratatui retains arrival
order. Neither UI adds a separate batch-coalescing layer, although Ratatui's
normal draw interval may render several already-applied events together.

If a root listing fails, received entries remain visible and the global status
shows the error. If an expansion fails, its partial children remain under an
expanded parent carrying an error marker. Retrying starts a fresh listing.

The SSH connection sheet dismisses after session setup and current-root
discovery; the remote root then streams in the main browser.
