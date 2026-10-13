# TODO

File Peeker's tracked work list. Everything outstanding lives here; the
documents under [docs/](old/docs/README.md) describe what is built, not what is
planned.

The list is currently dominated by one theme: the two layout and palette
galleries — [TUI](docs/design/tui-layouts.md) and
[GUI](docs/design/gui-layouts.md) — draw states this application cannot enter.
The GUI gallery's own fixture asks for three connections including a refused one,
a directory whose expansion was refused, another still listing, and one transfer
in flight ([GUI layout and palette system](docs/design/gui-layout-spec.md),
section 11). The desktop front end holds one connection and has no transfer.

So the work that comes before a new front end is not arrangement and color. It is
the entry model, the input model, and the operations the layouts assume already
exist. **The entry model is now settled** — items 1 to 3 are done, so an entry has a
size and a modification time, and a failure belongs to the entry it happened to
rather than to the whole listing. What is left in this tier is the tree that holds
those entries: how it stores its rows, and what order it puts them in. Beyond it are
the input model, a second connection, and the operations.

**Most of what remains is now built in v2 rather than here.** Item 8 rewrites
File Peeker as a second workspace crate — a persistent server per machine with a
task runtime as its top layer, front ends as clients — with no compatibility with
this one. The current crate takes bug fixes only. An item whose shape v2 changes is
built once, there, at the step that owns its area; the route below says which.

## Status

This is a **work list**, not current behavior. Each item names what is true in
the code today and what the proposals ask for. The proposals themselves stay
future possibilities until their own documents say otherwise.

Ordering rule: an item whose shape reaches the wire protocol or the shared tree
model comes before an item that only reaches one front end, because the first
kind gets more expensive the later it lands.

## The list

Legend: `[ ]` open, `[~]` in progress, `[x]` done.

### Tier 1 — data shape

Landing these late means reworking every layout that reads them.

- [x] **1. Report listing failures per entry, not per stream.**
  [`inspect_entry`](old/src/fs/directory/entry.rs) is now infallible: an entry it
  cannot describe is reported with `metadata: None` and the reason in `defect`.
  The stream item type did not change; what changed is which conditions produce
  `Err`, now only iterating the directory failing and cancellation. `browser::Row`
  keeps it on the `DirectoryEntry` it already held, so neither front end needed a
  new field and no placement invariant was touched. A degraded row draws as
  `unknown` in the desktop table and `×` in the terminal.
- [x] **2. Tolerate non-UTF-8 names and paths.**
  [`host_path`](old/src/host_path.rs) carries them through an invertible escape —
  `%%` for a literal marker, `%XX` for a byte outside UTF-8 — so a protobuf
  `string` holds any name without losing a byte and the name still addresses its
  file. The `HostPath` newtype keeps the escaped and displayed forms from being
  confused. Five separate refusals went away, including two in `resolve_path` and
  `write_file` that the item did not know about.
- [x] **3. Give an entry its metadata: size, modified time, mode, symlink
  target.** `EntryMetadata` carries all four, and one `symlink_metadata` call
  yields the file type with them rather than a second stat beside
  `DirEntry::file_type`. A symlink reports **its own** metadata plus its target,
  as `lstat` and `ls -l` do, so a dangling link keeps its metadata and stays
  distinguishable from an entry that could not be stat'd. That stat is also what
  made item 1 necessary rather than merely tidy: it is a per-entry syscall that
  can be refused on its own. The desktop table gained `Size` and `Modified`
  columns; the terminal shows a size above 52 columns.
- [ ] **4. Sort a listing.** *Unblocked: 3 is done.* Chosen: append while a listing
  streams, sort when it finishes. `Placement` caches row indices and its fixups only
  add and subtract, while a sort permutes variable-length blocks — a nested expansion
  interleaves deeper rows among the siblings — so each block moves rigidly and an
  index inside one maps by `new = old + delta`. Open: the collation, where the escaped
  form sorts `%` at `0x25`, the displayed form is not injective, and the real bytes
  cost a decode per key; then case, natural order, whether directories group first,
  where an absent size sorts, and the tie-break. Selection and scroll offset are both
  indices a permutation invalidates. Both layout specs say a listing is never sorted,
  so one of them has to be amended.
