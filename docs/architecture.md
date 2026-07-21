# File Peeker v1 Architecture

File Peeker is a read-only browser whose filesystem work runs in a dedicated
server process. A shared Rust client exposes the same typed asynchronous API to
the native Ratatui UI and to SwiftUI through UniFFI.

```text
User -> Ratatui ─┐
                 ├-> Rust client -> authenticated gRPC channel -> server -> filesystem
User -> SwiftUI ─┘
```

## Server and transport

`Client` strongly retains its started Sessions by UUID. Each `Session` owns one
local or remote server lifecycle. Launcher stdin defines that lifecycle. One
authenticated tonic HTTP/2 channel multiplexes every filesystem RPC and the
standard health check. Remote channels connect through one OpenSSH SOCKS
transport; tonic opens a replacement SOCKS stream when its channel reconnects.

The server lists one directory level with `tokio::fs::read_dir`. It accumulates
one bounded batch, writes it, then resumes enumeration. This makes transport
backpressure bound server memory while still reducing time to first visible
entry and avoiding one message per file.

Listing success is normal gRPC stream completion. A terminal gRPC status may
follow valid batches, allowing UIs to retain partial results and show an
incomplete state. The server never sorts because global sorting would require
collecting the entire directory.

## Client

The client owns all transport concerns:

- Local and SSH startup, installation, and shutdown.
- Sensitive bearer-token metadata and standard gRPC health checks.
- One reconnectable tonic channel per Session.
- One gRPC response stream per active listing.
- Mapping wire errors to public typed errors.
- Cancelling an operation when its `Listing` is dropped.

The UI-facing listing flow is:

```text
Session.op_current_root() -> absolute server working directory
Session.op_current_root_uniffi() -> UniFFI error-mapping adapter
Session.op_list(path) -> ListStream
ListStream.try_next() -> Some(entry batch)
ListStream.try_next() -> Some(entry batch)
ListStream.try_next() -> None or error
```

The client deliberately has no browsing `State`, snapshot, cache, or tree. A
native ListStream retains only the active transport stream and does not retain
batches already returned to the caller.

## UI display ownership

Both UIs start one local Session, discover its current root, and append each
received batch to a flat, display-only list in arrival order. They do not
navigate, expand directories, open files, or add a separate batch-coalescing
layer. The TUI keeps a visual selection per BrowserContext and can cancel and
restart the active Home listing with `R`; generation IDs prevent delayed events
from the cancelled stream from repopulating the cleared list.

The TUI owns one Client plus a map of `BrowserContext` values. Each context has
an independent UUID, Session UUID, path, accumulated entries, status,
generation, and listing task. Multiple contexts may share one Client-owned
Session and list concurrently. Events carry both context UUID and generation,
while rendering and `R` refresh target only the active context. `main` only
coordinates the terminal and event loop.

If listing fails, entries received from earlier batches remain visible and the
global status shows the terminal error. Listing completion or failure does not
close the Session; the UI retains it until the window closes or the TUI exits.

See [TUI Implementation](tui.md) for the concrete ownership, event-routing,
refresh, and shutdown design.
