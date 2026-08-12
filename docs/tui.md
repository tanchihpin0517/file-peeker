# TUI Implementation

`crate::tui` is a Ratatui frontend backed by the shared Rust Client. It presents
ordered browser tabs across local and SSH connections while rendering one active
Browser Context at a time.

## Ownership

```text
main
└── App
    ├── Client
    ├── ordered ConnectionRecord IDs + lookup
    │   └── target, stable label, lifecycle state, ConnectionId,
    │       last-active Browser Context
    ├── ordered Browser Context IDs + lookup
    │   └── BrowserTab -> ConnectionRecord ID + BrowserContext
    ├── active Browser Context and selected connection IDs
    ├── bounded Browser Context event channel
    ├── unbounded low-volume lifecycle event channel
    ├── lazy transfer clipboard and one tracked transfer task
    └── tracked start, resolve-path, and close tasks
```

`app/connection.rs` owns only TUI connection-row presentation. Its
`ConnectionRecordId` exists before the Client has a `ConnectionId`, so a
`Starting` row has stable identity, and it survives a failed open that never
produced a connection at all. Labels are derived once: `Local` or the raw SSH destination, with monotonic `#2`,
`#3`, and later suffixes for duplicates. Removing an earlier duplicate never
renames survivors.

Lookup maps route events by identity. Separate order vectors define Connection
Manager order and tab order. Central App helpers insert, activate, and remove
records so active and last-active references remain valid.

## Startup and asynchronous lifecycle

With a CLI path, App synchronously opens one local connection, resolves the path
on it, creates the first Browser Context, and activates it before terminal
initialization. Without a path, App starts with no connections and opens
Connection Manager over the normal empty shell.

Interactive startup first inserts a `Starting` row and spawns a tracked task.
`Client::open` returns the `ConnectionId` of the retained connection, which the
task forwards in one `ConnectionReady` event; the Client owns the connection
either way, so an aborted task cannot leak one and App keeps no separate cleanup
ledger. The row then becomes `Ready` or retains a visible `Failed` error. The available states are deliberately `Starting`, `Ready`,
`Failed`, and `Closing`; there is no heartbeat from which to claim connected
or disconnected state.

New Browser submits its path to a tracked `Client::op_resolve_path` task.
Events include a fresh request UUID and connection-record UUID. Only the currently
open matching dialog accepts the result. Success appends and activates a new
Browser Context; failure preserves both input and error for correction. Cancel
aborts the task and late events are ignored.

Browser listing events remain on a bounded channel to preserve
filesystem/network backpressure. Connection lifecycle events use a separate
unbounded channel because they are low-volume and must not strand an opened
connection while waiting to report its ID.

A listing event carries a group of entries rather than a single entry, and a group
is whatever the stream already had ready. Placing entries is what makes this
matter: appending a group under a row means finding that row and making room after
its existing children, both passes over the tree, so paying them per entry makes
opening a directory cost time quadratic in its size -- a 16k-entry directory spent
around 380ms of the loop placing rows, and a larger one proportionally more, which
is what a stall while a preview loads is made of. Per group it is a pass per few
hundred entries instead, and the same code carries every listing, so a root
listing, an expansion, and anything later built on them all stay off that curve.
Grouping by readiness rather than by count is what keeps it from costing latency:
a server that trickles still delivers entry by entry, while a directory sitting in
the page cache arrives in a handful of groups. The channel bound counts groups, so
its bound in entries is that multiplied by the group size.

The terminal loop draws and then waits, with no fixed-interval poll: it selects
over the terminal event channel and both event channels, so an idle browser costs
no redraws and an arriving listing waits on no timer. The first event to arrive
wakes the loop, which then keeps collecting for one frame budget before drawing,
so a listing costs one redraw per frame rather than one per entry. Each wake
applies every queued event rather than a fixed count per iteration; metering
events out per iteration used to make a listing fill in a chunk at a time,
capping the UI far below what the server streams. The listing channel bound is
correspondingly deep enough for a listing task to run a frame ahead of the
renderer while still bounding a producer that outruns it.

