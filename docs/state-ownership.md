# Client and UI State Ownership

The Rust client owns connection and transport state. Each UI owns only the
state needed to display one listing of the server's current root.

| Information | Rust client library | SwiftUI | TUI |
| --- | --- | --- | --- |
| Session UUID and registry | UUID and strong Session registry stored in `Client` | Retains the returned ID and Session | `App` tracks started IDs; each `BrowserContext` references one ID |
| Server process and connection lifecycle | Stored in the Session connection | Starts on appearance; closes on disappearance | `App.start` creates it; `App.shutdown` closes it after terminal restoration |
| gRPC channel and authentication | Stored in the Session connection | No | No |
| Current-root path | Returned to the caller | `homePath` | Initial `BrowserContext.path` |
| Active listing stream | Native `ListStream` or Swift `Listing` adapter | Consumed by `loadTask` | Each `BrowserContext.listing_task` consumes native `op_list` |
| Received directory entries | Returned batch-by-batch, not retained | Appended to `rows` | Appended to the matching context's `entries` |
| Loading and terminal error | Produced by the operation | `isLoading` and `errorMessage` | Stored independently in each `BrowserContext` |
| Selection | No | Not implemented | `selected_index` stored per `BrowserContext` |
| Navigation, expansion, and file opening | No UI state | Not implemented | Not implemented |

## Listing Data Flow

```text
local Session -> current_root -> list(current_root) -> bounded gRPC batches
                                                    -> append to UI rows
```

Both UIs preserve server arrival order and retain partial entries if the stream
fails. SwiftUI issues one listing per lifecycle. The TUI can run listings for
multiple BrowserContexts concurrently, routing events by context UUID and
generation. Pressing `R` replaces only the active context's listing, clearing
its entries and error first. Completing a stream releases its Listing but
leaves the shared Session alive until the UI shuts down.
