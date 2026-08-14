# GUI layout and palette system: implementation contract

[gui-layouts.md](gui-layouts.md) explains the idea and
[gui-layouts.html](gui-layouts.html) shows every combination. This document is
the part an implementer needs: the seams, the exact values, what each component
owes, and what may not change on the way in.

Read the gallery first. Every rule below has a rendered example there, and the
HTML is the authoritative source for anything numeric this document does not
restate.

## Status

**Future possibility.** None of this is implemented. `crate::gui` renders one
layout with one palette in `theme.rs`; [Code structure](../code-structure.md)
owns where that code lives.

## What must not change

The gallery changes how the desktop front end is arranged and colored. It
changes nothing about what it does, and an implementation that quietly alters
any of the following has gone wrong.

- **The shared tree model.** `crate::browser::Tree` is the model both frontends
  drive; the GUI holds no copy of it. A layout renders a `Tree` and raises
  `dock::Action`; it may not fork the model to suit an arrangement.
- **Listing and expansion states.** `browser::Status::{Loading, Complete,
  Failed(String)}` and `browser::Expansion::{Collapsed, Loading, Expanded,
  Failed}`. A layout may not invent a fifth, and the second column's vocabulary
  stays `loading` / `unreadable` / the entry kind.
- **Entry kinds.** `EntryKind::{File, Directory, Symlink, Other}`, and the rule
  that a non-navigable row never gets a chevron.
- **Streaming.** Listings arrive in groups of up to `LISTING_GROUP` (512) over a
  bounded channel drained with `try_recv` at the top of each frame, so a frame
  draws whatever has landed. No layout may require a complete listing before it
  draws, and none may block the render thread waiting for one.
- **Idle costs no frames.** Repaints are requested by the listing task. A layout
  may request continuous repaint only while something is genuinely animating —
  in practice the indeterminate bar, and only while a listing is in flight.
- **Row order.** A listing follows the host's iteration order and is never
  sorted. No layout may sort for presentation.
- **The pointer gestures.** Single click selects; a click on the chevron toggles
  expansion; a double click on a navigable row replaces the browser's root. One
  row is one click target, and which gesture happened is decided by where the
  pointer was, not by a second widget competing for the click.
- **Resizable columns.** The listing table is resizable today. A layout adding
  columns keeps them draggable rather than fixing widths.

### One thing the gallery proposes rather than preserves

The desktop front end has **no key bindings at all** — it is pointer-driven, and
`dock.rs` says so where it explains why the filter field is always on screen
rather than behind a shortcut. Three layouts (雙面板, 大綱, 指令面板) draw key
hints such as `y`, `p`, `Tab`, `⌘K` and `esc`. Those are a proposal for a
keyboard layer, not a depiction of existing behavior, and they are separate
scope from the layout seam. Building 指令面板 without that layer gives you an
overlay nothing can open.

## 1. What exists today, and what this adds

| Concern | Today in `crate::gui` | This proposal |
| --- | --- | --- |
| Palette | `theme::Palette`, 14 roles, one dark and one light set | 18 roles, ten palettes, each with an independent light set |
| Selection ground | computed as `accent.gamma_multiply(0.30)` | a named `sel` role, plus `on_sel` for text on it |
| Error color | none; `warn` serves both meanings | a named `bad`, distinct from `warn` |
| Elevation | a constant chosen by `Mode` | a `shadow` role owned by the palette |
| Layout | one arrangement, hard-coded across `app.rs` and `dock.rs` | a `Layout` enum of ten, each declaring a `Grammar` and a minimum size |
| Window resize | no response beyond egui reflow | three rungs, with one shared renderer below the top one |
| Configuration | none | `[ui]` in a config file, with per-role overrides |

The seam is deliberately narrow. A layout decides which rectangle is drawn where
and what role it plays; a palette decides what color a role is. Neither names
anything the other knows. That is what makes ten times ten a hundred
combinations rather than a hundred hand-written screens, and it is the one
property that must survive implementation.

## 2. The types

```rust
/// Named roles rather than named colours, so a theme swap stays one struct.
/// The first fourteen already exist in `crate::gui::theme`.
pub(crate) struct Palette {
    // Grounds, back to front.
    base: Color32,      // the window itself
    sunken: Color32,    // sidebar, status bar, any rail
    surface: Color32,   // the content face the lists sit on
    raised: Color32,    // hover, inactive tab, input field, chip

    border: Color32,    // the only kind of hairline

    sel: Color32,       // NEW: the selected row's ground
    on_sel: Color32,    // NEW: text and glyphs on that ground

    text: Color32,      // body
    muted: Color32,     // secondary: counts, kinds, paths
    faint: Color32,     // tertiary: indent guides, disabled, scroll thumb

    accent: Color32,    // "here" and "yours"
    folder: Color32,    // directories

    good: Color32,      // healthy connection, finished transfer
    warn: Color32,      // refused expansion, unreadable entry
    bad: Color32,       // NEW: failed connection, aborted transfer

    kind_doc: Color32,  // regular files
    kind_media: Color32,// symlinks

    shadow: Shadow,     // NEW: elevation, owned by the palette
}

pub(crate) struct Shadow {
    colour: Color32,
    alpha: f32,         // 0.10 (riso) to 0.70 (safelight)
    offset_y: i8,       // 8 throughout
    blur: u8,           // 24 throughout
}

pub(crate) enum Layout {
    Dock, Sidebar, Miller, Inspector, Dual,
    Palette, Gallery, Console, Outline, Treemap,
}

/// What a layout keeps when it no longer fits. Read only by the compact
/// renderer; a layout drawing at full size ignores it.
pub(crate) struct Grammar {
    rail: Rail,             // Full | Icons | None
    chrome: Chrome,         // Titlebar | Toolbar | None
    card: bool,             // content on a floating rounded face, or full bleed
    band: bool,             // alternating row tint
    cols: &'static [Column],// extra columns, in the order they are given up
    footer: Footer,         // Status | Pane | None
}

pub(crate) enum Rung { Full, Compact, Minimal }
```

