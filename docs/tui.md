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

`App` creates and owns the Client. Sessions remain strongly owned by the
Client's registry; BrowserContexts store only a Session UUID. Temporary Session
references are obtained through `Client::get_session` when discovering the
initial root or starting a listing.

`main` owns only the event receiver and terminal lifecycle. It creates App,
calls `App::start`, runs the draw/input loop, restores the terminal, and calls
`App::shutdown`.

## BrowserContext

Each BrowserContext represents one independent browsing instance:

| Field | Purpose |
| --- | --- |
| `id` | UUID used to route asynchronous events |
| `session_id` | Reference to a Client-owned Session |
| `path` | Directory requested by this context |
| `entries` | Batches accumulated in server arrival order |
| `selected_index` | Independent visual selection for this context |
| `loading` / `error` | Display status, including partial-result failures |
| `listing_task` | Tokio task handle used to cancel active work |
| `generation` | Rejects events from a cancelled or replaced listing |

Contexts are stored in a `HashMap<BrowserContextId, BrowserContext>`. Multiple
contexts may reference the same Session, the same path, or both. UUID identity
keeps those instances independent.

## Startup and listing flow

1. `App::start` creates one local Session through the App-owned Client.
2. App records the Session ID, obtains the retained Session, and calls
   `op_current_root`.
3. App creates the initial BrowserContext for that path, marks it active, and
   starts its listing.
4. The listing task resolves the Session through Client and consumes the native
   batched `op_list` stream with `try_next`.
5. Every batch, completion, or failure is sent to the UI loop as an `AppEvent`
   containing the context UUID and generation.
6. `App::update` applies an event only when both identifiers still match, then
   appends entries or records the terminal state.

The event channel lets filesystem/network work continue asynchronously while
the synchronous terminal loop redraws approximately every 50 milliseconds.
Only the active context is rendered; the map and event routing are ready for a
future pane or tab UI.

## Refresh and cancellation

Uppercase `R` refreshes only the active context. Refresh aborts its current
`listing_task`, increments its generation, clears entries and errors, marks it
loading, and starts a replacement listing using the same Session ID and path.
The selected numeric index is retained while loading, displayed at the nearest
currently available row, and clamped permanently when the stream terminates.

The task handle stops the old gRPC work. The generation check separately
protects against old events that were already queued before cancellation.
`Up`/`Down` and `k`/`j` move the active context's selection within the available
rows. The first received entry is selected automatically. Lowercase `r` has no
action. `q` and `Esc` exit.

Entry kinds use aligned prefixes and styles so they remain distinguishable in
the list: files use a two-space prefix and terminal defaults, directories use a
blue bold `▸`, symlinks use a cyan `@`, and other entries use a yellow `?`.
The reversed selection modifier is applied on top of the entry-specific style.

## Errors and shutdown

A terminal listing error leaves earlier batches visible and displays the error
for that context. A missing Session is reported through the same failed-event
path.

Shutdown aborts every BrowserContext listing task, then attempts to close every
Session started by App. All Sessions are attempted even if one close fails; the
first close error is returned after cleanup. Startup and terminal-initialization
failures use the same shutdown path.

## Current UI limits

- Startup creates one local Home context.
- Only the active context is visible and refreshable from the keyboard.
- There is not yet a command to create, select, close, or lay out contexts.
- Entries retain server order. Selection is visual only; there is no navigation,
  activation, sorting, searching, or file opening.

Unit tests cover empty App ownership, harmless empty shutdown, unique context
IDs, contexts sharing a Session ID, concurrent event routing, partial-result
errors, selection bounds and isolation, refresh index preservation,
stale-event rejection, and responsive layout.
