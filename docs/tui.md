# TUI Implementation

`crate::tui` is a Ratatui frontend backed by the shared Rust Client. It presents ordered browser tabs across local and SSH Sessions
while rendering one active Browser Context at a time.

## Ownership

```text
main
└── App
    ├── Client and allocated-Session cleanup ledger
    ├── ordered SessionRecord IDs + lookup
    │   └── target, stable label, lifecycle state, Client ID/Session,
    │       last-active Browser Context
    ├── ordered Browser Context IDs + lookup
    │   └── BrowserTab -> SessionRecord ID + BrowserContext
    ├── active Browser Context and selected Session IDs
    ├── bounded Browser Context event channel
    ├── unbounded low-volume lifecycle event channel
    ├── lazy transfer clipboard and one tracked transfer task
    └── tracked start, resolve-path, and close tasks
```

`app/session.rs` owns only TUI Session presentation. Its UUID exists before a
Client Session is allocated, so a `Starting` row has stable identity. Labels
are derived once: `Local` or the raw SSH destination, with monotonic `#2`,
`#3`, and later suffixes for duplicates. Removing an earlier duplicate never
renames survivors.

Lookup maps route events by identity. Separate order vectors define Session
Manager order and tab order. Central App helpers insert, activate, and remove
records so active and last-active references remain valid.

## Startup and asynchronous lifecycle

With a CLI path, App synchronously starts one local Session, records its Client
ID for cleanup, resolves the path on that Session, creates the first Browser
Context, and activates it before terminal initialization. Without a path, App
starts sessionless and opens Session Manager over the normal empty shell.

Interactive Session startup first inserts a `Starting` row and spawns a tracked
task. `Client::start_session` returns the retained Session itself, which the
task forwards in one `SessionReady` event; the Client owns the Session either
way, so an aborted task cannot leak one and App keeps no separate cleanup
ledger. The row then becomes `Ready` or retains a visible
`Failed` error. The available states are deliberately `Starting`, `Ready`,
`Failed`, and `Closing`; there is no heartbeat from which to claim connected
or disconnected state.

New Browser submits its path to a tracked `Session::op_resolve_path` task.
Events include a fresh request UUID and Session-record UUID. Only the currently
open matching dialog accepts the result. Success appends and activates a new
Browser Context; failure preserves both input and error for correction. Cancel
aborts the task and late events are ignored.

Browser listing events remain on a bounded channel to preserve
filesystem/network backpressure. Session lifecycle events use a separate
unbounded channel because they are low-volume and must not strand an allocated
Client Session while waiting to report its ID. App pumps a bounded number of
browser events and all available lifecycle events once per 50 ms terminal-loop
iteration.

## Browser Contexts and tabs

Each Browser Context independently owns its resolved root, visible tree rows,
selection, viewport, expansion state, loading/error state, and listing tasks.
Inactive contexts keep their state and continue
loading. Browser events carry Context UUID and generation/request identity;
removed contexts and stale listing generations are ignored.

The tab strip displays `SessionLabel:compact-path`, keeps the active tab
visible at narrow widths, and does not wrap. The header displays the active
Session label and full path. `[` and `]` switch tabs with wrapping. `n` creates
another browser on the active tab's Session and defaults to its full root path.
`x` immediately closes the active read-only Context without closing its
Session. Removal activates the Context that moves into the same order index, or
the preceding final Context.

The Files list occupies the full body; there is no placeholder Locations
sidebar. With no tabs, the shell explains `S: sessions` and `q/Esc: quit`.

Within a browser, `j`/`k` or arrows move selection, `Ctrl-D`/`Ctrl-U` scroll,
`h`/`l` navigate, `o` expands or collapses a navigable row, and `R` refreshes.
Activating a non-navigable row does nothing. Expansion, navigation, streaming,
and stale-event rejection are owned by `browser_context.rs`.

`y` copies the selected regular file into an App-owned lazy clipboard containing
only source Session identity, full path, and original name. Closing the source
Browser Context does not clear it; closing its Session does. `p` streams that
file into the active Context root, including local/remote upload, download, and
relay routes. Only one transfer runs at a time. The footer shows an
indeterminate `Copying…` state and the final byte count.

Paste initially fails if the destination name exists. `AlreadyExists` opens a
capturing overwrite confirmation: `Enter`/`y` restarts from byte zero with
replace semantics, while `q`/`Esc` cancels. Successful paste retains the
clipboard and refreshes the original destination Context only when it still
exists with the same Session and root. Switching tabs or closing that Context
does not cancel the transfer.

## Session Manager and dialogs

`S` opens Session Manager. It lists Sessions in creation order with label,
target summary, lifecycle state, and browser count. `j`/`k` or arrows move
selection while preserving Session identity across row changes.

- `a` opens the Local/SSH chooser. Local starts immediately; SSH accepts an
  unmodified printable destination.
- `Enter` on a ready Session activates its most recently active Context, or
  opens New Browser at `~` when it has none.
- `n` opens New Browser for the selected ready Session.
- `d` removes an unallocated failed row or asks to close a ready Session.
- `q`, `Esc`, or `S` dismisses the manager without quitting.

Confirmed Session close changes the row to `Closing`, drops all of its Browser
Contexts, and starts `Client::close_session` without blocking input. Completion
removes the row. Since Client removal precedes backend shutdown, a close error
is terminal and is retained in the application footer rather than exposed as a
fake retry.

Input routing uses one explicit mode with modal confirmation, chooser/text
dialogs, Session Manager, then main browser/empty state precedence. Undefined
keys in a capturing mode are ignored and never leak to navigation, yank/paste,
or process quit.

## Rendering and shutdown

Rendering uses a header, one-row tab strip, full-width Files body, and
mode-aware footer. Overlays use `Clear`, constrain themselves to the current
terminal, wrap paths/destinations/errors, and show starting, resolving, failed,
and closing feedback.

Shutdown first aborts and joins the active transfer, then cancels and drops all
Browser Context work, aborts and joins path
tasks, aborts and joins startup tasks, and drains already-enqueued allocation
events. Already-running close tasks are allowed to finish because they may have
consumed their Client registry entries. App then attempts every remaining ID in
the allocation ledger and returns the first close error only after attempting
all cleanup.
