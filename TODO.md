# TODO

File Peeker's tracked work list. Everything outstanding lives here; the
documents under [docs/](docs/README.md) describe what is built, not what is
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
rather than to the whole listing. What is left is the input model, a second
connection, and the operations.

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
  [`inspect_entry`](src/fs/directory/entry.rs) is now infallible: an entry it
  cannot describe is reported with `metadata: None` and the reason in `defect`.
  The stream item type did not change; what changed is which conditions produce
  `Err`, now only iterating the directory failing and cancellation. `browser::Row`
  keeps it on the `DirectoryEntry` it already held, so neither front end needed a
  new field and no placement invariant was touched. A degraded row draws as
  `unknown` in the desktop table and `×` in the terminal.
- [x] **2. Tolerate non-UTF-8 names and paths.**
  [`host_path`](src/host_path.rs) carries them through an invertible escape —
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
- [ ] **4. Decide which layer sorts, and let the tree reorder.** *Unblocked: 3 is
  done.* Note that paths and names are now escaped, and `Ord` on the escaped form is
  not the ordering of the real bytes — `%` is `0x25`, so an escaped high byte sorts
  before a letter. `HostPath` deliberately does not derive `Ord`; this item chooses
  whether to collate the displayed form or the real bytes.
  Listings arrive in `read_dir` order; [Client API](docs/api.md) promises stream
  order, not a collation. The difficulty is not the comparison: `Placement` in
  [`browser::tree`](src/browser/tree.rs) holds row indices and is only
  sound because a root listing appends, so any reorder disturbs the insertion
  point of an in-flight expansion. Sorting on the server costs the streaming;
  sorting on the client needs a reorder that preserves the placement invariant.
  Every layout with a column header depends on the answer.

### Tier 2 — what carries a new front end

Neither is blocked. Both are cheaper once, before ten layouts, than ten times
after.

- [ ] **5. Give the desktop front end a keyboard and a focus model.**
  `crate::gui` handles no key at all, which is why its filter field is always on
  screen rather than opened by a shortcut. The proposals include a command
  palette layout, selection movement, filtering, and rungs where the pointer
  loses controls. Needs focus ownership, a binding table aligned with the TUI's
  keys, and a rule for competing with `egui`'s own text-field capture.
- [ ] **6. Read and persist configuration.** The GUI proposal selects a layout, a
  palette and a mode from `[ui]` in a config file with per-role overrides.
  Nothing in the crate reads configuration today. Without persistence a hundred
  combinations reset to the default on every start. Decide the file location, the
  handling of missing and invalid values, and whether both front ends share one
  file.

### Tier 3 — states the proposals draw

- [ ] **7. Let the desktop front end hold several connections.** It holds one
  `ConnectionId`, and its rail and status bar name it with literal strings. The
  proposal's rail draws a connection list with three status colors and expects
  transfers between panes. The TUI already has a Connection Manager and tabs, so
  this is lifting a proven model into shared code rather than inventing one.
- [ ] **8. Report transfer progress, queue transfers, and cancel them.**
  *Unblocked: 3 is done, so the source size a determinate bar needs is available.*
  [`Client::op_copy_file`](src/client/file/copy.rs) is a
  single await returning a final byte count. The proposal draws a transfer track
  and an inspector queue; a determinate bar also needs the source size.
- [ ] **9. Expose a bounded, cancellable file read.** `read_file` is private and
  serves copying only. The Miller preview pane and the gallery's thumbnails need
  a prefix read with a size ceiling. Decide the signature, binary detection, and
  whether thumbnails require image decoding and its dependency.
- [ ] **10. Add refresh and content invalidation.** The desktop front end has no
  refresh; the TUI has `R`. Neither watches the filesystem. The more panes a
  layout shows at once, the more visible stale content becomes. Manual refresh
  first, preserving selection and expansion; then decide about watching.

### Tier 4 — decisions, not implementations

Each is cheap to settle now and expensive to settle after a hundred combinations
exist.

- [ ] **11. Decide whether the treemap's recursive directory sizes are worth it.**
  *Unblocked: 3 is done, so a walk entry now carries a size.* `op_walk_dir` still has
  no aggregation and no cache, and walking a large tree has a real cost and a cancellation story.
  Either compute lazily with a cache, or drop the layout.
- [ ] **12. Decide the accessibility cost of painting text.** `crate::gui` draws
  every string through `Painter::text` rather than as a widget, so there is no
  accessibility node, no selectable text and no keyboard traversal. The GUI
  proposal's component inventory extends that approach. Either accept it, or give
  the list row semantics.
- [ ] **13. Decide whether the dock or the layout seam owns pane arrangement.**
  The GUI proposal already raises this: four of its layouts place panes in ways
  [`gui::dock`](src/gui/dock.rs) owns today, and a layout seam fighting the
  dock is worse than no layout seam.

### Tier 5 — safe to start today

None of these touch the data layer.

- [ ] **14. Widen `Palette` with `sel`, `on_sel`, `bad` and `shadow`.**
  [`gui::theme`](src/gui/theme.rs) derives the selection ground from the
  accent, which cannot be tuned per palette and disappears in the darker ones;
  the shadow is a constant chosen by mode; and `warn` means both a refused
  expansion and a dead connection. This is the GUI proposal's first build phase.
- [ ] **15. Make the palette contrast floors executable.** *Blocked by 14.* The
  GUI proposal states contrast floors and a full set of acceptance checks as
  prose. Ten palettes in two modes cannot be reviewed by eye.
- [ ] **16. Abort the listing task when the desktop front end collapses a row.**
  `Tree::collapse` returns the request IDs whose tasks are no longer wanted;
  `crate::gui` discards them, so a collapsed large directory keeps streaming to
  completion. The TUI does this correctly.
- [ ] **17. Give the loading bar a visible track.** `gui::dock` passes `surface`
  as the track color onto a `surface` panel, so the accent segment slides through
  nothing. The GUI proposal's fix is the accent at reduced alpha, separating by
  hue as the status dot's halo already does, rather than another ground role.

## Dependency summary

| Item | Blocked by |
| --- | --- |
| 15 — contrast checks | 14 |

Nothing else is blocked. Items 4, 8 and 11 waited on the entry model and no longer
do.

## Suggested route

1. ~~Items 1, 2 and 3: the entry model settles before anything reads it.~~ Done.
2. Items 16 and 17, as one pass over the two defects the proposal already found.
3. Item 4, now that there is metadata to sort by — and a decision to make about
   whether a collation reads the displayed name or the real bytes.
4. Items 5 and 6, which are what a selectable layout and palette are carried on.
5. Items 7 through 10 in parallel with the GUI proposal's first three build
   phases — widening the palette, landing the ten, and extracting the
   components — since those phases do not read the entry model.
6. Items 11, 12 and 13 answered before the fourth build phase introduces
   `Layout`, because that is where the proposals begin to depend on them.