Configuration:

```toml
[ui]
layout  = "miller"
palette = "cyanotype"
mode    = "dark"          # or "light"

# A palette is chosen whole, then corrected role by role.
[ui.palette.override]
accent = "#ffc247"
```

`Layout::min()` returns the smallest window the layout draws itself in. Above
it, the layout paints. Below it, the shared compact renderer paints from the
layout's `Grammar`. See section 8.

## 3. Palette roles

Where each role is used, so an implementer can tell a wrong color from a wrong
placement.

| Role | Drawn by |
| --- | --- |
| `base` | window ground; the gap around a floating content card; the command palette's scrim at 0.42 alpha uses `shadow.colour`, not this |
| `sunken` | rail, status bar, dual-pane headers, console activity log, Miller preview pane, treemap panel |
| `surface` | content face, active tab, grid ground, dual panes |
| `raised` | hovered row, inactive tab, filter field, clipboard card, transfer track, segmented control, inspector card, gutter buttons |
| `border` | every hairline, and the 1px `window_stroke` on the command palette overlay |
| `sel` | selected row ground; selected grid cell; selected command-palette hit |
| `on_sel` | text, glyph tint and chevron on any `sel` ground, at 0.8–0.85 alpha for secondary values |
| `text` | file and directory names, headings, connection names, metadata values |
| `muted` | column headers (0.75 alpha), status text, match counts, chevrons, `loading` |
| `faint` | indent guides (0.45 alpha), scroll thumb (0.5), sizes, timestamps, section heads |
| `accent` | the filter `/`, active-tab underline, active connection marker, active pane bar, transfer fill and track, selected treemap outline, matched-substring underline |
| `folder` | directory glyphs, Places glyphs, treemap directory cells |
| `good` | connected status dots, latency, successful log lines |
| `warn` | `unreadable` status, the `!` badge on a refused directory |
| `bad` | failed connection dot and label, failed log lines |
| `kind_doc` | file glyphs, treemap file cells |
| `kind_media` | symlink glyphs |
| `shadow` | the floating content card, the command palette overlay, and nothing else |

## 4. The rules a palette must satisfy

These are the design decisions most likely to be lost in implementation, so they
are stated as checks rather than as advice. The gallery runs all of them.

**Contrast floors.** Every palette, in both modes, must clear these ratios. All
twenty variants currently do.

| Pair | Floor |
| --- | --- |
| `text` on `surface`, `raised` | 4.5 |
| `muted` on `surface`, `sunken`, `base` | 4.5 |
| `on_sel` on `sel` | 4.5 |
| `accent`, `folder`, `kind_doc`, `kind_media`, `good`, `warn`, `bad` on `surface` | 3.0 |

`faint` is deliberately exempt. It draws indent guides and the scroll thumb,
never a word the reader must read.

**The sameness rule.** Two roles do not have to be equal to stop being told
apart. The gallery measures separation with a red-mean weighted RGB distance and
treats anything under **60** as one color:

```rust
fn apart(a: Color32, b: Color32) -> f32 {
    let rm = (f32::from(a.r()) + f32::from(b.r())) / 2.0;
    let (dr, dg, db) = (/* per-channel differences as f32 */);
    ((2.0 + rm / 256.0) * dr * dr + 4.0 * dg * dg
        + (2.0 + (255.0 - rm) / 256.0) * db * db).sqrt()
}
```

**What a palette may collapse, and what it may not.** A palette that will not
distinguish two roles gives them the same color, and the cost is reported rather
than hidden. Three collapses are legitimate and are computed, not hard-coded:

- `folder` ~ `kind_media` — directories and symlinks separate by glyph shape
  only. Survivable in a layout carrying a kind column; not otherwise.
- `accent` ~ `folder` — "selected" and "is a directory" become one signal.
  Affects the treemap outline and the dual-pane active bar.
- `kind_doc` ~ `kind_media` — four entry kinds become three.
- `accent == good` exactly — a transfer in progress and a finished one look
  alike. Deliberate in 示波.
- `shadow.alpha < 0.13` — elevation stops working; card layouts fall back on
  the border hairline. Deliberate in 孔版.

A palette may **not** collapse `text`/`muted` against their grounds, or `warn`
against `bad` to the point where a refused directory and a dead connection read
the same.

**Compute the consequence, never write it.** Deriving these notes from the color
values is what keeps them true when a color changes. An implementation that
hard-codes "this palette has low contrast" reintroduces exactly the staleness
this design avoids.