Terminal events arrive over their own channel, filled by a task that owns the
Ratatui event stream. That stream registers a wake task on its first `Pending`
and suppresses later registrations until it fires, so polling it from a `select!`
arm that loses the race can strand a wake-up and freeze the UI; one task awaiting
it to completion cannot. The channel is also what makes a burst coalescable: the
loop applies the waking event and every one queued behind it before it redraws,
because a held scroll key delivers presses faster than a frame draws and drawing
per press makes the screen chase the backlog after the key is up.

## Browser Contexts and tabs

Each Browser Context independently owns its resolved root, visible tree rows,
selection, viewport, active filter, expansion state, loading/error state, and
listing tasks.
Inactive contexts keep their state and continue
loading. Browser events carry Context UUID and generation/request identity;
removed contexts and stale listing generations are ignored.

The tab strip displays `ConnectionLabel:compact-path`, keeps the active tab
visible at narrow widths, and does not wrap. The header displays the active
connection label and full path. `[` and `]` switch tabs with wrapping. `n`
creates another browser on the active tab's connection and defaults to its full
root path.
`x` immediately closes the active read-only Context without closing its
connection. Removal activates the Context that moves into the same order index, or
the preceding final Context.

The Files list occupies the full body; there is no placeholder Locations
sidebar. With no tabs, the shell explains `S: connections` and `q/Esc: quit`.

Within a browser, `j`/`k` or arrows move selection, `Ctrl-D`/`Ctrl-U` scroll,
`h`/`l` navigate, `o` expands or collapses a navigable row, and `R` refreshes.
A Browser Context owns its own scroll offset rather than leaving it to the list
widget, and hands the renderer just the rows one frame shows, with the selection
placed relative to them. That is what keeps a keystroke costing what is on screen
rather than what the tree holds: the renderer builds one list item per row it is
given, so building them for every row made scrolling in a large directory cost
time proportional to its size.
Activating a non-navigable row does nothing. Expansion, navigation, streaming,
and stale-event rejection are owned by `browser_context.rs`.

## Filtering

`/` opens a filter prompt over the active browser. Typing narrows the rows to
the names that contain the query, case-insensitively, along with the ancestors of
each match so a hit inside a directory still reads as being inside it. The
selection follows the nearest match rather than a carried-along ancestor. `Tab`
extends the query to the longest prefix every match agrees on, the way a shell
completes a partial word, which can never narrow what is on screen because every
match starts with the prefix it completes to. `Esc` closes the prompt and puts the
selection back where it started.

`Enter` takes two presses, because the first has something to say: a query that
only narrowed to a row is not yet that row, so the first press spells the selected
match out in the prompt and the second, now that the prompt names it outright,
closes the prompt on it. A row the query did not match -- the directory a `/`
stepped into, or an ancestor the arrows walked onto -- has nothing to spell out and
is taken by the first press.

Moving the selection spells out the row it lands on the same way, so walking the
candidates reads as choosing between them. What narrows the rows is still only what
was typed: the candidate is spelled out *over* the query rather than into it, which
is what keeps the set being walked from collapsing to the one candidate showing.
Editing adopts what is on screen, so a backspace after walking onto `june.txt`
leaves `june.tx` rather than the query that found it. `Tab` and `Enter` therefore
complete to different things on purpose: `Tab` to what every match agrees on,
`Enter` to the one match selected.

Matching is a substring rather than a fuzzy subsequence so that completion has
something to complete to: a subsequence match shares no prefix with what was
typed, leaving `Tab` nothing to agree on.

A second `/` steps into the selected directory: it is expanded inline if it was
closed, the query is spent getting there, and matching is confined to what lies
under it, so an empty query then reads as everything inside. The root never moves
-- this opens a directory rather than navigating to one, leaving the tab, its
path, and everything outside where they were -- and the prompt shows the segments
committed so far, so `/src/tui/` reads as the place the next query applies. A name
can never contain a separator, so the key is spent on this rather than on a query
that could only ever match nothing.

