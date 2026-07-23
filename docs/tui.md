# TUI Implementation

The `file-peeker-tui` crate is a read-only Ratatui frontend backed by the shared
Rust Client. Its current UI displays one active browser context, while its state
and event model support multiple concurrent directory listings.

## Ownership

```text
main
└── App
    ├── Client
    │   └── Session registry
    ├── started Session IDs
    ├── BrowserContext map
    │   ├── Context A -> Session 1 -> /path/a -> visible tree rows
    │   └── Context B -> Session 1 -> /path/b -> visible tree rows
    └── active BrowserContext ID
```

`App` always creates and owns the Client. When a startup path is supplied,
Sessions remain strongly owned by the Client's registry and each Browser
Context also retains its resolved Session behind its `BrowserSource` trait
object. `Client::get_session` is used once after startup, not on each refresh.

`main` parses an optional path, creates the event channel and help text, owns the
terminal lifecycle and event receiver, and maps key presses to App commands. It
always creates App, calls `App::start` with that optional path, runs the
draw/input loop, restores the terminal, and calls `App::shutdown`.

## Browser Context

Each Browser Context represents one independent browsing instance:

| Field | Purpose |
| --- | --- |
| `id` | UUID used to route asynchronous events |
| `source` | Listing source; production stores the resolved, Client-owned Session behind `BrowserSource` |
| `root_path` | Absolute lexical directory path resolved by the Session |
| `rows` | Visible entries with their full selected-host path, tree depth, and expansion state |
| `selected_index` | Independent visual selection for this context |
| `ListingStatus` | Mutually exclusive loading, complete, or failed outcome |
| listing tasks | One root task plus independently cancellable expanded-directory tasks |
| `generation` and request IDs | Reject events from replaced roots or collapsed directory requests |
| open state | Pending confirmation, opener task, and opening/success/failure feedback |

Browser Contexts are stored in a `HashMap<BrowserContextId, BrowserContext>`.
Multiple contexts may reference the same Session, the same path, or both. UUID
identity keeps those instances independent.

## Startup and listing flow

1. `App::start` receives the optional path. Without one, it leaves the app
   sessionless so the shared render loop displays clap-generated help.
2. With a path, `App::start` creates one local Session through the App-owned
   Client and obtains the retained Session.
3. App resolves the path through the Session. The selected host expands `~` and
   environment variables, makes the path absolute against its working
   directory, and lexically normalizes it. App creates the initial Browser
   Context with that Session and resolved root path, which starts the first
   listing immediately.
4. The context-owned root task consumes the native entry `op_list_dir` stream
   with `try_next`.
5. Every entry, completion, or failure is sent through a bounded channel as a
   private context event containing the context UUID and generation.
6. App routes the event by UUID. The matching Browser Context rejects stale
   generations and applies the entry or terminal transition itself.

The bounded event channel lets filesystem/network and file-opening work continue
asynchronously while preserving listing-stream backpressure. The synchronous
terminal loop redraws approximately every 50 milliseconds and handles a bounded
number of events per frame. Only the active context is rendered; inactive
contexts continue loading independently.

## Inline expansion, opening, and navigation

Lowercase `l` enters the selected entry when its `navigable` field is true;
files and other non-navigable entries have no action. Lowercase `h` changes to
the current path's lexical parent and has no action at the filesystem root. A
path change aborts the current listing, updates the context path, resets the
selection, and starts a replacement listing. The attempted path remains visible
if that listing fails, so `R` can retry it.

Lowercase `o` is context-sensitive. On a navigable directory or directory
symlink it starts a separate `op_list_dir` request and streams children directly
beneath the selected row without changing the context root. Each row stores its
full path, so nested expansions and `l` navigation operate on the selected row
rather than reconstructing a path from only the root. Multiple directories may
load concurrently. Children retain their per-directory selected-host order and
are indented by depth.

Pressing `o` on an expanded, loading, or failed directory collapses it, aborts
every active listing in that subtree, removes all descendant rows, and rejects
already queued events by request ID. Collapsed contents are not cached:
re-expanding starts a fresh listing. A failed expansion retains partial children
and shows an error marker until it is collapsed.

On a regular file or non-directory symlink, `o` opens a modal containing the
full path. The modal snapshots that path and captures input: `o` confirms,
`Esc` or `q` cancels the modal without exiting, and other keys do nothing.
Confirmation calls `Session::op_open_file` asynchronously. The footer reports
opening, success, or failure. Navigable symlinks expand; other special entries
have no `o` action.

Uppercase `R` refreshes only the active context. Refresh aborts its root and
expanded-directory listing tasks, increments its generation, clears rows,
expansion errors, and failed root status, marks the Listing Status as loading,
and starts a replacement root listing using the same Session and path.
The selected numeric index is retained while loading, displayed at the nearest
currently available row, and clamped permanently when the stream terminates.

Aborting a task drops its native stream; a future remote-backed context would
consequently drop its gRPC response stream. The context's root generation and
per-expansion request checks separately protect against events that were already
queued before cancellation. Dropping a Browser Context aborts all listing and
opening tasks. `Up`/`Down` and `k`/`j` move the active context's selection
within the visible rows. The first received entry is selected automatically.
Lowercase `r` has no action. Outside the confirmation modal, `q` and `Esc` exit.

Entry kinds use aligned prefixes and styles so they remain distinguishable in
the list: files use a two-space prefix and terminal defaults, directory actions
use `▸`, `…`, `▾`, or `!` for collapsed, loading, expanded, or failed state,
symlinks use cyan, and other entries use a yellow `?`. The reversed selection
modifier is applied on top of the entry-specific style.

## Errors and shutdown

A terminal listing error leaves earlier entries visible and displays the error
for that context. A Session closed before a refresh is reported through the
same failed-event path.

Shutdown aborts every Browser Context listing and opening task, then attempts to
close every Session started by App. All Sessions are attempted even if one close
fails; the first close error is returned after cleanup. Startup and
terminal-initialization failures use the same shutdown path.

## Current UI limits

- Startup without a path displays help; `file-peeker PATH` creates one local
  browser context.
- The help screen is informational and exits with `q` or `Esc`.
- Only the active context is visible and refreshable from the keyboard.
- There is not yet a command to create, select, close, or lay out contexts.
- Entries retain selected-host filesystem order within each directory. There is
  no sorting or searching.

Unit tests cover empty App ownership, harmless empty shutdown, initial listing,
partial-result errors, navigation and failed navigation, refresh clearing,
selection preservation and clamping, nested expansion, collapse/reload,
stale-event rejection, bounded backpressure, open confirmation and failures,
drop cancellation, popup rendering, and responsive layout.
