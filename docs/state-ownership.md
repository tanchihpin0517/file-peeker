# Client and UI State Ownership

The Rust client owns connection and transport state. Each UI owns only the
state needed to display its requested listing.

| Information | Rust client library | SwiftUI | TUI |
| --- | --- | --- | --- |
| Session UUID and registry | UUID and strong Session registry stored in `Client` | Retains the returned ID and Session | `App` keeps an allocation ledger plus ordered UI Session records; each `BrowserContext` retains the resolved Session behind `BrowserSource` |
| Unified operation interface | Private `SessionBackend` trait implemented by native `FsService` and remote `RemoteBackend` | Uses `Session` only | Uses `Session` only |
| Server process and connection lifecycle | Owned by the Session backend slot until consuming close | Starts on appearance; closes on disappearance | Interactive tracked tasks start and close Sessions; `App.shutdown` joins or cancels them and closes every allocated Client Session |
| gRPC channel and authentication | Stored in the remote Session backend | No | No |
| Startup path | Resolves shell expressions to an absolute lexical path on the selected host | `homePath` | CLI or New Browser path is resolved on its selected Session before BrowserContext creation |
| Active listing stream | Native `EntryStream` or Swift `Listing` adapter | Consumed by `loadTask` | Each `BrowserContext` owns one root task plus a task per loading expanded directory |
| Backend-only read stream | One local core stream or one remote gRPC response stream per operation | Not exposed | Not exposed |
| Received directory entries | Returned one-by-one, not retained | Appended to `rows` | Stored as visible tree rows with full path, depth, and expansion state |
| Loading and terminal error | Produced by the operation | `isLoading` and `errorMessage` | Root `ListingStatus` plus independent expanded-directory states |
| Selection | No | Not implemented | `selected_index` stored per `BrowserContext` |
| Session/tab selection | No | Not implemented | Ordered Session and Browser Context IDs, selected Session, active Context, and per-Session last-active Context |
| Navigation | No UI state | Not implemented | `h`/`l` replace the root listing; `o` expands/collapses a subtree without changing it |
| File opening | Stages remote files when needed and invokes the system opener | Not implemented | Browser Context owns confirmation, async opener task, and result feedback |
| File transfer | `Session::op_copy_file_from` streams between two backends | Not exposed | `App` owns the lazy clipboard, transfer task, overwrite modal, stale-result checks, and status; Browser Context exposes only an owned selection snapshot |

## Listing Data Flow

```text
SwiftUI: local Session -> resolve_path("~") -> op_list_dir_uniffi(home_path) -> append entries
TUI: no path -> Session Manager -> add local/SSH Session -> resolve browser path
     optional path -> local Session -> resolve_path(path) -> BrowserContext root_path
                                                       -> op_list_dir(root_path) -> append entries
                                                       -> op_list_dir(child path) -> insert subtree rows
                                                       -> op_open_file(file path) -> system opener
```

Both UIs preserve selected-host stream arrival order and retain partial entries
if a stream fails. SwiftUI issues one listing per lifecycle. The TUI can run
listings for multiple Browser Contexts and multiple expanded directories
concurrently. Bounded events are routed by context UUID; each context rejects
stale root generations and collapsed-directory request IDs before applying
transitions. Pressing `o` toggles a navigable row inline or confirms an
openable row through a modal. Pressing `h` or `l` changes the active context
path and starts a replacement tree with reset selection; pressing `R` replaces
the tree at the same path. Completing a stream releases its Listing but leaves
the shared Session alive until it is explicitly closed or the UI shuts down.