Only a directory the prompt names outright is opened. A query that has merely
narrowed onto one is narrowing onto its neighbours just as much, so guessing at it
would open the wrong directory as readily as the right one; `Tab`, `Enter`, or
`Ctrl-N`/`Ctrl-P` are what name it first. A `/` that opens nothing says why on the
prompt line -- `no exact match to open`, or `not a directory` for a name that is
neither -- until the next keystroke clears it, so the key never fails silently.

Backspace walks the prompt back one keystroke at a time, including over each `/`:
across a query it deletes characters, across the `/` that entered a directory it
steps back out, closing the directory again and restoring the query that entered
it, and across the `/` that opened the prompt it closes the prompt as `Esc` would.
A directory the prompt opened is part of what the prompt did, so abandoning closes
it while committing keeps it; one that was already expanded is not, and stays open
either way.

The prompt takes the letters, so walking and scrolling the matches without leaving
it belongs to the arrows and to the chords a prompt is expected to answer:
`Ctrl-N`/`Ctrl-P` move the selection and `Ctrl-D`/`Ctrl-U` scroll it by half a
page.

The query, the candidate spelled out over it, the directories a `/` has stepped
into, and the row to restore all
belong to the Browser Context whose rows they narrow, not to the overlay stack, so
`InputMode::Filter` is derived from the active Context rather than stored beside
it. The restore point is a path rather than an index, because rows inserted while
the prompt is open -- by a streaming listing, or by the expansion a `/` starts --
would drift an index out from under it. Filtering never removes rows: expansion, streaming
insertion, and collapse all address rows by index, so a filter that dropped them
would move every index out from under an in-flight listing. It is a window
instead, and the only state it moves is the selection and the scroll offset,
which are positions in that window rather than row indices. The window is
recomputed per keystroke and per frame drawn while filtering rather than cached,
which costs one pass over the tree -- the same order as the matching itself --
and leaves no invalidation for row mutation elsewhere to remember. An unfiltered
browser pays nothing.

The prompt takes a line of its own directly above the Files pane, and no line at
all while it is closed. Matches then read downwards from the query with the first
of them against the list's top edge, which is the `fzf --layout=reverse` shape:
the eye stays at the top where the typing is, and rows keep tree order, so an
expanded directory still sits above the children indented under it.

Stacking the matches against the bottom of the pane instead, the way `fzf` does by
default, was tried and dropped. It anchors the eye only until the matches overflow
the pane, and keeping the selection against the prompt past that point requires
reversing the rows, which puts a directory below its own contents.

`y` copies the selected regular file into an App-owned lazy clipboard containing
only source connection identity, full path, and original name. Closing the
source Browser Context does not clear it; closing its connection does. `p` streams that
file into the active Context root, including local/remote upload, download, and
relay routes. Only one transfer runs at a time. The footer shows an
indeterminate `Copying…` state and the final byte count.

Paste initially fails if the destination name exists. `AlreadyExists` opens a
capturing overwrite confirmation: `Enter`/`y` restarts from byte zero with
replace semantics, while `q`/`Esc` cancels. Successful paste retains the
clipboard and refreshes the original destination Context only when it still
exists with the same connection and root. Switching tabs or closing that Context
does not cancel the transfer.

## Connection Manager and dialogs

`S` opens Connection Manager. It lists connections in creation order with label,
target summary, lifecycle state, and browser count. `j`/`k` or arrows move
selection while preserving connection identity across row changes.

- `a` opens the Local/SSH chooser. Local starts immediately; SSH accepts an
  unmodified printable destination.
- `Enter` on a ready connection activates its most recently active Context, or
  opens New Browser at `~` when it has none.
- `n` opens New Browser for the selected ready connection.
- `d` removes a failed row that never opened, or asks to close a ready one.
- `q`, `Esc`, or `S` dismisses the manager without quitting.

A confirmed close changes the row to `Closing`, drops all of its Browser
Contexts, and starts `Client::close` without blocking input. Completion
removes the row. Since Client removal precedes backend shutdown, a close error
is terminal and is retained in the application footer rather than exposed as a
fake retry.

Input routing uses one explicit mode with modal confirmation, chooser/text
dialogs, Connection Manager, then main browser/empty state precedence. Undefined
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
