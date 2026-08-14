# TUI layout and palette system: implementation contract

[tui-layouts.md](tui-layouts.md) explains the idea and
[tui-layouts.html](tui-layouts.html) shows every combination. This document is
the part an implementer needs: the seams, the exact rules, where the values
live, and what may not change on the way in.

Read the gallery first. Every rule below has a rendered example there, and the
HTML is the authoritative source for anything numeric this document does not
restate.

## Status

**Future possibility.** None of this is implemented. `crate::tui` renders one
layout with colors written into `app/view.rs`; [TUI implementation](../tui.md)
owns what it does today.

## What must not change

The galleries change how the TUI is arranged and colored. They change nothing
about what it does, and an implementation that quietly alters any of the
following has gone wrong.

- **Key bindings.** `src/tui/mod.rs` already maps `j`/`k`, `h`, `l`, `o`, `R`,
  `[`, `]`, `n`, `x`, `S`, `y`, `p`, `q`/`Esc`, arrows, and the `Ctrl-` set, plus
  the whole filter mode (`Tab`, `/`, `Ctrl-W`, `Ctrl-N`/`P`, `Ctrl-D`/`U`,
  `Enter`, `Esc`). Layouts render hints for these; they do not add, remove or
  rebind any of them. Note that today's footer lists only a subset -- `j`, `k`,
  `h`, `l`, `o` and `R` are bound but never shown -- and the new layouts are
  expected to show the real set.
- **Connection states.** `Starting`, `Ready`, `Failed(error)`, `Closing`, from
  `tui::app::connection::ConnectionStatus`. A layout may not invent a fifth.
- **Listing and expansion states.** `ListingStatus::{Loading, Complete,
  Failed}` and `ExpansionStatus::{Collapsed, Loading, Expanded, Failed}`. The
  glyphs `▸ ▾ … !` are the expansion states and stay one-to-one with them.
- **Entry kinds.** `EntryKind::{File, Directory, Symlink, Other}`, glyphs
  `(space) ▸ @ ?`.
- **Streaming.** Listings arrive in groups and a frame draws whatever has landed;
  no layout may require a complete listing before it draws.
- **Viewport ownership.** `BrowserContext::viewport` decides which rows a frame
  shows. Layouts consume that window; they do not scroll on their own.
- **Row order.** A listing follows the host's iteration order and is never
  sorted. No layout may sort for presentation.

## The two seams

```rust
/// Named roles rather than named colours, the rule `crate::gui::theme` already
/// follows, extended with what a terminal needs.
pub struct Palette { /* the 22 roles below */ }

pub trait Layout {
    /// The smallest terminal this layout can be drawn in.
    fn min(&self) -> Size;
    /// What survives when it is folded.
    fn grammar(&self) -> Grammar;
    fn render(&self, frame: &mut Frame<'_>, app: &mut App, palette: &Palette);
}
```

A layout names roles; a palette supplies them. Neither names anything the other
knows. That is the whole reason ten and ten make one hundred instead of one
hundred hand-drawn screens, and it is the one invariant to protect: **no layout
may reach for a literal color, and no palette may know a layout exists.**

## Part 1: The palette

Twenty-two roles. Every palette supplies all of them; a palette that declines to
distinguish two roles gives them the same value, which is a decision, not a gap.

| Role | Drawn as |
| --- | --- |
| `bg` | the window ground, behind everything |
| `rail` | sidebar ground, one step from `bg` |
| `surf` | the ground a list or pane sits on |
| `band` | alternating rows, and hover or raised states |
| `sel` | the selected row's ground |
| `on_sel` | the selected row's text |
| `on_sel_dim` | secondary text on the selected row (sizes, counts) |
| `chip` | inverted block: title bars, column headers, key caps |
| `on_chip` | text on a chip |
| `line` | structural rules and box frames |
| `hair` | faint rules, indent guides, dotted leaders |
| `text` | body text |
| `strong` | emphasis: headings, the active pane, a directory name |
| `dim` | secondary: sizes, counts, targets |
| `faint` | tertiary: labels, disabled, decoration |
| `accent` | "here" and "yours": active tab, selection marker, prompt, progress |
| `accent2` | second hue, carrying directories |
| `good` | `Ready`, a completed transfer |
| `warn` | in flight: `Starting`, a listing still arriving |
| `bad` | `Failed`, a refused expansion, an error line |
| `link` | `EntryKind::Symlink` |
| `other` | `EntryKind::Other` |

### Resolution

Authoring twenty-two values twice per palette invites drift, so a palette
declares what it distinguishes and the rest resolve:

```text
surf       <- bg          rail   <- bg          band       <- rail
hair       <- faint       strong <- text        accent2    <- accent
link       <- accent2     other  <- warn        on_sel_dim <- on_sel
```

Anything else is required. Resolution happens once, at load; nothing downstream
sees an unresolved palette.

### Where the values live