## 5. The ten palettes

Design intent, so a future change knows what it is preserving:

| Palette | Family | The idea | Deliberate cost |
| --- | --- | --- | --- |
| 石墨 graphite | system | macOS's own neutral greys, a hint of blue, system-blue accent. The one that is never wrong. | Least distinctive of the ten. |
| 藍曬 cyanotype | optics | Prussian blue is the surface, not the background; amber accent, the only warm that survives on blue. | Committed color; not a quiet room. |
| 海圖 bathymetric | paper | Nautical chart: buff paper, inked navy, chart-red marks. Dark mode is the night bridge, not an inversion. | Two very different modes to maintain. |
| 暗房 safelight | optics | Darkroom: near-black with red in it, the lamp's orange as accent. One waveband of discipline. | `folder` ~ `kind_media` in light. |
| 亞麻 linen | paper | Archive: linen cloth, oxblood spine labels, endpaper green. Light-first; dark is one lamp, not an inversion. | — |
| 陽極 anodized | metal | Anodized aluminium: cold greys, a violet no natural light makes, cyan folders. | — |
| 銅綠 verdigris | metal | Oxidised bronze: two hues only, seven steps of lightness. Verdigris marks directories because directories also grow by themselves. | `folder` ~ `kind_media`; two hues cannot carry four kinds. |
| 孔版 riso | print | Risograph: newsprint, fluorescent pink and reflex blue, flat opaque ink. | `shadow.alpha` 0.10 — elevation is off by design. |
| 示波 oscilloscope | optics | Instrument: warm black, olive-tinted greys, phosphor green. On an instrument, "signal" and "fine" are one color. | `accent == good`; in light, `accent` ~ `folder` too. |
| 青花 porcelain | paper | Blue-and-white ware: glaze white, cobalt as the only hue. Underglaze allows one color, so this palette takes one. | Everything but state colors is one hue. |

Two of these exist partly to prove the seam reports its own costs. 孔版 and 示波
are the palettes that make the consequence notes fire, and removing them would
make the mechanism untestable.

The values, generated from the gallery:

