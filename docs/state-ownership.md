# Client and TUI State Ownership

The Client layer owns connection and transport state. The TUI owns only the
state needed to display its requested listing.

| Information | Client layer | TUI |
| --- | --- | --- |
| Connection ID and registry | `ConnectionId` and the strong connection registry stored in `Client` | `App` keeps ordered UI rows keyed by their own record ID, each holding the `ConnectionId` it was given |
| Unified operation interface | One `ServerConnection` per connection, borrowed by ID through one guarded accessor | Names a connection by ID only |
| Server process and connection lifecycle | Owned by the Client's connection slot until consuming close | Interactive tracked tasks open and close connections; `App.shutdown` joins or cancels them and calls `Client::close_all` |
| gRPC channel and authentication | Stored in the remote connection | No |
| Startup path | Resolves shell expressions to an absolute lexical path on the selected host | CLI or New Browser path is resolved on its selected connection before BrowserContext creation |
| Active listing stream | Native `EntryStream` | Each `BrowserContext` owns one root task plus a task per loading expanded directory |
| Client-only read stream | One gRPC response stream per operation | Not exposed |
| Received directory entries | Returned one-by-one, not retained | Stored as visible tree rows with full path, depth, and expansion state |
| Loading and terminal error | Produced by the operation | Root `ListingStatus` plus independent expanded-directory states |
| Selection | No | `selected_index` stored per `BrowserContext` |
| Filtering | No | Query and the selection to restore stored per `BrowserContext`; the visible window is derived from the rows per keystroke rather than removing any |
| Connection/tab selection | No | Ordered connection record and Browser Context IDs, selected connection, active Context, and per-connection last-active Context |
| Navigation | No UI state | `h`/`l` replace the root listing; `o` expands/collapses a subtree without changing it |
| File transfer | `Client::op_copy_file` streams between two connections | `App` owns the lazy clipboard, transfer task, overwrite modal, stale-result checks, and status; Browser Context exposes only an owned selection snapshot |

## Listing Data Flow

```text
TUI: no path -> Connection Manager -> add local/SSH connection -> resolve browser path
     optional path -> local connection -> op_resolve_path(id, path) -> BrowserContext root_path
                                                       -> op_list_dir(id, root_path) -> append entries
                                                       -> op_list_dir(id, child path) -> insert subtree rows
```

The TUI preserves selected-host stream arrival order and retains partial entries
if a stream fails. It can run listings for multiple Browser Contexts and
multiple expanded directories concurrently. Bounded events are routed by context
UUID; each context rejects stale root generations and collapsed-directory
request IDs before applying transitions. Pressing `o` toggles a navigable row
inline and does nothing on any other row. Pressing `h` or `l` changes
the active context path and starts a replacement tree with reset selection;
pressing `R` replaces the tree at the same path. Completing a stream releases its
stream handle but leaves the shared connection alive until it is explicitly
closed or the UI shuts down.
