# Client and UI State Ownership

The Rust client owns connection and transport state. Each UI owns the data and
interaction state needed to display a directory tree.

| Information | Rust client library | SwiftUI | TUI |
| --- | --- | --- | --- |
| Local or remote connection target | Stored in `Session` | References the session | References the session |
| Local versus remote mode | Stored in `Session` | No | No |
| Server process lifecycle | Stored in `Session` | No | No |
| Server port, token, and stdin lifetime lease | Stored in the client lifecycle | No | No |
| SSH SOCKS and control-socket state | Stored in the client lifecycle | No | No |
| Heartbeat and fatal connection error | Stored in the client lifecycle | No | No |
| Active listing TCP connection | Stored in `Listing` | No | No |
| Partially received protocol frame | Stored in `Listing` | No | No |
| Listing state: active, complete, or failed | Stored in `Listing` | No | No |
| Listing's requested parent path | Temporarily stored to construct child paths | No | No |
| Current received batch | Returned to the caller, not retained | Merged into `treeRows` | Merged into `rows` |
| Accumulated directory entries | Not stored | Stored in `treeRows` | Stored in `rows` |
| Parent-child relationship | No | `parentPath` | `parent_path` |
| Display depth | No | Stored per row | Stored per row |
| Expanded or collapsed state | No | Stored per row | Stored per row |
| Current displayed directory | No | `currentPath` | `path` |
| Selection | No | Entry path in SwiftUI `State` | Numeric row index |
| Sorting preference | No | `sortOrder` | No sorting currently |
| Sorted visible rows | No | Derived during rendering | Rows are stored in display order |
| Root loading state | No | `isLoading` | `loading` |
| Expanding-directory loading state | No | `loadingTreePaths` | `loading_tree_paths` |
| Tasks consuming listing batches | No | `loadTask` and `expansionTasks` | `root_task` and `expansion_tasks` |
| Stale-request generation | No | `generation` | `generation` |
| Operation or protocol error | Produces a typed `FilePeekerError` | Converts it to displayed text | Converts it to displayed text |
| Per-directory expansion error | No | Stored on `DisplayRow` | Stored on `DisplayRow` |
| View style, search, and popovers | No | Stored as SwiftUI state | No |
| Cached collapsed children | No | No; descendants are removed | No; descendants are removed |

## Listing Data Flow

```text
Server batch
    |
    v
Rust Listing parses it
    |
    v
next_batch() / nextBatch() returns directory entries
    |
    v
UI merges entries into its display tree
    |
    v
Rust Listing does not retain the batch
```

The client's `Listing` retains only the operation stream, an incomplete frame,
the requested parent path, and its active, complete, or failed status. It does
not accumulate directory entries after returning them to the caller.

SwiftUI stores parent-linked rows in `BrowserModel.treeRows` and derives a
sorted, depth-first visible list during rendering. The TUI stores its visible
rows directly in depth-first display order. Both remove descendants when a
directory is collapsed, so expanding it again starts a fresh listing.
