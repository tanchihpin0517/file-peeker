# Client and UI State Ownership

The Rust client owns connection and transport state. Each UI owns only the
state needed to display its requested listing.

| Information | Rust client library | SwiftUI | TUI |
| --- | --- | --- | --- |
| Session UUID and registry | UUID and strong Session registry stored in `Client` | Retains the returned ID and Session | `App` tracks started IDs; each `BrowserContext` retains the resolved Session |
| Server process and connection lifecycle | Stored in the Session connection | Starts on appearance; closes on disappearance | `App.start` creates it when given a path; `App.shutdown` closes it after terminal restoration |
| gRPC channel and authentication | Stored in the Session connection | No | No |
| Startup path | Expands `~` and variables in the server environment | `homePath` | Optional CLI path forwarded unchanged |
| Active listing stream | Native `ListStream` or Swift `Listing` adapter | Consumed by `loadTask` | Each `BrowserContext` owns the task that consumes native `op_list` |
| Received directory entries | Returned batch-by-batch, not retained | Appended to `rows` | Appended to the matching context's `entries` |
| Loading and terminal error | Produced by the operation | `isLoading` and `errorMessage` | One `ListingStatus` stored independently in each `BrowserContext` |
| Selection | No | Not implemented | `selected_index` stored per `BrowserContext` |
| Navigation, expansion, and file opening | No UI state | Not implemented | Not implemented |

## Listing Data Flow

```text
SwiftUI: local Session -> current_root -> list(current_root) -> append batches
TUI: optional path -> help screen
                   -> local Session -> list(path) -> server resolves path -> append batches
```

Both UIs preserve server arrival order and retain partial entries if the stream
fails. SwiftUI issues one listing per lifecycle. The TUI can run listings for
multiple Browser Contexts concurrently. Bounded events are routed by context
UUID; each context privately rejects stale generations and applies its own
listing transitions. Pressing `R` replaces only the active context's listing,
clearing its entries and failed status first. Completing a stream releases its
Listing but leaves the shared Session alive until the UI shuts down.