- [ ] **5. Stop moving every row below an expansion on every group.** Chosen: split
  the tail off at `begin_expansion`, append each group, merge at `finish`, which is
  `2T` rather than `m·T`. `extend_child` splices into one flat `Vec<Row>` today, so a
  100k-entry directory expanded above a 100k-row tail is 195 groups over 100k rows,
  near 7.5 GB and a quarter of a second of dropped frames; the trigger is the product,
  so both have to be large. It costs the `rows() -> &[Row]` shape, though every caller
  uses only `len`, `get`, `iter` and `position`, and several expansions in flight mean
  several segments. A `Placement` holding a segment and an offset then never moves
  when another grows, so `placements_after_insertion` goes away. It does not settle 4;
  the far end is one segment per directory, a nested tree, worth it once a second
  customer for the tree shape appears (item 13).

### Tier 2 — what carries a new front end

None is blocked. Each is cheaper once, before ten layouts, than ten times
after.

- [ ] **6. Give the desktop front end a keyboard and a focus model.**
  `crate::gui` handles no key at all, which is why its filter field is always on
  screen rather than opened by a shortcut. The proposals include a command
  palette layout, selection movement, filtering, and rungs where the pointer
  loses controls. Needs focus ownership, a binding table aligned with the TUI's
  keys, and a rule for competing with `egui`'s own text-field capture.
- [ ] **7. Read and persist configuration.** The GUI proposal selects a layout, a
  palette and a mode from `[ui]` in a config file with per-role overrides.
  Nothing in the crate reads configuration today. Without persistence a hundred
  combinations reset to the default on every start. Decide the file location, the
  handling of missing and invalid values, and whether both front ends share one
  file.
