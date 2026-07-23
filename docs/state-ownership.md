# Client and UI State Ownership

The Rust client owns connection and transport state. Each UI owns only the
state needed to display its requested listing.

| Information | Rust client library | SwiftUI | TUI |
| --- | --- | --- | --- |
| Session UUID and registry | UUID and strong Session registry stored in `Client` | Retains the returned ID and Session | `App` tracks started IDs; each `BrowserContext` retains the resolved Session |
| Unified operation interface | Private `SessionBackend` trait implemented by native `FsService` and remote `RemoteBackend` | Uses `Session` only | Uses `Session` only |
| Server process and connection lifecycle | Owned by the Session backend slot until consuming close | Starts on appearance; closes on disappearance | `App.start` creates it when given a path; `App.shutdown` closes it after terminal restoration |
| gRPC channel and authentication | Stored in the remote Session backend | No | No |
| Startup path | Resolves shell expressions to an absolute lexical path on the selected host | `homePath` | Optional CLI path resolved before BrowserContext creation |
| Active listing stream | Native `EntryStream` or Swift `Listing` adapter | Consumed by `loadTask` | Each `BrowserContext` owns the task that consumes native `op_list_dir` |
| Backend-only read stream | One local core stream or one remote gRPC response stream per operation | Not exposed | Not exposed |
| Received directory entries | Returned one-by-one, not retained | Appended to `rows` | Appended to the matching context's `entries` |
| Loading and terminal error | Produced by the operation | `isLoading` and `errorMessage` | One `ListingStatus` stored independently in each `BrowserContext` |
| Selection | No | Not implemented | `selected_index` stored per `BrowserContext` |
| Navigation | No UI state | Not implemented | `h`/`l` mutate the active context path and replace its listing |

## Listing Data Flow

```text
SwiftUI: local Session -> resolve_path("~") -> op_list_dir_uniffi(home_path) -> append entries
TUI: optional path -> help screen
                   -> local Session -> resolve_path(path) -> BrowserContext root_path
                                                       -> op_list_dir(root_path) -> append entries
```

Both UIs preserve server arrival order and retain partial entries if the stream
fails. SwiftUI issues one listing per lifecycle. The TUI can run listings for
multiple Browser Contexts concurrently. Bounded events are routed by context
UUID; each context privately rejects stale generations and applies its own
listing transitions. Pressing `h` or `l` changes the active context path and
starts a replacement listing with reset selection; pressing `R` replaces the
listing at the same path. Both clear entries and failed status first. Completing
a stream releases its Listing but leaves the shared Session alive until the UI
shuts down.
