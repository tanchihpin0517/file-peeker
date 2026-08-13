//! The docked workspace and what a browser tab draws inside it.
//!
//! `egui_dock` owns the layout, so this module holds no pane structure: a tab is
//! the unit, and where it sits — split, or dragged out into its own window — is
//! the dock's business. What a tab *contains* is here: the filter row, the
//! listing as an `egui_extras` table, and a footer.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Id, Pos2, Rect, Sense, Stroke, Vec2,
};
use egui_dock::{DockState, Style as DockStyle, TabViewer};
use egui_extras::{Column, TableBuilder};

use super::glyph::{self, Side};
use super::theme::{INDENT, Palette, ROW_HEIGHT};
use super::tree::{Expansion, Row, Status, Tree};
use crate::EntryKind;

/// One browser: a root being listed.
pub(crate) struct Browser {
    pub(crate) tree: Tree,
    pub(crate) id: u64,
}

impl Browser {
    pub(crate) fn new(tree: Tree, id: u64) -> Self {
        Self { tree, id }
    }

    /// The tab's label: the last component of its root, which is what the user
    /// recognises it by.
    fn label(&self) -> String {
        let root = self.tree.root.trim_end_matches('/');
        root.rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or("/")
            .to_owned()
    }
}

/// Requests raised by the view for the app to act on, since a tab cannot reach
/// the connection or the runtime itself.
#[derive(Debug)]
pub(crate) enum Action {
    /// Expand or collapse the row, listing its children if it opens.
    Toggle { browser: u64, row: usize },
    /// Replace the browser's root with this path.
    Enter { browser: u64, path: String },
}

pub(crate) fn initial_state(browser: Browser) -> DockState<Browser> {
    DockState::new(vec![browser])
}

/// Everything the tab contents need from the app, borrowed for one frame.
pub(crate) struct Viewer<'a> {
    pub(crate) palette: &'a Palette,
    pub(crate) actions: &'a mut Vec<Action>,
}

impl TabViewer for Viewer<'_> {
    type Tab = Browser;

    fn id(&mut self, tab: &mut Self::Tab) -> Id {
        Id::new(("browser", tab.id))
    }

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        tab.label().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        ui.spacing_mut().item_spacing.y = 0.0;
        if tab.tree.loading() {
            loading_bar(ui, self.palette);
        }
        // Always on screen: with the keyboard gone there is nothing left to open
        // it with, and a field that is always there needs no shortcut.
        filter_row(ui, self.palette, tab);
        // The footer is reserved before the table is built, so a long listing
        // cannot push it off the bottom.
        egui::Panel::bottom(Id::new(("footer", tab.id)))
            .exact_size(30.0)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| footer(ui, self.palette, tab));
        table(ui, self.palette, tab, self.actions);
    }

    /// Each tab paints its own surface, and the dock's would sit over it.
    fn clear_background(&self, _tab: &Self::Tab) -> bool {
        false
    }

    /// The table scrolls itself; an outer scroll area would fight it.
    fn scroll_bars(&self, _tab: &Self::Tab) -> [bool; 2] {
        [false, false]
    }
}

/// The dock's style, derived from ours so its tab bar joins the palette instead
/// of announcing itself.
pub(crate) fn dock_style(egui_style: &egui::Style, palette: &Palette) -> DockStyle {
    let mut style = DockStyle::from_egui(egui_style);
    style.dock_area_padding = None;
    style.main_surface_border_stroke = Stroke::NONE;
    style.separator.width = 10.0;
    style.separator.extra = 0.0;
    style.separator.color_idle = Color32::TRANSPARENT;
    style.separator.color_hovered = palette.border;
    style.separator.color_dragged = palette.accent;

    style.tab_bar.bg_fill = palette.surface;
    style.tab_bar.height = 38.0;
    style.tab_bar.hline_color = palette.border;
    style.tab_bar.fill_tab_bar = false;
    style.tab_bar.corner_radius = CornerRadius::same(7);

    style.tab.active.bg_fill = palette.raised;
    style.tab.active.text_color = palette.text;
    style.tab.active.outline_color = Color32::TRANSPARENT;
    style.tab.active.corner_radius = CornerRadius {
        nw: 7,
        ne: 7,
        sw: 0,
        se: 0,
    };
    style.tab.inactive.bg_fill = Color32::TRANSPARENT;
    style.tab.inactive.text_color = palette.muted;
    style.tab.inactive.outline_color = Color32::TRANSPARENT;
    style.tab.inactive.corner_radius = CornerRadius::same(7);
    style.tab.hovered.bg_fill = palette.raised.gamma_multiply(0.5);
    style.tab.hovered.text_color = palette.text;
    style.tab.hovered.outline_color = Color32::TRANSPARENT;
    style.tab.focused.bg_fill = palette.raised;
    style.tab.focused.text_color = palette.text;
    style.tab.focused.outline_color = Color32::TRANSPARENT;

    style.buttons.close_tab_color = palette.muted;
    style.buttons.close_tab_active_color = palette.text;
    style.buttons.close_tab_bg_fill = palette.raised;
    style.buttons.add_tab_color = palette.muted;
    style.buttons.add_tab_active_color = palette.text;
    style.buttons.add_tab_bg_fill = palette.raised;

    style.overlay.selection_color = palette.accent.gamma_multiply(0.25);
    style.overlay.hovered_leaf_highlight.color = palette.accent.gamma_multiply(0.18);
    style
}