```rust
const fn c(hex: u32) -> Color32 { /* 0xRRGGBB */ }
const fn shadow(colour: Color32, alpha: f32) -> Shadow {
    Shadow { colour, alpha, offset_y: 8, blur: 24 }
}

const GRAPHITE_DARK: Palette = Palette {      // 石墨 · dark
    base: c(0x16181c), sunken: c(0x101216), surface: c(0x1b1e23), raised: c(0x24282f),
    border: c(0x2b3038), sel: c(0x23324a), on_sel: c(0xeaf1ff),
    text: c(0xdee1e6), muted: c(0x8d949f), faint: c(0x4c525b),
    accent: c(0x4f8cff), folder: c(0x5ec2f2),
    good: c(0x57c46a), warn: c(0xe2a33c), bad: c(0xff5d55),
    kind_doc: c(0x99a2ae), kind_media: c(0x5fd3c0),
    shadow: shadow(c(0x000000), 0.55),
};

const GRAPHITE_LIGHT: Palette = Palette {      // 石墨 · light
    base: c(0xf4f5f7), sunken: c(0xeceef1), surface: c(0xffffff), raised: c(0xe6e9ee),
    border: c(0xdcdfe5), sel: c(0xd6e4fb), on_sel: c(0x0b2a5a),
    text: c(0x1b1e24), muted: c(0x646c78), faint: c(0xb6bcc5),
    accent: c(0x0a63d6), folder: c(0x0f7fb0),
    good: c(0x2f7d2a), warn: c(0x8a5c08), bad: c(0xc33027),
    kind_doc: c(0x5a6472), kind_media: c(0x0d7a72),
    shadow: shadow(c(0x141820), 0.16),
};

const CYANOTYPE_DARK: Palette = Palette {      // 藍曬 · dark
    base: c(0x0a1c2e), sunken: c(0x071624), surface: c(0x0e2438), raised: c(0x163149),
    border: c(0x1e3f5c), sel: c(0x1c4a6e), on_sel: c(0xeaf6ff),
    text: c(0xd8e8f4), muted: c(0x7ea3bf), faint: c(0x476b86),
    accent: c(0xffc247), folder: c(0x7fd4ff),
    good: c(0x6fd6a6), warn: c(0xffb03a), bad: c(0xff7b6b),
    kind_doc: c(0x9dbdd4), kind_media: c(0x63d9d0),
    shadow: shadow(c(0x020c16), 0.6),
};

const CYANOTYPE_LIGHT: Palette = Palette {      // 藍曬 · light
    base: c(0xe7eff5), sunken: c(0xdbe6ef), surface: c(0xf4f8fb), raised: c(0xd4e3ef),
    border: c(0xb9cfe0), sel: c(0xc3ddf0), on_sel: c(0x0a2436),
    text: c(0x10293d), muted: c(0x466880), faint: c(0x9db8cb),
    accent: c(0xa05e00), folder: c(0x06688f),
    good: c(0x2c7358), warn: c(0x8a5000), bad: c(0xb23a2a),
    kind_doc: c(0x47637a), kind_media: c(0x0d6f6a),
    shadow: shadow(c(0x10293d), 0.15),
};

const BATHYMETRIC_DARK: Palette = Palette {      // 海圖 · dark
    base: c(0x101820), sunken: c(0x0b1219), surface: c(0x161f28), raised: c(0x1f2a34),
    border: c(0x2a3742), sel: c(0x22343f), on_sel: c(0xeaf2f6),
    text: c(0xdfe4e8), muted: c(0x8b98a3), faint: c(0x4d5a65),
    accent: c(0xff6a4d), folder: c(0x58a8c8),
    good: c(0x66b884), warn: c(0xe0a13c), bad: c(0xff5545),
    kind_doc: c(0x97a3ad), kind_media: c(0x4fc6bd),
    shadow: shadow(c(0x000000), 0.6),
};

const BATHYMETRIC_LIGHT: Palette = Palette {      // 海圖 · light
    base: c(0xf2ece0), sunken: c(0xe9e1d2), surface: c(0xfbf7ef), raised: c(0xe5dccb),
    border: c(0xd3c8b3), sel: c(0xdbe7ea), on_sel: c(0x12242f),
    text: c(0x1e2b38), muted: c(0x58646e), faint: c(0xb0a894),
    accent: c(0xc3452c), folder: c(0x1d5f7a),
    good: c(0x3d6f4a), warn: c(0x98600e), bad: c(0xa52d1c),
    kind_doc: c(0x5a6470), kind_media: c(0x157a4e),
    shadow: shadow(c(0x322819), 0.18),
};

const SAFELIGHT_DARK: Palette = Palette {      // 暗房 · dark
    base: c(0x150c0c), sunken: c(0x0f0808), surface: c(0x1c1010), raised: c(0x281818),
    border: c(0x35201f), sel: c(0x3a1d1a), on_sel: c(0xffe9dd),
    text: c(0xefdcd8), muted: c(0xb08a84), faint: c(0x6b4a46),
    accent: c(0xff8a3d), folder: c(0xffb066),
    good: c(0x9fbf6a), warn: c(0xffc247), bad: c(0xff5147),
    kind_doc: c(0xbd9a94), kind_media: c(0xd9a06a),
    shadow: shadow(c(0x000000), 0.7),
};

const SAFELIGHT_LIGHT: Palette = Palette {      // 暗房 · light
    base: c(0xf6efe9), sunken: c(0xece2da), surface: c(0xfffaf6), raised: c(0xe8dcd2),
    border: c(0xdbcabe), sel: c(0xf0dccf), on_sel: c(0x35180f),
    text: c(0x2a1a16), muted: c(0x7a5e55), faint: c(0xbda99d),
    accent: c(0xb7431a), folder: c(0xa15a1c),
    good: c(0x4d6b2c), warn: c(0x8c5809), bad: c(0xa82a20),
    kind_doc: c(0x6d5a53), kind_media: c(0x8a5a2e),
    shadow: shadow(c(0x3c1e14), 0.16),
};

const LINEN_DARK: Palette = Palette {      // 亞麻 · dark
    base: c(0x171814), sunken: c(0x111210), surface: c(0x1d1f1a), raised: c(0x272923),
    border: c(0x33362c), sel: c(0x2e3128), on_sel: c(0xf2f0e4),
    text: c(0xe6e4d8), muted: c(0x97998a), faint: c(0x575a4f),
    accent: c(0xd9707a), folder: c(0x7fb894),
    good: c(0x86b96a), warn: c(0xd8a94a), bad: c(0xe8615f),
    kind_doc: c(0xa3a596), kind_media: c(0x7fa6d0),
    shadow: shadow(c(0x000000), 0.6),
};

const LINEN_LIGHT: Palette = Palette {      // 亞麻 · light
    base: c(0xf3f0e8), sunken: c(0xeae6db), surface: c(0xfcfbf6), raised: c(0xe6e2d5),
    border: c(0xd6d1c2), sel: c(0xe3ddcb), on_sel: c(0x241a19),
    text: c(0x23241f), muted: c(0x63655c), faint: c(0xb2b0a2),
    accent: c(0x7d2731), folder: c(0x2f5d43),
    good: c(0x3c6b46), warn: c(0x82590c), bad: c(0x93242c),
    kind_doc: c(0x6b6a5e), kind_media: c(0x2a5878),
    shadow: shadow(c(0x2d2a1e), 0.15),
};

const ANODIZED_DARK: Palette = Palette {      // 陽極 · dark
    base: c(0x1a1a1f), sunken: c(0x141419), surface: c(0x202027), raised: c(0x2a2a33),
    border: c(0x34343f), sel: c(0x322a48), on_sel: c(0xf0eaff),
    text: c(0xe2e2e8), muted: c(0x91919e), faint: c(0x55555f),
    accent: c(0xa97dff), folder: c(0x6bb8f5),
    good: c(0x77cf8a), warn: c(0xe6b158), bad: c(0xff6a72),
    kind_doc: c(0x9e9eab), kind_media: c(0x64d0c8),
    shadow: shadow(c(0x000000), 0.55),
};

const ANODIZED_LIGHT: Palette = Palette {      // 陽極 · light
    base: c(0xeeeef1), sunken: c(0xe4e4e9), surface: c(0xfbfbfd), raised: c(0xe0e0e7),
    border: c(0xd2d2da), sel: c(0xe2daf7), on_sel: c(0x2a1a52),
    text: c(0x1e1e24), muted: c(0x63636f), faint: c(0xb4b4bf),
    accent: c(0x6b3fd4), folder: c(0x0a63aa),
    good: c(0x2f7a45), warn: c(0x835a0e), bad: c(0xbf2f3e),
    kind_doc: c(0x5c5c68), kind_media: c(0x0f7570),
    shadow: shadow(c(0x191923), 0.14),
};

const VERDIGRIS_DARK: Palette = Palette {      // 銅綠 · dark
    base: c(0x16150f), sunken: c(0x100f0b), surface: c(0x1d1b14), raised: c(0x27251b),
    border: c(0x343123), sel: c(0x33301f), on_sel: c(0xf8f2df),
    text: c(0xe8e2cf), muted: c(0xa09983), faint: c(0x605b49),
    accent: c(0xd98a3f), folder: c(0x62c2a4),
    good: c(0x8fc46a), warn: c(0xe0b455), bad: c(0xe2635a),
    kind_doc: c(0xa8a18c), kind_media: c(0x5fbfb4),
    shadow: shadow(c(0x000000), 0.62),
};

const VERDIGRIS_LIGHT: Palette = Palette {      // 銅綠 · light
    base: c(0xf0ece0), sunken: c(0xe6e0d0), surface: c(0xfbf8ee), raised: c(0xe2dccb),
    border: c(0xcfc7b1), sel: c(0xddd6c0), on_sel: c(0x2b2617),
    text: c(0x26231a), muted: c(0x676150), faint: c(0xb4ac96),
    accent: c(0x8a4e17), folder: c(0x1f7a63),
    good: c(0x45702f), warn: c(0x7d5a0e), bad: c(0x9c3325),
    kind_doc: c(0x625d4c), kind_media: c(0x1c6f68),
    shadow: shadow(c(0x322d1e), 0.16),
};

const RISO_DARK: Palette = Palette {      // 孔版 · dark
    base: c(0x14131a), sunken: c(0x0e0d13), surface: c(0x1a1922), raised: c(0x23222d),
    border: c(0x2e2d3a), sel: c(0x3a2036), on_sel: c(0xffe6f0),
    text: c(0xece9f2), muted: c(0x94909f), faint: c(0x565360),
    accent: c(0xff5c94), folder: c(0x6f8dff),
    good: c(0x45d3a8), warn: c(0xffb340), bad: c(0xff4f6d),
    kind_doc: c(0x9c98a8), kind_media: c(0x4fd0c6),
    shadow: shadow(c(0x000000), 0.12),
};

const RISO_LIGHT: Palette = Palette {      // 孔版 · light
    base: c(0xefece4), sunken: c(0xe5e1d6), surface: c(0xfaf8f2), raised: c(0xe3dfd2),
    border: c(0xcdc7b8), sel: c(0xffd9e6), on_sel: c(0x4a0a22),
    text: c(0x1a1a1a), muted: c(0x5d5d5d), faint: c(0xadaaa0),
    accent: c(0xe0004d), folder: c(0x1642c8),
    good: c(0x00674f), warn: c(0xa35f00), bad: c(0xc2003f),
    kind_doc: c(0x565656), kind_media: c(0x00726b),
    shadow: shadow(c(0x1a1a1a), 0.1),
};

const OSCILLOSCOPE_DARK: Palette = Palette {      // 示波 · dark
    base: c(0x0d1110), sunken: c(0x080b0a), surface: c(0x121716), raised: c(0x1a211f),
    border: c(0x242d2a), sel: c(0x17322a), on_sel: c(0xddffe9),
    text: c(0xd6e2d8), muted: c(0x7f9186), faint: c(0x4a5a52),
    accent: c(0x4ee08a), folder: c(0x8fe3b4),
    good: c(0x4ee08a), warn: c(0xffb300), bad: c(0xff4d4d),
    kind_doc: c(0x97a89c), kind_media: c(0x5ad6cf),
    shadow: shadow(c(0x000000), 0.65),
};

const OSCILLOSCOPE_LIGHT: Palette = Palette {      // 示波 · light
    base: c(0xe9eeea), sunken: c(0xdfe5e0), surface: c(0xf7faf8), raised: c(0xdbe3dd),
    border: c(0xc6d0c9), sel: c(0xcfe3d6), on_sel: c(0x06251a),
    text: c(0x10201a), muted: c(0x536159), faint: c(0xa4b0a8),
    accent: c(0x05704a), folder: c(0x0d6b52),
    good: c(0x05704a), warn: c(0x7d5300), bad: c(0xb32b2b),
    kind_doc: c(0x55625a), kind_media: c(0x0a6d66),
    shadow: shadow(c(0x0f2319), 0.14),
};

const PORCELAIN_DARK: Palette = Palette {      // 青花 · dark
    base: c(0x101822), sunken: c(0x0b1119), surface: c(0x16202c), raised: c(0x1e2a38),
    border: c(0x283544), sel: c(0x22354d), on_sel: c(0xe9f2ff),
    text: c(0xdce6f2), muted: c(0x8699ad), faint: c(0x4d5f72),
    accent: c(0x6fa8ff), folder: c(0x8ec8ff),
    good: c(0x6fce9e), warn: c(0xe5b055), bad: c(0xff6b6b),
    kind_doc: c(0x94a4b6), kind_media: c(0x58c9c6),
    shadow: shadow(c(0x000000), 0.55),
};

const PORCELAIN_LIGHT: Palette = Palette {      // 青花 · light
    base: c(0xf1f3f5), sunken: c(0xe6eaef), surface: c(0xffffff), raised: c(0xe2e8ef),
    border: c(0xccd7e2), sel: c(0xd5e2f4), on_sel: c(0x0d244a),
    text: c(0x14243a), muted: c(0x56697f), faint: c(0xa8b8c8),
    accent: c(0x1d4ea8), folder: c(0x2c6fb5),
    good: c(0x2c6a52), warn: c(0x7d5f0f), bad: c(0xa83030),
    kind_doc: c(0x5b6a7c), kind_media: c(0x1f6f77),
    shadow: shadow(c(0x14243a), 0.13),
};
```