- [~] **8. Rewrite as v2: a persistent server per machine, run by tasks.**
  Proposed in [v2 architecture](docs/design/v2-architecture.md), with every
  decision defaulting to how [herdr](https://herdr.dev) does it. Each machine runs
  one persistent server that owns its tasks and its browsing sessions — tabs,
  trees, selection — and keeps running with no client attached; front ends attach
  over a unix socket or an SSH bridge, render views the server supplies, and send
  actions. Servers never talk to each other; the client aggregates them. The
  server's top layer is a task runtime: every asynchronous, fallible operation is
  a task kind in a structured tree, and filesystem access is one family of kinds.
  A public JSON API sits beside the private client protocol, and the CLI is a
  client of the API. Built at the repository root in fourteen steps, 0 to 13,
  ending in cutover. Step 1 is an empty server — started, found, answering and stopped,
  with nothing inside — step 2 an empty TUI client attached to it, and step 6 a
  local walking skeleton. The binary is
  `fpk` until cutover. Open: whether round-tripped input needs an optimistic
  cursor over SSH.
  Step 0 is done: v2 is the package `file-peeker` at the repository root, and the
  shipping crate is `file-peeker-old` in `old/`, a workspace of its own; `make
  verify` and CI cover both. Step 1 is done: `fpk` starts, finds, attaches to and
  stops a detached per-user server.

### Tier 3 — states the proposals draw

- [ ] **9. Let the desktop front end hold several connections.** It holds one
  `ConnectionId`, and its rail and status bar name it with literal strings. The
  proposal's rail draws a connection list with three status colors and expects
  transfers between panes. The TUI already has a Connection Manager and tabs, so
  this is lifting a proven model into shared code rather than inventing one.
- [ ] **10. Report transfer progress, queue transfers, and cancel them.**
  *Unblocked: 3 is done, so the source size a determinate bar needs is available.*
  [`Client::op_copy_file`](old/src/client/file/copy.rs) is a
  single await returning a final byte count. The proposal draws a transfer track
  and an inspector queue; a determinate bar also needs the source size.
- [ ] **11. Expose a bounded, cancellable file read.** `read_file` is private and
  serves copying only. The Miller preview pane and the gallery's thumbnails need
  a prefix read with a size ceiling. Decide the signature, binary detection, and
  whether thumbnails require image decoding and its dependency.
- [ ] **12. Add refresh and content invalidation.** The desktop front end has no
  refresh; the TUI has `R`. Neither watches the filesystem. The more panes a
  layout shows at once, the more visible stale content becomes. Manual refresh
  first, preserving selection and expansion; then decide about watching.

### Tier 4 — decisions, not implementations

Each is cheap to settle now and expensive to settle after a hundred combinations
exist.

- [ ] **13. Decide whether the treemap's recursive directory sizes are worth it.**
  *Unblocked: 3 is done, so a walk entry now carries a size.* `op_walk_dir` still has
  no aggregation and no cache, and walking a large tree has a real cost and a cancellation story.
  Either compute lazily with a cache, or drop the layout.
- [ ] **14. Decide the accessibility cost of painting text.** `crate::gui` draws
  every string through `Painter::text` rather than as a widget, so there is no
  accessibility node, no selectable text and no keyboard traversal. The GUI
  proposal's component inventory extends that approach. Either accept it, or give
  the list row semantics.
- [ ] **15. Decide whether the dock or the layout seam owns pane arrangement.**
  The GUI proposal already raises this: four of its layouts place panes in ways
  [`gui::dock`](old/src/gui/dock.rs) owns today, and a layout seam fighting the
  dock is worse than no layout seam.

### Tier 5 — safe to start today

None of these touch the data layer.

- [ ] **16. Widen `Palette` with `sel`, `on_sel`, `bad` and `shadow`.**
  [`gui::theme`](old/src/gui/theme.rs) derives the selection ground from the
  accent, which cannot be tuned per palette and disappears in the darker ones;
  the shadow is a constant chosen by mode; and `warn` means both a refused
  expansion and a dead connection. This is the GUI proposal's first build phase.
- [ ] **17. Make the palette contrast floors executable.** *Blocked by 16.* The
  GUI proposal states contrast floors and a full set of acceptance checks as
  prose. Ten palettes in two modes cannot be reviewed by eye.
- [ ] **18. Abort the listing task when the desktop front end collapses a row.**
  `Tree::collapse` returns the request IDs whose tasks are no longer wanted;
  `crate::gui` discards them, so a collapsed large directory keeps streaming to
  completion. The TUI does this correctly. Item 8 retires this by construction;
  fixing it here first is a few lines that item 8 then deletes.
- [ ] **19. Give the loading bar a visible track.** `gui::dock` passes `surface`
  as the track color onto a `surface` panel, so the accent segment slides through
  nothing. The GUI proposal's fix is the accent at reduced alpha, separating by
  hue as the status dot's halo already does, rather than another ground role.

## Dependency summary

| Item | Blocked by |
| --- | --- |
| 17 — contrast checks | 16 |

Nothing else is blocked. Every item from 4 onward except the palette and drawing
items is now built in v2 rather than against the current crate.

## Suggested route

1. ~~Items 1, 2 and 3: the entry model settles before anything reads it.~~ Done.
2. Item 8, through the [v2 build order](docs/design/v2-architecture.md#build-order).
   The current crate keeps shipping and takes bug fixes only.
3. The other items land inside that order, designed in rather than retrofitted:

   | Item | v2 step |
   | --- | --- |
   | 4 and 5 — sorting and segmented rows | 5, sessions on the server |
   | 7 — configuration | 6, client-side `config.toml`; the server reads no UI settings |
   | 9 — several connections | 9, machines |
   | 10 — transfer progress and queue | 10, copies |
   | 11, 12, 13 — preview read, refresh, directory sizes | 11 |
   | 6 — desktop keyboard and focus | 12, the desktop client |
   | 18 — the collapse leak | 5, by construction: a collapse cancels on the server |

4. The palette and drawing items — 14 to 17 and 19 — belong to the desktop client,
   step 12. The current GUI is replaced rather than ported, so work on them there
   would be thrown away.
5. Item 13's treemap decision, and items 14 and 15, answered before step 12 builds
   the desktop client's layouts.