/// A hairline of movement while any listing in this tab is in flight.
fn loading_bar(ui: &mut egui::Ui, palette: &Palette) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 3.0), Sense::hover());
    let time = ui.input(|input| input.time);
    glyph::indeterminate(ui.painter(), rect, time, palette.surface, palette.accent);
    ui.ctx().request_repaint();
}

fn filter_row(ui: &mut egui::Ui, palette: &Palette, tab: &mut Browser) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::hover());
    let painter = ui.painter().clone();
    painter.text(
        Pos2::new(rect.min.x + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        "/",
        FontId::monospace(13.0),
        palette.accent,
    );
    let matches = tab.tree.matches();
    let field = Rect::from_center_size(
        Pos2::new(
            (rect.min.x + 26.0 + rect.max.x - 96.0) / 2.0,
            rect.center().y,
        ),
        Vec2::new(rect.width() - 122.0, 18.0),
    );
    let mut query = tab.tree.filter().to_owned();
    ui.scope_builder(egui::UiBuilder::new().max_rect(field), |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut query)
                // No frame: the `/` and the hairline under the row already
                // say "field", and egui's default box plus focus ring is the
                // loudest default in the library.
                .frame(egui::Frame::NONE)
                .background_color(Color32::TRANSPARENT)
                .hint_text("Filter by name…")
                .desired_width(field.width())
                .font(FontId::proportional(13.0)),
        );
    });
    tab.tree.set_filter(query);
    if let Some(count) = matches {
        painter.text(
            Pos2::new(rect.max.x - 14.0, rect.center().y),
            Align2::RIGHT_CENTER,
            if count == 1 {
                "1 match".to_owned()
            } else {
                format!("{count} matches")
            },
            FontId::proportional(11.5),
            palette.muted,
        );
    }
}

/// The listing. `egui_extras` owns the columns, their widths and the header;
/// only the name cell is ours, because it is the one that has to know where the
/// row sits in the tree.
fn table(ui: &mut egui::Ui, palette: &Palette, tab: &mut Browser, actions: &mut Vec<Action>) {
    // The column dividers inherit the panel hairline, which is far too loud
    // repeated down a long list.
    ui.style_mut().visuals.widgets.noninteractive.bg_stroke =
        Stroke::new(1.0, palette.border.gamma_multiply(0.35));

    let total = tab.tree.visible_len();
    let selected = tab.tree.selected();
    let browser = tab.id;
    let mut clicked = None;

    TableBuilder::new(ui)
        .id_salt(("listing", browser))
        .sense(Sense::click())
        .resizable(true)
        .column(Column::remainder().at_least(180.0).clip(true))
        .column(Column::initial(88.0).at_least(56.0))
        .header(24.0, |mut header| {
            for label in ["Name", "Status"] {
                header.col(|ui| header_label(ui, palette, label));
            }
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, total, |mut table_row| {
                let position = table_row.index();
                let Some((index, matched)) = tab.tree.at(position) else {
                    return;
                };
                let Some(row) = tab.tree.rows().get(index) else {
                    return;
                };
                // One background and one click target for the whole row, which
                // is what a file row needs and what cell-by-cell painting makes
                // awkward.
                table_row.set_selected(index == selected);
                let alpha = if matched { 1.0 } else { 0.45 };
                let tint = kind_colour(palette, row.entry.kind).gamma_multiply(alpha);

                let mut chevron = None;
                table_row.col(|ui| chevron = name_cell(ui, palette, row, tint, alpha));
                table_row.col(|ui| {
                    let (text, colour) = state_label(palette, row);
                    cell(
                        ui,
                        text,
                        FontId::proportional(10.5),
                        if text == kind_label(row) {
                            tint
                        } else {
                            colour
                        },
                    );
                });

                // The row is one click target, so which gesture happened is
                // decided by where the pointer was rather than by letting a
                // second widget compete with it for the click.
                let response = table_row.response();
                let on_chevron = chevron
                    .zip(response.interact_pointer_pos())
                    .is_some_and(|(chevron, pointer): (Rect, Pos2)| chevron.contains(pointer));
                if response.clicked() {
                    clicked = Some(index);
                    if on_chevron && !response.double_clicked() {
                        actions.push(Action::Toggle {
                            browser,
                            row: index,
                        });
                    }
                }
                // Opening a directory replaces the browser's root; the expander
                // is the way to see inside one without leaving where you are.
                if response.double_clicked() && !on_chevron && row.entry.navigable {
                    actions.push(Action::Enter {
                        browser,
                        path: row.path.clone(),
                    });
                }
            });
        });

    if let Some(index) = clicked {
        tab.tree.select(index);
    }
}

