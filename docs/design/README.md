# Layout and palette galleries

This directory holds two browsable proposals, one per frontend, for making a
frontend's arrangement and its colors selectable rather than fixed.

| Gallery | Document | Open |
| --- | --- | --- |
| TUI | [TUI layout and palette gallery](tui-layouts.md) | `open docs/design/tui-layouts.html` |
| GUI | [GUI layout and palette gallery](gui-layouts.md) | `open docs/design/gui-layouts.html` |

Each is a single HTML file carrying its own renderer. Neither needs a server, a
build, a terminal or a network request.

Building one is a separate document. The gallery shows what a choice looks like;
the contract says what an implementation owes.

| Contract | Covers |
| --- | --- |
| [TUI layout and palette system](tui-layout-spec.md) | roles, grammar, rungs, config, build order, and what may not change |
| [GUI layout and palette system](gui-layout-spec.md) | the same, plus a component inventory with metrics, and the palette values as Rust |

## Status

Both galleries are a **future possibility**, not current behavior. Each frontend
has one layout and one set of colors today; [TUI implementation](../tui.md) and
`crate::gui` own what they actually render.

## The shared idea

Both galleries are built on one seam. A layout names roles. A palette supplies
colors for them. Neither names anything the other knows, which is what makes the
two axes independent rather than one hundred hand-drawn screens.

The idea is not new here. `crate::gui::theme` already stores named roles rather
than named colors, which is why swapping a theme is one struct. Each gallery
takes that rule, extends the role set with what its own frontend needs, and adds
a second axis so the arrangement becomes selectable too.

Where the two galleries deliberately differ:

| | TUI | GUI |
| --- | --- | --- |
| Drawing model | a 120x34 grid of character cells | shapes in logical points |
| What may appear | anything a terminal can draw | anything `eframe`'s `Painter` can paint |
| Palettes | ten terminal palettes | ten desktop-native palettes, unrelated to the TUI ten |
| Degradation | three terminal-size rungs | three window-size rungs |

The palettes are not shared. A terminal palette answers to what a terminal can
hold; a desktop palette gets true color, real elevation and a selection ground
that is a color rather than an inversion. Naming the same palette twice across
two frontends would have forced one of the two to render a compromise.
