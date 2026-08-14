# TUI layout and palette gallery

[tui-layouts.html](tui-layouts.html) draws ten TUI layouts against ten palettes,
one hundred combinations, in a browser. Open the file; it carries its own
renderer, needs no server, no build and no terminal, and makes no network
request.

```text
open docs/design/tui-layouts.html
```

`j`/`k` change layout, `h`/`l` change palette, `d` changes dark or light, and
`[`/`]` change terminal size. The URL fragment names the whole combination, so
`#board/gruvbox/dark/80x24` opens that one directly and a combination can be
sent to someone else.

## Status

This gallery is a **future possibility**, not current behavior. The TUI has one
layout and one hard-coded set of colors today; [TUI implementation](../tui.md)
owns what it actually renders.

## What is being shown

Every screen is a 120x34 grid of character cells, the same model as a Ratatui
`Buffer`: one glyph and one style per cell. Nothing appears that a terminal
could not draw. Box-drawing characters and colors only, no fractional spacing,
no glyph wider than one cell.

Content is identical across all one hundred, so only the design varies: three
connections, one local and two SSH with one of them failed; this repository's
own tree, including a directory whose expansion was refused and one still
listing; the real key bindings; and the four lifecycle states the TUI actually
has.

## The seam

A layout names roles. A palette supplies colors for them. Neither names anything
the other knows, which is what makes the two axes independent rather than one
hundred hand-drawn screens.

| Group | Roles |
| --- | --- |
| Grounds | `bg`, `rail`, `surf`, `band` |
| Selection | `sel`, `on_sel`, `on_sel_dim` |
| Inverted blocks | `chip`, `on_chip` |
| Rules | `line`, `hair` |
| Text tiers | `text`, `strong`, `dim`, `faint` |
| Identity | `accent`, `accent2` |
| State | `good`, `warn`, `bad` |
| Entry kinds | `link`, `other` |

This is the rule the desktop front end already follows in `crate::gui::theme`,
named roles rather than named colors, extended with what a terminal needs
(inverted blocks, hairlines, an explicit selection ground) and paired with a
layout seam so the arrangement becomes selectable too.

A palette that declines to distinguish two roles simply gives them the same
color. That is how the monochrome palette works, and the consequence is real
rather than hidden: with one hue, directories, symlinks and refused expansions
stop being told apart by color, and a layout that carries a kind column survives
it while a layout that does not carry one loses the distinction. The gallery
prints that consequence above any combination where it applies.

## Degrading with the terminal

None of the ten holds its shape at every size, so each declares the smallest
terminal it can be drawn in and the TUI falls to a smaller rung below that.
There are three rungs and two renderers.

| Rung | When | What is drawn |
| --- | --- | --- |
| Full | at or above the layout's declared minimum | the layout itself |
| Compact | 64x16 and above | one responsive renderer, carrying that layout's grammar |
| Minimal | below 64x16 | the same renderer with the grammar switched off |

A folded layout is not a different product. Compact keeps the grammar the layout
declares -- frame, letter case, row banding, dotted leaders, line numbering,
column-header treatment, footer style -- and loses only the panels there is no
room for, sidebar first, then any second pane. So `board` folded is still a
board: reversed column header, banded rows, upper-case chrome. `menu` folded
still ends in a command line. Minimal is that same renderer with every grammar
switch off, which is the floor every layout shares: one bar above, one list, one
bar below.

Both rungs below Full drop columns as the width runs out (kind below 76 columns,
size below 52), elide the header path from the left, spend key hints in priority
order until the row is full, and keep the selected row on screen by following it
rather than starting at row zero.

One renderer rather than ten narrow variants is the point: a layout that folds
badly is a grammar to fix in one place, not ten.

## Open questions

- Where configuration lives, and whether a palette can be overridden role by
  role rather than only chosen whole.
- Whether `mode = "system"` is answerable at all. A terminal reports no theme
  preference, so the honest options are an explicit setting or a heuristic over
  `COLORFGBG` and terminal-specific escapes.
- Whether the rung is recomputed on every resize, and whether crossing a
  boundary mid-session should be silent or announced. A layout vanishing on a
  drag of the window edge is the kind of change that reads as a bug.
- Whether a layout may declare a narrow form of its own when the shared compact
  renderer serves it badly. `columns` folds to something nearly identical to
  `plain`, which is the weakest result of the ten.