Regenerate after any change to the gallery rather than editing both by hand; the
gallery's `PALETTES` array uses these same role names in camelCase.

## 6. Components

Sizes are egui logical points. Every one is either already in `crate::gui` or
derived from something that is.

### Primitives

The whole page is built from these, and nothing else may appear:

| Primitive | egui call |
| --- | --- |
| filled rect, corner radius 7 (controls) or 10–12 (panels, overlays) | `Painter::rect_filled` |
| 1px hairline | `glyph::edge`, a `Stroke` of 1.0 |
| stroked path | `Painter::line_segment` |
| polygon | `Shape::Path` |
| circle | `Painter::circle_filled` |
| text at a point with an alignment | `Painter::text`, `FontId`, `Align2` |
| one shadow | `epaint::Shadow { offset: [0, 8], blur: 24 }` |
| a color at reduced strength | `Color32::gamma_multiply` |

No gradients, no blur behind, nothing that exists only in CSS. Icons are drawn
from these, never typed as font characters — the breadcrumb `›` and `／` are the
only characters standing in for shapes, and they are already what `app.rs` types.

### Glyphs

All in `crate::gui::glyph`; the last three are new.

| Glyph | Construction |
| --- | --- |
| `folder` | 14×13. Tab: `w*0.46 × 4`, radius 1, 0.75 alpha. Body: from `y+2.5`, radius 2. Open state: body drops to 0.55 alpha and gains a 3pt solid lid. |
| `file` | 14×13. Pentagon with a 4.5 fold at top-right, 0.85 alpha; fold triangle at 0.45. |
| `chevron` | Two segments, arm 3.2, stroke 1.5, round caps. Rotates a quarter turn when open. |
| `status_dot` | Halo circle r5 at 0.22 alpha, core r2.6. |
| `traffic_lights` | r6, pitch 20, fixed `#ff5f57` `#febc2e` `#28c840`. |
| `indeterminate` | Height 3, radius 1. **Track is the accent at 0.22 alpha; the segment is the accent solid at 28% width.** The track is a hue, not a ground role — see section 12. |
| `edge` | 1px on one side of a rect. |
| `cross` **NEW** | Two segments, arm 3.4, stroke 1.3. Tab close. |
| `arrow` **NEW** | Shaft plus two barbs, barb 4.2, stroke 1.6. Dual-pane copy direction. |

