//! The palette and the egui style derived from it.
//!
//! Every colour the app draws comes from `Palette`, so a theme swap is one
//! struct. egui's defaults are deliberately overwritten wholesale: the giveaway
//! of a default-looking egui app is the widget borders and the blue-grey greys.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Visuals,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Dark,
    Light,
}

impl Mode {
    pub(crate) fn toggled(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }
}

/// Named roles rather than named colours, so the light and dark sets stay in
/// step and no widget reaches for a raw hex.
#[derive(Clone, Copy)]
pub(crate) struct Palette {
    /// Behind everything: the window itself.
    pub(crate) base: Color32,
    /// Sidebar and status bar — one step back from the content.
    pub(crate) sunken: Color32,
    /// The content surface the lists sit on.
    pub(crate) surface: Color32,
    /// Hovered rows, inactive tabs, input fields.
    pub(crate) raised: Color32,
    /// Hairlines between regions.
    pub(crate) border: Color32,
    /// Body text.
    pub(crate) text: Color32,
    /// Secondary text: sizes, kinds, counts.
    pub(crate) muted: Color32,
    /// Tertiary text: indent guides, disabled.
    pub(crate) faint: Color32,
    /// The one colour that means "here" or "yours".
    pub(crate) accent: Color32,
    /// A second hue, for directories.
    pub(crate) folder: Color32,
    pub(crate) warn: Color32,
    pub(crate) good: Color32,
    /// Per-kind glyph tints. The listing reports four kinds, so there are four.
    pub(crate) kind_doc: Color32,
    pub(crate) kind_media: Color32,
}

impl Palette {
    pub(crate) fn of(mode: Mode) -> Self {
        match mode {
            Mode::Dark => Self {
                base: Color32::from_rgb(0x14, 0x16, 0x1b),
                sunken: Color32::from_rgb(0x11, 0x13, 0x17),
                surface: Color32::from_rgb(0x1a, 0x1d, 0x23),
                raised: Color32::from_rgb(0x23, 0x27, 0x2f),
                border: Color32::from_rgb(0x2a, 0x2f, 0x38),
                text: Color32::from_rgb(0xdc, 0xdf, 0xe4),
                muted: Color32::from_rgb(0x8b, 0x93, 0xa1),
                faint: Color32::from_rgb(0x4a, 0x51, 0x5d),
                accent: Color32::from_rgb(0x7a, 0xa2, 0xf7),
                folder: Color32::from_rgb(0x7d, 0xcf, 0xff),
                warn: Color32::from_rgb(0xe0, 0xaf, 0x68),
                good: Color32::from_rgb(0x9e, 0xce, 0x6a),
                kind_doc: Color32::from_rgb(0x9a, 0xa5, 0xb8),
                kind_media: Color32::from_rgb(0x73, 0xda, 0xca),
            },
            Mode::Light => Self {
                base: Color32::from_rgb(0xf6, 0xf7, 0xf9),
                sunken: Color32::from_rgb(0xed, 0xef, 0xf3),
                surface: Color32::from_rgb(0xff, 0xff, 0xff),
                raised: Color32::from_rgb(0xe8, 0xeb, 0xf0),
                border: Color32::from_rgb(0xdb, 0xdf, 0xe6),
                text: Color32::from_rgb(0x1e, 0x22, 0x2a),
                muted: Color32::from_rgb(0x67, 0x6f, 0x7d),
                faint: Color32::from_rgb(0xbf, 0xc5, 0xcf),
                accent: Color32::from_rgb(0x30, 0x62, 0xd6),
                folder: Color32::from_rgb(0x1a, 0x7f, 0xa8),
                warn: Color32::from_rgb(0xa8, 0x6a, 0x0c),
                good: Color32::from_rgb(0x3f, 0x82, 0x2b),
                kind_doc: Color32::from_rgb(0x5d, 0x67, 0x78),
                kind_media: Color32::from_rgb(0x0f, 0x7d, 0x74),
            },
        }
    }
}

/// Row metrics, kept in one place so the list, the tabs and the sidebar agree.
pub(crate) const ROW_HEIGHT: f32 = 26.0;
pub(crate) const INDENT: f32 = 15.0;

