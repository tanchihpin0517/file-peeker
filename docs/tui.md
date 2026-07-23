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
    │   ├── Context A -> Session 1 -> /path/a
    │   └── Context B -> Session 1 -> /path/b
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
| `entries` | Individual entries appended in selected-host stream arrival order |
| `selected_index` | Independent visual selection for this context |
| `ListingStatus` | Mutually exclusive loading, complete, or failed outcome |
| `listing_task` | Tokio task handle used to cancel active work |
| `generation` | Rejects events from a cancelled or replaced listing |

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
4. The context-owned task consumes the native entry `op_list_dir` stream with
   `try_next`.
5. Every entry, completion, or failure is sent through a bounded channel as a
   private context event containing the context UUID and generation.
6. App routes the event by UUID. The matching Browser Context rejects stale
   generations and applies the entry or terminal transition itself.

The bounded event channel lets filesystem/network work continue asynchronously
while preserving backpressure through the listing stream. The synchronous
terminal loop redraws approximately every 50 milliseconds and handles a bounded
number of listing events per frame. Only the active context is rendered;
inactive contexts continue loading independently.

## Navigation, refresh, and cancellation

Lowercase `l` enters the selected entry when its `navigable` field is true;
files and other non-navigable entries have no action. Lowercase `h` changes to
the current path's lexical parent and has no action at the filesystem root. A
path change aborts the current listing, updates the context path, resets the
selection, and starts a replacement listing. The attempted path remains visible
if that listing fails, so `R` can retry it.

Uppercase `R` refreshes only the active context. Refresh aborts its current
listing task, increments its generation, clears entries and failed status, marks
the Listing Status as loading, and starts a replacement listing using the same
Session and path.
The selected numeric index is retained while loading, displayed at the nearest
currently available row, and clamped permanently when the stream terminates.

The task handle stops the old listing work by dropping its native stream; a
future remote-backed context would consequently drop its gRPC response stream.
The context's private generation check separately protects against old events
that were already queued before cancellation. Dropping a Browser Context also
aborts its active listing task. `Up`/`Down` and `k`/`j` move the active context's
selection within the available rows. The first received entry is selected
automatically. Lowercase `r` has no action. `q` and `Esc` exit.

Entry kinds use aligned prefixes and styles so they remain distinguishable in
the list: files use a two-space prefix and terminal defaults, directories use a
blue bold `▸`, symlinks use a cyan `@`, and other entries use a yellow `?`.
The reversed selection modifier is applied on top of the entry-specific style.

## Errors and shutdown

A terminal listing error leaves earlier entries visible and displays the error
for that context. A Session closed before a refresh is reported through the
same failed-event path.

Shutdown aborts every Browser Context listing task, then attempts to close every
Session started by App. All Sessions are attempted even if one close fails; the
first close error is returned after cleanup. Startup and terminal-initialization
failures use the same shutdown path.

## Current UI limits

- Startup without a path displays help; `file-peeker PATH` creates one local
  browser context.
- The help screen is informational and exits with `q` or `Esc`.
- Only the active context is visible and refreshable from the keyboard.
- There is not yet a command to create, select, close, or lay out contexts.
- Entries retain selected-host filesystem order. Navigation changes the active
  context's path; there is no sorting or searching.

Unit tests cover empty App ownership, harmless empty shutdown, initial listing,
partial-result errors, navigation and failed navigation, refresh clearing,
selection preservation and clamping, stale-event rejection, bounded
backpressure, drop cancellation, and responsive layout.