### Chrome

**Titlebar** — height 44 (48 where a layout wants more air). Ground `base`,
bottom hairline. Traffic lights at x=18, centred. Breadcrumb from x=90: `／` in
`faint`, then components at 13pt `muted` with `›` separators, last component
13.5pt `text` weight 600. A borderless 62×26 pill at the right edge, x = w−76.

**Toolbar** — height 52, ground `sunken`. Same breadcrumb, plus the layout's own
control (a segmented control for tabs in 側欄, a List/Grid switch in 縮圖) and a
200×26 inline filter field in `raised`.

**Status bar** — height 28, ground `sunken`, top hairline. Connection name at
11pt `muted`, x=14. Item count at 11pt `faint`, right-aligned x = w−14.

### Rail

Width 216 by default; 240 when it carries per-connection tabs, 200–208 when a
layout needs the width elsewhere. Ground `sunken`, right hairline. Sections in
order: Connections, Places, Clipboard, Transfer.

**Section head** — 10.5pt `faint`, letter-spacing 0.04em.

**Connection row** — height 38, pitch 42, radius 8. Active: `raised` fill plus a
2.5×22 `accent` marker at the left, radius 2. Status dot at x+18. Name 12.5pt
`text` weight 500; detail (`this machine`, `ssh · 10.0.0.14`) 10.5pt `faint`.
Dot color is `good` when connected and `bad` when refused — this is the only
place in the app where `bad` is unavoidable.

**Place row** — height 28, pitch 30, radius 7. Folder glyph at x+8, label 12pt,
`text` when current and `muted` otherwise.

**Clipboard card** — height 40, radius 8, `raised`. File glyph, name 11.5pt
`text`, origin 10pt `faint`.

**Transfer** — name 11.5pt `text`, percentage 10.5pt `accent` monospace
right-aligned. Track height 4, radius 2, `raised`; fill `accent`. Route and byte
counts 10.5pt `faint` below.

### Content