fn header_label(ui: &mut egui::Ui, palette: &Palette, label: &str) {
    let rect = ui.max_rect();
    let (at, align) = if label == "Name" {
        (
            Pos2::new(rect.min.x + 39.0, rect.center().y),
            Align2::LEFT_CENTER,
        )
    } else {
        (
            Pos2::new(rect.max.x - 2.0, rect.center().y),
            Align2::RIGHT_CENTER,
        )
    };
    ui.painter().text(
        at,
        align,
        label,
        FontId::proportional(10.0),
        palette.muted.gamma_multiply(0.75),
    );
}

fn cell(ui: &mut egui::Ui, text: &str, font: FontId, colour: Color32) {
    let rect = ui.max_rect();
    ui.painter().text(
        Pos2::new(rect.max.x - 2.0, rect.center().y),
        Align2::RIGHT_CENTER,
        text,
        font,
        colour,
    );
}

/// The one cell no table library can own: indent guides, the expander, the
/// glyph and a clipped name all need the row's place in the tree.
///
/// Returns the expander's rect, which is what tells a click on the arrow from a
/// click on the row.
fn name_cell(
    ui: &mut egui::Ui,
    palette: &Palette,
    row: &Row,
    tint: Color32,
    alpha: f32,
) -> Option<Rect> {
    let rect = ui.max_rect();
    let painter = ui.painter();
    let mut x = rect.min.x;

    for level in 0..row.depth {
        let guide = x + f32::from(level) * INDENT + 3.0;
        painter.line_segment(
            [Pos2::new(guide, rect.min.y), Pos2::new(guide, rect.max.y)],
            Stroke::new(1.0, palette.faint.gamma_multiply(0.45)),
        );
    }
    x += f32::from(row.depth) * INDENT;

    // Widened past the arrow itself so it is comfortable to hit at 13px.
    let chevron = row.entry.navigable.then(|| {
        Rect::from_center_size(
            Pos2::new(x + 5.0, rect.center().y),
            Vec2::new(18.0, rect.height()),
        )
    });
    if let Some(chevron) = chevron {
        let hovered = ui
            .input(|input| input.pointer.latest_pos())
            .is_some_and(|pointer| chevron.contains(pointer));
        glyph::chevron(
            painter,
            chevron.center(),
            if hovered {
                palette.text
            } else {
                palette.muted.gamma_multiply(alpha)
            },
            row.expansion == Expansion::Expanded,
        );
    }
    x += 15.0;

    let icon = Rect::from_min_size(Pos2::new(x, rect.center().y - 6.5), Vec2::new(14.0, 13.0));
    if row.entry.navigable {
        glyph::folder(painter, icon, tint, row.expansion == Expansion::Expanded);
    } else {
        glyph::file(painter, icon, tint);
    }
    x += 24.0;

    let galley = painter.layout_no_wrap(
        row.name.clone(),
        FontId::proportional(12.5),
        palette.text.gamma_multiply(alpha),
    );
    painter.with_clip_rect(rect).galley(
        Pos2::new(x, rect.center().y - galley.size().y / 2.0),
        galley,
        palette.text,
    );
    chevron
}

fn footer(ui: &mut egui::Ui, palette: &Palette, tab: &mut Browser) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
    let painter = ui.painter().clone();
    glyph::edge(&painter, rect, palette.border, Side::Top);

    let (left, colour) = match tab.tree.status() {
        Status::Failed(error) => (format!("Cannot read: {error}"), palette.warn),
        _ => (
            tab.tree
                .selected_row()
                .map(|row| row.path.clone())
                .unwrap_or_default(),
            palette.muted,
        ),
    };
    painter.text(
        Pos2::new(rect.min.x + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        left,
        FontId::proportional(11.5),
        colour,
    );
    painter.text(
        Pos2::new(rect.max.x - 14.0, rect.center().y),
        Align2::RIGHT_CENTER,
        format!(
            "{}{} rows",
            if tab.tree.loading() {
                "Loading · "
            } else {
                ""
            },
            tab.tree.rows().len()
        ),
        FontId::monospace(10.5),
        palette.faint,
    );
}

fn kind_colour(palette: &Palette, kind: EntryKind) -> Color32 {
    match kind {
        EntryKind::Directory => palette.folder,
        EntryKind::Symlink => palette.kind_media,
        EntryKind::File => palette.kind_doc,
        EntryKind::Other => palette.muted,
    }
}

fn kind_label(row: &Row) -> &'static str {
    match row.entry.kind {
        EntryKind::Directory => "",
        EntryKind::Symlink => "link",
        EntryKind::File => "file",
        EntryKind::Other => "other",
    }
}

/// What the second column says: how an expansion is going when something is
/// happening to it, and otherwise what kind of entry it is.
fn state_label(palette: &Palette, row: &Row) -> (&'static str, Color32) {
    match row.expansion {
        Expansion::Loading => ("loading", palette.muted),
        Expansion::Failed => ("unreadable", palette.warn),
        _ => (kind_label(row), palette.muted),
    }
}
