# GUI layout and palette gallery

[gui-layouts.html](gui-layouts.html) draws ten desktop layouts against ten
palettes, one hundred combinations, in a browser. Open the file; it carries its
own renderer, needs no server, no build and no desktop session, and makes no
network request.

```text
open docs/design/gui-layouts.html
```

`j`/`k` change layout, `h`/`l` change palette, `d` changes dark or light, and
`[`/`]` change window size. The URL fragment names the whole combination, so
`#miller/cyanotype/dark/compact` opens that one directly and a combination can be
sent to someone else.

There are five views: one window at its real point size; either axis as a strip
of ten; the window-size ladder; and all one hundred at once, layouts down and
palettes across. At overview scale a window is composition and color and nothing
else, which is the question that view is asked; clicking a cell opens it.

Building it is a separate document: [GUI layout and palette
system](gui-layout-spec.md) carries the types, the component metrics, the palette
values and the build order.

## Status

This gallery is a **future possibility**, not current behavior. The desktop
front end has one layout and one palette today, in `crate::gui`. It appears in
the gallery as the layout named 分頁塢, which is drawn from what `app.rs`,
`dock.rs` and `theme.rs` currently do.

## What is being shown

Every screen is a window measured in egui's logical points, and nothing appears
in one that `eframe`'s `Painter` could not paint:

| On the page | The call it stands for |
| --- | --- |
| filled rectangle, one corner radius | `Painter::rect_filled` |
| 1px hairline | `glyph::edge`, a `Stroke` of 1.0 |
| stroked segments | `Painter::line_segment` |
| polygon | `Shape::Path` |
| circle | `Painter::circle_filled` |
| text at a point, with an alignment | `Painter::text` with a `FontId` and an `Align2` |
| one shadow, with an offset and a blur | `epaint::Shadow` |
| a color at reduced strength | `Color32::gamma_multiply` |

There are no gradients, no backdrop blur and no effect that exists only in CSS.
Icons are built from those primitives the way `crate::gui::glyph` builds them,
never typed as font characters, so the tab's close control and the two-pane
gutter's arrows are drawn rather than borrowed from a typeface.

The metrics are the current ones rather than invented ones: 26pt rows, 15pt
indent, a 44pt titlebar, a 216pt sidebar, a 40pt filter row, a 24pt column
header, a 30pt pane footer and a 28pt status bar.

Content is identical across all one hundred, so only the design varies: three
connections, one local and two SSH with one of them refused; this repository's
own tree, including a directory whose expansion was refused and one still
listing; a real `/var/log` listing on the remote host, which only the two-pane
layout has room to show; and one transfer in flight.

## The seam

A layout names roles. A palette supplies colors for them.

| Group | Roles |
| --- | --- |
| Grounds | `base`, `sunken`, `surface`, `raised` |
| Selection | `sel`, `on_sel` |
| Rules | `border` |
| Text tiers | `text`, `muted`, `faint` |
| Identity | `accent`, `folder` |
| State | `good`, `warn`, `bad` |
| Entry kinds | `kind_doc`, `kind_media` |
| Elevation | `shadow` |

Fourteen of these are already in `crate::gui::theme::Palette`. Four are not, and
each is something a desktop has and a terminal does not:

- `sel` and `on_sel` make the selected row's ground a color the palette chose,
  rather than today's `accent.gamma_multiply(0.30)`, which is a formula that
  cannot be tuned per palette.
- `bad` separates "this is broken" from "look at this". Today both reach for
  `warn`, so a refused expansion and a refused connection are the same color.
- `shadow` moves elevation strength into the palette. It is currently a constant
  chosen by dark or light, which is why a palette that wants to be flat has no
  way to say so.

Drawing the gallery also turned up one thing that is a bug rather than a missing
role. `dock.rs` passes `palette.surface` as the loading bar's track colour onto a
panel that is already `palette.surface`, so the bar has no track: the accent
segment slides through empty space with no start and no end. The gallery draws
that track as `raised`, which is what the role set already provides.

## What a palette declines to distinguish

A palette that will not tell two roles apart simply gives them the same color,
or close enough that the eye cannot separate them. The consequence is real
rather than hidden, and the gallery computes it from the colors themselves and
prints it above any combination where it applies:

- 示波 makes `accent` and `good` one color, because on an instrument "there is a
  signal" and "this is fine" have always been one color. The cost is that a
  transfer in progress and a transfer that finished look alike.
- 銅綠 commits to two hues, so directories and symlinks are told apart by icon
  shape and not by color. A layout carrying a kind column survives that; one
  without a kind column does not.
- 孔版 sets `shadow` almost to nothing, because screen-printed ink does not cast
  one. Layouts that lean on elevation lose their separation and have to fall
  back on the border hairline.

These are the palettes being honest about their own commitments, not defects to
fix. The point of computing the note rather than writing it by hand is that it
cannot go stale when a color changes.

## Degrading with the window

Each layout declares the smallest window it can be drawn in, and below that the
gallery falls to one shared responsive renderer. There are three rungs.

| Rung | Window | What is drawn |
| --- | --- | --- |
| Full | 1280x800 | every layout in its own form |
| Compact | 1000x680 | eight of the ten fall back |
| Minimal | 760x560 | the same renderer with the grammar switched off |

A folded layout is not a different product. The compact renderer keeps the
grammar its layout declares -- whether it has a rail and of what kind, whether
its chrome is a titlebar or a toolbar, whether content sits on a floating card
or runs full bleed, which extra columns it asks for, whether its footer is a
status bar or a per-pane path -- and loses only the panels there is no room for.
The rail is the first to go, from a 200pt panel to a 56pt strip of connection
dots to nothing; Places lose their labels before the connections lose theirs,
because which host you are on outranks which folder you are in. Columns are
spent in priority order, and `Status` is the last one dropped, because it is
where a refused expansion says so.

Two of the ten hold their own form at every rung. 指令面板 is a centred overlay,
so it only has to grow narrower, and the app behind it degrades on its own.
大綱 is a single centred column whose skeleton is indentation, so it sheds its
status column at 600pt of column width and its size column at 520pt and is still
itself.

## Open questions

- Where configuration lives, and whether a palette can be overridden role by
  role rather than only chosen whole. The gallery's config panel shows the
  override syntax, but nothing reads it.
- Whether `mode = "system"` should follow the OS appearance. The desktop front
  end can answer this where the TUI cannot, but today it deliberately overwrites
  egui's own light and dark preference so the app cannot be pulled out from
  under itself.
- Whether the rung is recomputed on every resize, and whether a layout vanishing
  as the window edge is dragged reads as a feature or as a bug.
- Whether `egui_dock` can express more than one of these. Four of the ten
  (瀑布欄, 雙面板, 主控台, 面積圖) place panes in ways that are the dock's job
  to own, and a layout seam that fights the dock is worse than no seam.
- Whether the ten palettes should ship whole or whether some are demonstrations.
  孔版 and 示波 exist partly to prove the seam reports its own costs.