**Tab bar** — height 30. Tab width is the label at 12pt plus 50. Active tab:
`surface` fill, radius 7, plus a 2pt `accent` underline inset 10 from each side.
Inactive: `raised` at 0.55 alpha. Close cross at the right of each tab.

**Filter row** — height 40. `/` in `accent` monospace 13pt at x+14. Query at
13pt `text` from x+26, or the hint `Filter by name…` in `faint`. Caret 1.5×17 in
`accent` after the query. Match count 11.5pt `muted`, right-aligned. Bottom
hairline at 0.6 alpha.

**Column header** — height 24. `Name` at 10pt `muted` 0.75 alpha, x+39, aligned
with the first name cell. Value columns right-aligned from `w − 12`, each with a
1px `border` divider at 0.5 alpha. The 12pt gutter is reserved whether or not
the list scrolls, so columns do not jump when it starts.

**List row** — height 26. Selected: `sel` fill inset 2pt, radius 6, and every
glyph and word switches to `on_sel` (secondary values at 0.8–0.85 alpha).
Banded layouts tint odd rows `raised` at 0.35 alpha.

The name cell is the one cell a table library cannot own, because it needs the
row's place in the tree:

1. One indent guide per ancestor level, 1px `faint` at 0.45 alpha, at
   `x + 10 + level*15 + 3`.
2. Chevron at the row's own depth, `muted`, only when the entry is navigable.
3. Kind glyph 15pt further right; `folder` for directories, `file` otherwise,
   tinted by kind.
4. Name 24pt further right, 12.5pt, weight 500 for directories. Clipped to what
   is left after the value columns and the gutter.

Value columns carry `size`, `status`, `kind`, `modified` or the parent path at
10.5pt. `status` is `loading` in `muted`, `unreadable` in `warn`, and otherwise
the entry kind tinted like its glyph. Unmatched rows under an active filter draw
at 0.45 alpha rather than being removed.

**Scroll thumb** — width 5, radius 2.5, `faint` at 0.5 alpha, minimum height 28,
inside the reserved gutter.

**Pane footer** — height 30, top hairline. Selected path 11.5pt `muted` left;
row count 10.5pt `faint` monospace right.

### Layout-specific components

**Overlay card** (指令面板) — `min(720, w−64)` × `min(428, h−132)`, radius 12,
`surface`, drop shadow **and** a 1px `border` stroke, because `window_stroke` is
what makes it survive a palette with no elevation. Header 56pt in `raised`.
Hit rows 44pt; the matched substring gets a 1.5pt `accent` underline. Connection
chips along the bottom. The app behind is dimmed by a `shadow.colour` scrim at
0.42 alpha — a scrim, not a blur.

**Grid cell** (縮圖) — 152×128, gap 8, radius 10. Glyph drawn at 3.4× its list
size, the only place the app's own icons carry a whole cell. Name 12pt centred,
size 10.5pt monospace centred. A refused directory gets a `warn` `!` badge; a
listing one gets an indeterminate bar across the cell.

**Treemap cell** (面積圖) — radius 4, filled with the entry's kind tint at 0.3
alpha, 0.58 when selected. Selection is a 2pt `accent` outline, never an
inverted fill: the fill already carries kind, and one color cannot do two jobs.
Label at 11.5pt only when the cell exceeds 54×26; size at 10pt only above 42pt
tall.

**Activity log line** (主控台) — pitch 21, all monospace 10.5pt. Timestamp
`faint` at x=14, verb `muted` at 72, target `text` at 116, outcome right-aligned
in the role its status names: `good`, `accent`, `warn` or `bad`.

**Connection card** (主控台) — height 56, radius 9, `raised` when active with the
same 2.5pt accent marker as the rail row. Name 13pt weight 600, detail 10.5pt
`faint`, latency and tab count monospace right-aligned.

**Pane header** (雙面板) — height 40, `sunken`, bottom hairline. Status dot, host
name 12.5pt weight 600, path 11.5pt `muted`, latency monospace right. The active
pane gets a 2pt `accent` bar along its bottom edge — the one unambiguous signal
of which side a copy runs from.

**Gutter** (雙面板) — 76pt wide. Two 34pt `raised` buttons carrying `arrow`
glyphs, the enabled direction in `accent` and the other in `muted`, then the
transfer percentage and track.

**Inspector** (檢視器) — width 300, `sunken`, left hairline. An 88pt `raised`
card with the glyph and name, then `Where`, then seven metadata rows at pitch
24, then the transfer queue.

## 7. The ten layouts

Each is an arrangement of the components above. `min` is the smallest window it
draws itself in.