The ten palettes, in both modes, are in `tui-layouts.html` as the `PALETTES`
table. That file is the source of truth -- do not retype 440 hex values into
Rust by hand. Extract them:

```bash
node -e '
const fs = require("fs");
const html = fs.readFileSync("docs/design/tui-layouts.html", "utf8");
const js = html.split("<script>")[1].split("</scr" + "ipt>")[0];
const src = js.slice(js.indexOf("const ROLES"),
                     js.indexOf("/* ====", js.indexOf("const PALETTES")));
const out = {};
eval(src + "; PALETTES.forEach(p => { out[p.id] = { dark: p.dark, light: p.light }; });");
console.log(JSON.stringify(out, null, 2));
'
```

Each palette also carries `id`, a display `name`, a `note` on its character and
a `from` naming where it comes from. Those are user-facing strings and belong in
the implementation too, so `--help` and any picker can explain a choice.

### Contrast floor

Every informational pair reaches 3:1 in both modes: `text` on each ground,
`on_sel` on `sel`, `on_chip` on `chip`, `dim` on `bg` and `rail`, and each state
color on `bg`. `faint` is exempt by role -- it is decoration, and sits at
2.0--2.5:1 on purpose. Two known exceptions are recorded rather than fixed:
Solarized light holds its own `accent2` and `good` at 2.9:1, because moving them
would stop it being Solarized.

Any new palette is checked against this floor before it ships.

## Part 2: The layouts

Ten, each drawn at 120x34 in the gallery. The painter named in the last column
is the exact reference; read it for coordinates rather than re-deriving them.

| `id` | Minimum | Sidebar | Body | Painter |
| --- | --- | --- | --- | --- |
| `commander` | 120x30 | 28, double frame | two 46-wide panes, source and destination | `commander()` |
| `cockpit` | 120x30 | 32, round frame | 52 tree + 36 preview | `cockpit()` |
| `plain` | 110x26 | 28, fill only, no frame | one list, four columns | `plain()` |
| `sheet` | 120x30 | 28, dashed legend | ruled schedule with dotted leaders | `sheet()` |
| `board` | 120x30 | 24, rule only | full-width table, status column | `board()` |
| `menu` | 118x30 | 26, numbered menu | one list; footer is a command line | `menu()` |
| `columns` | 120x28 | 22, rule only | three cascading columns | `columns()` |
| `logbook` | 120x30 | 30, fill, gauges | tree over an event log | `logbook()` |
| `prompt` | 118x28 | 18, labels only | prompt-first, matches read downward | `promptLayout()` |
| `manifest` | 118x30 | 26, tab markers | numbered, ruled, banded table, totals row | `manifest()` |

Every layout's sidebar carries the same four things in its own grammar:
connections with their state, the tabs under each, Places, and the clipboard and
transfer. That content is fixed; only its treatment varies. Two consequences:

- `S` currently opens a modal connection manager. With a permanent sidebar the
  modal is redundant for *viewing*, but it still owns *acting* (`a` add, `d`
  close, `Enter` activate). Deciding whether `S` focuses the sidebar or keeps
  opening the modal is listed under decisions still owed; do not settle it
  silently.
- Overlays (`NewBrowser`, `RemoteInput`, the two confirmations) are unchanged and
  draw over any layout. They are not part of the layout seam.

## Part 3: Grammar

What survives folding. Seven fields, all data:

| Field | Values | Effect when folded |
| --- | --- | --- |
| `frame` | `none` \| `single` \| `round` \| `double` \| `dash` \| `rule` | `rule` draws a heavy rule under the column header; `double` also makes the top bar a chip |
| `caps` | bool | chrome, column headers and hints in upper case; **never** entry names |
| `band` | bool | alternate row grounds |
| `leaders` | bool | dotted leaders from name to size |
| `numbered` | bool | a line-number column |
| `header` | `none` \| `plain` \| `chip` | column header treatment |
| `footer` | `plain` \| `chips` \| `command` | key hints, key caps, or a filter command line |

`caps` applies to chrome only. Filenames are data and are never re-cased.

## Part 4: The compact renderer

One function serves every rung below Full. Exact rules, all verified in the
gallery:

**Breakpoints.** `size` column at width >= 52. `kind` column at width >= 76.
Everything optional by height (`roomy`) at height >= 20.

**Rows, top to bottom.** Top bar (always 1). Tab strip (1, only when `roomy`).
Column header (1 when `grammar.header != none` and `roomy`; plus 1 more when
`frame == rule`). Body (the rest). Status bar (1, only when `roomy`). Hint row
(always 1).

**The top bar** carries the app name, the path, and at width >= 44 the tab
position and item count. The path elides **from the left** with a leading `…`:
the tail locates you, the head does not.

**The body** keeps the selection on screen. The window starts at
`clamp(selected - rows / 2, 0, len - rows)`, not at zero. When the listing
overflows, one row is reserved for a marker showing both directions (`▴ n` above,
`▾ n more` below); the marker never overwrites an entry.