pub(crate) fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    // The chrome is English, but filenames are whatever the filesystem holds.
    // egui's default fonts carry no CJK glyphs, so a Chinese filename renders as
    // tofu until a system face is registered; that is why this runs at all.
    let candidates: [(&str, &[&str]); 2] = [
        (
            "ui",
            &[
                "/System/Library/Fonts/SFNS.ttf",
                "/System/Library/Fonts/HelveticaNeue.ttc",
            ],
        ),
        (
            "cjk",
            &[
                "/System/Library/Fonts/PingFang.ttc",
                "/System/Library/Fonts/Hiragino Sans GB.ttc",
            ],
        ),
    ];
    let mut proportional: Vec<String> = Vec::new();
    for (name, paths) in candidates {
        for path in paths {
            if let Ok(bytes) = std::fs::read(path) {
                fonts.font_data.insert(
                    name.to_owned(),
                    std::sync::Arc::new(FontData::from_owned(bytes)),
                );
                proportional.push(name.to_owned());
                break;
            }
        }
    }
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") {
        fonts.font_data.insert(
            "mono".to_owned(),
            std::sync::Arc::new(FontData::from_owned(bytes)),
        );
        let mono = fonts.families.entry(FontFamily::Monospace).or_default();
        mono.insert(0, "mono".to_owned());
        mono.push("cjk".to_owned());
    }
    // Registered ahead of the built-ins, which stay on as the last resort for
    // anything the system faces miss.
    let family = fonts.families.entry(FontFamily::Proportional).or_default();
    for name in proportional.into_iter().rev() {
        family.insert(0, name);
    }
    ctx.set_fonts(fonts);
}

pub(crate) fn apply(ctx: &egui::Context, palette: &Palette, mode: Mode) {
    // Both themes get the same palette, so egui's own light/dark preference
    // cannot pull the app out from under us.
    ctx.all_styles_mut(|style| style_into(style, palette, mode));
}

fn style_into(style: &mut egui::Style, palette: &Palette, mode: Mode) {
    let mut visuals = match mode {
        Mode::Dark => Visuals::dark(),
        Mode::Light => Visuals::light(),
    };
    visuals.panel_fill = palette.base;
    visuals.window_fill = palette.surface;
    visuals.extreme_bg_color = palette.sunken;
    visuals.faint_bg_color = palette.raised;
    visuals.window_stroke = Stroke::new(1.0, palette.border);
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.override_text_color = Some(palette.text);
    visuals.selection.bg_fill = palette.accent.gamma_multiply(0.30);
    visuals.selection.stroke = Stroke::new(1.0, palette.accent);
    visuals.hyperlink_color = palette.accent;
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(if mode == Mode::Dark { 120 } else { 40 }),
    };
    visuals.popup_shadow = visuals.window_shadow;

    // Borderless widgets: fills carry the state instead of outlines, which is
    // what stops the app reading as a debug panel.
    for (widget, fill) in [
        (&mut visuals.widgets.noninteractive, palette.surface),
        (&mut visuals.widgets.inactive, palette.raised),
        (&mut visuals.widgets.hovered, palette.raised),
        (&mut visuals.widgets.active, palette.raised),
        (&mut visuals.widgets.open, palette.raised),
    ] {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.bg_stroke = Stroke::NONE;
        widget.corner_radius = CornerRadius::same(7);
        widget.fg_stroke = Stroke::new(1.0, palette.text);
        widget.expansion = 0.0;
    }
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.muted);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, palette.text);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);

    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.window_margin = egui::Margin::same(10);
    style.spacing.menu_margin = egui::Margin::same(6);
    style.spacing.indent = INDENT;
    style.spacing.scroll.bar_width = 8.0;
    style.spacing.scroll.floating = true;
    style.spacing.scroll.bar_inner_margin = 2.0;
    style.spacing.interact_size.y = ROW_HEIGHT;

    style.text_styles = [
        (TextStyle::Heading, FontId::proportional(15.0)),
        (TextStyle::Body, FontId::proportional(13.0)),
        (TextStyle::Button, FontId::proportional(13.0)),
        (TextStyle::Small, FontId::proportional(11.0)),
        (TextStyle::Monospace, FontId::monospace(12.0)),
    ]
    .into();
}