| # | `Layout` | Name | min | Structure |
| --- | --- | --- | --- | --- |
| 1 | `Dock` | 分頁塢 | 1100×700 | Titlebar, 216 rail, tab bar, floating content card with filter row, table and pane footer, status bar. **What ships today.** |
| 2 | `Sidebar` | 側欄 | 1040×620 | Toolbar with a segmented tab control, 240 rail carrying each connection's own tabs, full-bleed banded table with four columns. |
| 3 | `Miller` | 瀑布欄 | 1180×620 | Three 232pt directory columns plus a preview pane. The only layout where the path back is visible without a breadcrumb. |
| 4 | `Inspector` | 檢視器 | 1160×660 | Rail, content card, and a permanent 300pt right-hand inspector holding metadata and the transfer queue. |
| 5 | `Dual` | 雙面板 | 1120×640 | Two panes with their own connection headers, a 76pt gutter carrying direction and progress. No rail. The copy workflow made structural. |
| 6 | `Palette` | 指令面板 | 700×500 | The filter promoted from a row to a centred overlay over a dimmed app. |
| 7 | `Gallery` | 縮圖 | 1060×640 | Toolbar with a List/Grid switch, rail, and a grid of 152×128 cells. |
| 8 | `Console` | 主控台 | 1140×720 | Connection strip across the top, full-width four-column table, docked 148pt activity log. No rail. |
| 9 | `Outline` | 大綱 | 620×460 | One centred column, 34pt rows, 14pt names, indent guides. No rail, no tab bar, no card, no column header. |
| 10 | `Treemap` | 面積圖 | 1160×640 | Table on the left, directory sizes as area on the right. |

Selection state that every layout must express: which connection, which tab,
which row, and — where the layout has two panes — which pane.

## 8. Degradation

Below its `min`, a layout is drawn by one shared renderer reading its `Grammar`.
A folded layout is not a different product: it keeps its own grammar and loses
only the panels there is no room for.

| Rung | Window | Behavior |
| --- | --- | --- |
| Full | 1280×800 | every layout draws itself |
| Compact | 1000×680 | eight of ten fold; 指令面板 and 大綱 do not |
| Minimal | 760×560 | the same renderer with the grammar switched off |

Drop order, in this sequence:

1. The rail goes from a 200pt panel, to a 56pt strip of connection dots, to
   nothing. Places lose their labels before connections lose theirs, because
   which host you are on outranks which folder you are in.
2. Extra columns are given up in the order the `Grammar` lists them. `Status` is
   always last, because it is where a refused expansion says so.
3. The tab bar goes below 600pt of height.
4. The filter row goes below 560pt of height.

大綱 folds within itself instead: it drops `status` below 600pt of column width
and `size` below 520pt, leaving names and indentation, which is its skeleton
anyway.

## 9. Build order

Each phase is independently shippable and independently verifiable.

1. **Widen `Palette`.** Add `sel`, `on_sel`, `bad`, `shadow`; replace the
   `accent.gamma_multiply(0.30)` selection formula and the mode-constant shadow
   with the new roles. One palette still, no user-visible change beyond the
   selection ground. Fixes the loading-track bug in section 12 on the way past.
2. **Land the ten palettes** behind a config key, with the override table. Still
   one layout. This is where the checks in section 4 become tests.
3. **Extract the components** in section 6 out of `app.rs` and `dock.rs` into
   functions taking a rect and a `&Palette`. No behavior change; this is the
   refactor that makes ten layouts possible without ten copies.
4. **Introduce `Layout`** with two variants, `Dock` and `Outline` — the two
   furthest apart structurally, so the seam gets stressed early. `Outline` needs
   no rail, no tab bar and no card, which proves the components are optional.
5. **Add the compact renderer** and `Grammar`, with the rungs in section 8.
6. **The remaining eight layouts**, cheapest first: `Sidebar`, `Inspector`,
   `Gallery`, `Console`, `Treemap`, `Miller`, `Dual`, `Palette`. The last two are
   last for a reason — see the caution below.

**Caution on `egui_dock`.** Four layouts (瀑布欄, 雙面板, 主控台, 面積圖) place
panes in ways the dock currently owns. Before building them, settle whether the
dock expresses them or whether the layout seam replaces it for those variants. A
layout seam fighting the dock is worse than no layout seam.

## 10. Acceptance checks

Write these as tests; they are the design decisions in executable form.

1. Every palette in both modes clears the contrast floors in section 4.
2. `apart()` is above 60 for the pairs a palette does not declare collapsed, and
   the consequence notes fire for exactly the pairs it does.
3. The loading track clears 60 against `surface` in all twenty variants.
4. Every layout renders at all three rungs without a panic and without drawing
   outside the window rect.
5. The ten layouts produce ten distinct outputs at every rung; if two match, the
   compact renderer has lost a grammar field.
6. Column values and their headers stay aligned when a list starts scrolling.
7. `Status` survives every column drop.

## 11. Content for fixtures

The gallery uses one fixture across all one hundred combinations so only the
design varies. Reusing it keeps screenshots comparable: three connections (local,
one SSH, one refused), this repository's own tree including a directory whose
expansion was refused and one still listing, a real `/var/log` listing for the
second pane, and one transfer in flight.

## 12. Defects in current code found while drawing this

- **`dock.rs:173`** passes `palette.surface` as the loading bar's track color
  onto a panel that is already `palette.surface`. The bar therefore has no
  track: the accent segment slides through empty space with no start and no end.
  The fix is not another ground role — `raised` against `surface` measures 27
  on the `apart()` scale, under the 60 threshold — but the accent at 0.22 alpha,
  which separates by hue the way `status_dot` already separates its halo.
- **`theme.rs`** computes the selection ground as `accent.gamma_multiply(0.30)`.
  That formula cannot be tuned per palette, and in the darker palettes it lands
  too close to `surface` to read as a selection at all.
- **`warn` carries two meanings.** A refused expansion and a dead connection are
  the same color today, which is what `bad` exists to separate.