**The name column** gets whatever is left after the optional columns, and elides
with `…` at its own end.

**Hints** are spent in priority order and stop when the row runs out, ending in
`…`: `j k` move, `l h` open, `/` filter, `y p` copy, `o` expand, `n x` tab, `S`
conns, `q` quit.

**Minimal** is this same function with `Grammar::BARE` -- every switch off. It is
not a separate renderer, and there is no eleventh layout.

## Part 5: Choosing the rung

```rust
match (area, layout.min()) {
    _ if area >= layout.min()      => Rung::Full,     // the layout itself
    _ if area >= Size::new(64, 16) => Rung::Compact,  // fold, keep the grammar
    _                              => Rung::Minimal,  // fold, drop the grammar
}
```

Both dimensions must clear the bar; a wide, short terminal folds like a narrow
one.

## Part 6: Configuration

```toml
[ui]
layout  = "board"
palette = "amber"
mode    = "dark"    # dark | light
```

- An unknown `layout` or `palette` name is a startup error naming the value and
  listing what is valid. It does not silently fall back -- a user who typo'd a
  palette wants to be told, not handed the default.
- Both axes are independent. Every one of the hundred combinations is valid,
  including the ones the gallery warns about; a warning is information, not a
  veto.
- `mode` has no `system` value yet. See decisions still owed.

## Part 7: Where the code goes

`crate::tui` owns rendering, so this stays inside it. `app/view.rs` today is one
layout's render plus its helpers; the natural split follows the seams:

```text
src/tui/
├── palette.rs          the 22 roles, the resolution rule, the ten palettes
├── layout/
│   ├── mod.rs          the Layout trait, Grammar, Rung, name lookup
│   ├── compact.rs      the one responsive renderer
│   └── <one per layout>
```

Today's rendering is none of the ten: it is a single bordered Files pane with a
tab strip and a modal connection manager. Step 3 below keeps it as the first
`Layout` implementation rather than retrofitting it into one of the ten, so the
seam lands before any redesign does.

Per [Code structure](../code-structure.md), `tui` may depend on the crate-root
facade and `browser`, and nothing new is needed here: layouts read `App` and
`BrowserContext` and write to a `Frame`. Keep `tui/mod.rs`'s key mapping pure and
untouched -- input routing is not part of this change.

## Part 8: Build order

Each step is independently reviewable and leaves the TUI working.

1. **Palette only.** Add `Palette`, resolution, and one palette matching today's
   colors. Route `view.rs` through it. Nothing changes on screen, which is how
   you know it is right.
2. **The other nine palettes**, extracted from the HTML, plus config parsing for
   `palette` and `mode`. Now one axis is real.
3. **The seam.** Extract `Layout` and make today's rendering the first
   implementation. Still one layout, but selectable in principle.
4. **Compact and the rungs.** Add `Grammar`, `compact`, rung selection, and give
   the one layout its `min` and `grammar`. Small terminals stop being broken,
   which is a fix worth having on its own.
5. **The remaining nine layouts**, one commit each, each against the gallery.

## Part 9: How to verify

- **Every palette supplies every role**, after resolution. A table test over the
  ten, both modes, asserting all twenty-two are set.
- **The contrast floor** above, as a test over the same table.
- **No literal colors in layout modules.** A grep in CI is enough:
  layout code that names a color has broken the seam.
- **The cell grid.** Every glyph a layout draws is one cell wide. No Private Use
  Area codepoints (no Nerd Font), no emoji, no double-width characters. `⚙` was
  already dropped for this reason, and `⌂`, `❯`, `✕`, `▰`, `▱` are the remaining
  glyphs worth re-checking on a target terminal.
- **Every rung at every size.** Render each layout at 120x34, 100x30, 80x24,
  64x20 and 46x14 in both modes and assert nothing panics and no row exceeds the
  width. `TestBackend` already does this in `view.rs`'s tests.
- **The existing view tests still pass**, in particular that the filter prompt
  sits directly above its matches and that an armed quit takes the footer. Those
  describe behavior, not decoration, and every layout owes them.

## Decisions still owed

Do not settle these while implementing. They are the user's.

- Whether `S` focuses the sidebar or keeps opening the connection-manager modal.
- Whether `mode = "system"` exists. A terminal reports no theme preference, so
  the honest options are an explicit setting or a heuristic over `COLORFGBG` and
  terminal-specific escapes.
- Whether a palette can be overridden role by role, and where the config file
  lives.
- Whether crossing a rung boundary mid-resize is silent or announced. A layout
  vanishing when a window edge is dragged reads as a bug.
- Whether a layout may declare its own narrow form when the shared compact
  renderer serves it badly. `columns` folds to nearly the same thing as `plain`,
  which is the weakest of the ten.
- Whether a `glyphs` axis (`ascii` / `unicode` / `nerd`) is added later. It was
  considered and deferred; nothing here forecloses it.
