# Client and TUI State Ownership

The Client and Session layer owns connection and transport state. The TUI owns
only the state needed to display its requested listing.

| Information | Client and Session layer | TUI |
| --- | --- | --- |
| Session UUID and registry | UUID and strong Session registry stored in `Client` | `App` keeps ordered UI Session records, each holding the started `Arc<Session>` |
| Unified operation interface | Private `SessionBackend` trait with one implementation, `ServerBackend`, borrowed through one guarded accessor | Uses `Session` only |
| Server process and connection lifecycle | Owned by the Session backend slot until consuming close | Interactive tracked tasks start and close Sessions; `App.shutdown` joins or cancels them and calls `Client::close_all` |
| gRPC channel and authentication | Stored in the remote Session backend | No |
| Startup path | Resolves shell expressions to an absolute lexical path on the selected host | CLI or New Browser path is resolved on its selected Session before BrowserContext creation |
| Active listing stream | Native `EntryStream` | Each `BrowserContext` owns one root task plus a task per loading expanded directory |
| Backend-only read stream | One gRPC response stream per operation | Not exposed |
| Received directory entries | Returned one-by-one, not retained | Stored as visible tree rows with full path, depth, and expansion state |
| Loading and terminal error | Produced by the operation | Root `ListingStatus` plus independent expanded-directory states |
| Selection | No | `selected_index` stored per `BrowserContext` |
| Session/tab selection | No | Ordered Session and Browser Context IDs, selected Session, active Context, and per-Session last-active Context |
| Navigation | No UI state | `h`/`l` replace the root listing; `o` expands/collapses a subtree without changing it |
| File transfer | `Session::op_copy_file_from` streams between two backends | `App` owns the lazy clipboard, transfer task, overwrite modal, stale-result checks, and status; Browser Context exposes only an owned selection snapshot |

## Listing Data Flow

```text
TUI: no path -> Session Manager -> add local/SSH Session -> resolve browser path
     optional path -> local Session -> resolve_path(path) -> BrowserContext root_path
                                                       -> op_list_dir(root_path) -> append entries
                                                       -> op_list_dir(child path) -> insert subtree rows
```

The TUI preserves selected-host stream arrival order and retains partial entries
if a stream fails. It can run listings for multiple Browser Contexts and
multiple expanded directories concurrently. Bounded events are routed by context
UUID; each context rejects stale root generations and collapsed-directory
request IDs before applying transitions. Pressing `o` toggles a navigable row
inline and does nothing on any other row. Pressing `h` or `l` changes
the active context path and starts a replacement tree with reset selection;
pressing `R` replaces the tree at the same path. Completing a stream releases its
stream handle but leaves the shared Session alive until it is explicitly closed
or the UI shuts down.
