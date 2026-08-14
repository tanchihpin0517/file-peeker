//! The application: chrome, key handling, and the bridge between a synchronous
//! immediate-mode UI and the asynchronous [`Client`].
//!
//! The bridge is the interesting part. Listings run as tasks on a Tokio runtime;
//! each task sends groups of entries over a bounded channel, and the UI drains
//! that channel with `try_recv` at the top of every frame. Bounded, so the
//! filesystem still feels backpressure when the UI is the slow one; `try_recv`,
//! so the render thread never blocks. A repaint is requested from the task, so
//! an idle browser costs no frames.

use std::sync::Arc;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Id, Layout, Pos2, Rect, Sense, Vec2,
};
use tokio::sync::mpsc;

use super::dock::{self, Action, Browser, Viewer};
use super::glyph::{self, Side};
use super::theme::{self, Mode, Palette};
use crate::browser::{Expansion, Status, Tree};
use crate::{Client, ConnectionId, DirectoryEntry};

/// How many entries one listing event may carry.
///
/// Entries are coalesced into an event per group that is *already* ready rather
/// than per entry: making room for children ahead of the rows that follow them
/// costs a move of everything after, so paying that per entry is quadratic in the
/// size of the tree. A group is whatever the stream has ready, so a server that
/// trickles still arrives entry by entry.
const LISTING_GROUP: usize = 512;

/// Deep enough for a listing task to run a frame ahead of the renderer, counted
/// in groups rather than entries.
const EVENT_CAPACITY: usize = 64;

/// What a listing task reports back.
enum Event {
    Entries {
        browser: u64,
        request: Request,
        entries: Vec<DirectoryEntry>,
    },
    Finished {
        browser: u64,
        request: Request,
        result: Result<(), String>,
    },
}

/// Which listing an event belongs to: a browser's root, or one expansion of it.
#[derive(Clone, Copy)]
enum Request {
    Root,
    Child(u64),
}

pub(crate) struct App {
    client: Arc<Client>,
    connection: ConnectionId,
    palette: Palette,
    mode: Mode,
    dock: egui_dock::DockState<Browser>,
    events: mpsc::Sender<Event>,
    incoming: mpsc::Receiver<Event>,
    /// Ids for browsers and for expansion requests; both only have to be unique.
    next_id: u64,
    /// Actions raised by the chrome, which draws before the frame's actions are
    /// applied.
    pending: Vec<Action>,
    home: String,
}

impl App {
    pub(crate) fn new(client: Arc<Client>, connection: ConnectionId, root: String) -> Self {
        let (events, incoming) = mpsc::channel(EVENT_CAPACITY);
        let mode = Mode::Light;
        let mut app = Self {
            client,
            connection,
            palette: Palette::of(mode),
            mode,
            dock: dock::initial_state(Browser::new(Tree::new(root.clone()), 1)),
            events,
            incoming,
            next_id: 2,
            pending: Vec::new(),
            home: root,
        };
        app.dock
            .set_focused_node_and_surface(egui_dock::NodePath::MAIN_ROOT);
        app.start_root(1);
        app
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    // ── listings ────────────────────────────────────────────────────────────

    fn start_root(&mut self, browser: u64) {
        let path = self
            .browser(browser)
            .map(|tab| tab.tree.root_path().to_owned())
            .unwrap_or_default();
        self.spawn_listing(browser, Request::Root, path);
    }

    /// Drives one listing to completion, sending its entries a group at a time.
    fn spawn_listing(&self, browser: u64, request: Request, path: String) {
        let client = Arc::clone(&self.client);
        let connection = self.connection;
        let events = self.events.clone();
        tokio::spawn(async move {
            use futures::StreamExt as _;
            let result = match client.op_list_dir(&connection, &path).await {
                Ok(listing) => {
                    let mut groups = listing.ready_chunks(LISTING_GROUP);
                    let mut failure = None;
                    while let Some(group) = groups.next().await {
                        let mut entries = Vec::with_capacity(group.len());
                        for entry in group {
                            match entry {
                                Ok(entry) => entries.push(entry),
                                Err(error) => {
                                    failure = Some(error.to_string());
                                    break;
                                }
                            }
                        }
                        if !entries.is_empty()
                            && events
                                .send(Event::Entries {
                                    browser,
                                    request,
                                    entries,
                                })
                                .await
                                .is_err()
                        {
                            return;
                        }
                        if let Some(error) = failure {
                            failure = Some(error);
                            break;
                        }
                    }
                    failure.map_or(Ok(()), Err)
                }
                Err(error) => Err(error.to_string()),
            };
            let _ = events
                .send(Event::Finished {
                    browser,
                    request,
                    result,
                })
                .await;
        });
    }

    /// Applies everything the listing tasks have queued. One pass per frame, so
    /// a directory the server streams faster than the screen repaints lands in a
    /// few frames rather than being metered out over hundreds.
    fn drain(&mut self) {
        while let Ok(event) = self.incoming.try_recv() {
            match event {
                Event::Entries {
                    browser,
                    request,
                    entries,
                } => {
                    if let Some(tab) = self.browser(browser) {
                        match request {
                            Request::Root => tab.tree.extend_root(entries),
                            Request::Child(id) => tab.tree.extend_child(id, entries),
                        }
                    }
                }
                Event::Finished {
                    browser,
                    request,
                    result,
                } => {
                    if let Some(tab) = self.browser(browser) {
                        match request {
                            Request::Root => tab.tree.finish_root(result),
                            Request::Child(id) => tab.tree.finish_child(id, result),
                        }
                    }
                }
            }
        }
    }

    fn browser(&mut self, id: u64) -> Option<&mut Browser> {
        self.dock
            .iter_all_tabs_mut()
            .map(|(_, tab)| tab)
            .find(|tab| tab.id == id)
    }

    fn focused(&mut self) -> Option<&mut Browser> {
        self.dock.find_active_focused().map(|(_, tab)| tab)
    }

    fn apply(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Toggle { browser, row } => {
                    let request = self.id();
                    let path = self
                        .browser(browser)
                        .and_then(|tab| toggle(&mut tab.tree, row, request));
                    if let Some(path) = path {
                        self.spawn_listing(browser, Request::Child(request), path);
                    }
                }
                Action::Enter { browser, path } => {
                    if let Some(tab) = self.browser(browser) {
                        tab.tree.reroot(path);
                    }
                    self.start_root(browser);
                }
            }
        }
    }
}

/// Opens a closed directory or closes an open one, reporting the path to list
/// when it opens.
///
/// A failed expansion reopens rather than closing, because clicking one again is
/// a retry -- but it is closed first, so whatever the failed attempt managed to
/// place is gone before the new listing starts filling the same spot.
fn toggle(tree: &mut Tree, index: usize, request: u64) -> Option<String> {
    let row = tree.rows().get(index)?;
    if !row.entry().navigable {
        return None;
    }
    match row.expansion() {
        Expansion::Collapsed => Some(tree.begin_expansion(index, request)),
        Expansion::Failed { .. } => {
            tree.collapse(index);
            Some(tree.begin_expansion(index, request))
        }
        Expansion::Loading { .. } | Expansion::Expanded { .. } => {
            tree.collapse(index);
            None
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        Color32::TRANSPARENT.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        theme::apply(&ctx, &self.palette, self.mode);
        self.drain();

        let mut actions = std::mem::take(&mut self.pending);
        self.title_bar(ui);
        self.status_bar(ui);
        self.sidebar(ui, &mut actions);
        self.workspace(ui, &mut actions);
        self.apply(actions);
    }
}

impl App {
    /// The unified top bar: the OS keeps the window buttons, we draw the rest, so
    /// the toolbar and the title share one surface.
    fn title_bar(&mut self, root: &mut egui::Ui) {
        let palette = self.palette;
        let ctx = root.ctx().clone();
        let path = self
            .focused()
            .map(|tab| tab.tree.root_path().to_owned())
            .unwrap_or_default();
        let browser = self.focused().map(|tab| tab.id);
        let mut toggle = false;
        let mut jump = None;
        egui::Panel::top("title")
            .exact_size(46.0)
            .frame(
                egui::Frame::NONE
                    .fill(palette.sunken)
                    .inner_margin(egui::Margin::symmetric(14, 0)),
            )
            .show(root, |ui| {
                let bar = ui.max_rect();
                glyph::edge(
                    ui.painter(),
                    bar.expand2(Vec2::new(14.0, 0.0)),
                    palette.border,
                    Side::Bottom,
                );
                glyph::traffic_lights(ui.painter(), Pos2::new(bar.min.x + 6.0, bar.center().y));
                // Dragging the bar moves the window, since the OS titlebar is
                // hidden.
                if ui
                    .interact(bar, Id::new("drag"), Sense::click_and_drag())
                    .is_pointer_button_down_on()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(bar.translate(Vec2::new(72.0, 0.0))),
                    |ui| {
                        ui.horizontal_centered(|ui| {
                            jump = breadcrumb(ui, &palette, &path);
                        });
                    },
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if self.mode == Mode::Dark {
                            "Light"
                        } else {
                            "Dark"
                        };
                        toggle = pill(ui, &palette, label);
                    });
                });
            });
        if toggle {
            self.mode = self.mode.toggled();
            self.palette = Palette::of(self.mode);
        }
        if let Some((browser, path)) = browser.zip(jump) {
            self.pending.push(Action::Enter { browser, path });
        }
    }

    fn sidebar(&mut self, root: &mut egui::Ui, actions: &mut Vec<Action>) {
        let palette = self.palette;
        let home = self.home.clone();
        let browser = self.focused().map(|tab| tab.id);
        egui::Panel::left("sidebar")
            .exact_size(216.0)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(palette.sunken)
                    .inner_margin(egui::Margin::symmetric(10, 12)),
            )
            .show(root, |ui| {
                glyph::edge(
                    ui.painter(),
                    ui.max_rect().expand2(Vec2::new(10.0, 12.0)),
                    palette.border,
                    Side::Right,
                );
                section(ui, &palette, "Connections");
                connection_row(ui, &palette, "Local", "this machine");

                ui.add_space(18.0);
                section(ui, &palette, "Places");
                let places = [
                    ("Home", home.clone(), "~"),
                    ("Documents", format!("{home}/Documents"), "~/Documents"),
                    ("Downloads", format!("{home}/Downloads"), "~/Downloads"),
                    ("Root", "/".to_owned(), "/"),
                ];
                for (label, path, hint) in places {
                    if place_row(ui, &palette, label, hint)
                        && let Some(browser) = browser
                    {
                        actions.push(Action::Enter { browser, path });
                    }
                }
            });
    }

    /// The content area is the dock: it draws the tab bars, the splits and the
    /// drag-and-drop, and calls back for each tab's contents.
    fn workspace(&mut self, root: &mut egui::Ui, actions: &mut Vec<Action>) {
        let palette = self.palette;
        egui::CentralPanel::no_frame()
            .frame(
                egui::Frame::NONE
                    .fill(palette.base)
                    .inner_margin(egui::Margin::same(10)),
            )
            .show(root, |ui| {
                let style = dock::dock_style(ui.style(), &palette);
                let mut viewer = Viewer {
                    palette: &palette,
                    actions,
                };
                egui_dock::DockArea::new(&mut self.dock)
                    .style(style)
                    .show_add_buttons(false)
                    .show_close_buttons(true)
                    // The dock also offers a collapse arrow and a close-all
                    // cross per leaf; the second is one click from losing a
                    // pane, and neither earns its space beside one tab.
                    .show_leaf_collapse_buttons(false)
                    .show_leaf_close_all_buttons(false)
                    .draggable_tabs(true)
                    .show_inside(ui, &mut viewer);
            });
    }

    fn status_bar(&mut self, root: &mut egui::Ui) {
        let palette = self.palette;
        let status = self.focused().map(|tab| match tab.tree.status() {
            Status::Loading => "Loading…".to_owned(),
            Status::Complete => format!("{} items", tab.tree.rows().len()),
            Status::Failed(error) => format!("Error: {error}"),
        });
        egui::Panel::bottom("status")
            .exact_size(28.0)
            .frame(
                egui::Frame::NONE
                    .fill(palette.sunken)
                    .inner_margin(egui::Margin::symmetric(14, 0)),
            )
            .show(root, |ui| {
                let bar = ui.max_rect();
                glyph::edge(
                    ui.painter(),
                    bar.expand2(Vec2::new(14.0, 0.0)),
                    palette.border,
                    Side::Top,
                );
                let painter = ui.painter().clone();
                painter.text(
                    Pos2::new(bar.min.x, bar.center().y),
                    Align2::LEFT_CENTER,
                    "Local · this machine",
                    FontId::proportional(11.0),
                    palette.muted,
                );
                painter.text(
                    Pos2::new(bar.max.x, bar.center().y),
                    Align2::RIGHT_CENTER,
                    status.unwrap_or_default(),
                    FontId::proportional(11.0),
                    palette.faint,
                );
            });
    }
}

/// Draws the path, and reports the prefix a click asked to go back to. Opening a
/// directory replaces the root, so the breadcrumb is the way back out.
fn breadcrumb(ui: &mut egui::Ui, palette: &Palette, path: &str) -> Option<String> {
    ui.spacing_mut().item_spacing.x = 5.0;
    let mut jump = None;
    if ui
        .add(
            egui::Label::new(
                egui::RichText::new("／")
                    .color(palette.faint)
                    .font(FontId::proportional(12.0)),
            )
            .sense(Sense::click()),
        )
        .clicked()
    {
        jump = Some("/".to_owned());
    }
    for (part, target) in crumbs(path) {
        let last = target.is_none();
        let response = ui.add(
            egui::Label::new(
                egui::RichText::new(part)
                    .color(if last { palette.text } else { palette.muted })
                    .font(FontId::proportional(if last { 13.5 } else { 13.0 })),
            )
            .sense(Sense::click()),
        );
        if let Some(target) = target {
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if response.clicked() {
                jump = Some(target);
            }
            ui.label(egui::RichText::new("›").color(palette.faint));
        }
    }
    jump
}

/// The breadcrumb's segments, each with the path that clicking it goes to.
///
/// The last segment is where the browser already is, so it has nowhere to go, and
/// that is what the renderer reads to draw it as the current one rather than as a
/// link.
fn crumbs(path: &str) -> Vec<(&str, Option<String>)> {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    parts
        .iter()
        .enumerate()
        .map(|(index, part)| {
            let target =
                (index + 1 < parts.len()).then(|| format!("/{}", parts[..=index].join("/")));
            (*part, target)
        })
        .collect()
}

/// A borderless text button that lights up on hover — the app's only button.
fn pill(ui: &mut egui::Ui, palette: &Palette, text: &str) -> bool {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), FontId::proportional(12.0), palette.muted);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(galley.size().x + 20.0, 26.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), palette.raised);
    }
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        if response.hovered() {
            palette.text
        } else {
            palette.muted
        },
    );
    response.clicked()
}

fn section(ui: &mut egui::Ui, palette: &Palette, title: &str) {
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(title)
            .font(FontId::proportional(10.5))
            .color(palette.faint),
    );
    ui.add_space(4.0);
}

fn connection_row(ui: &mut egui::Ui, palette: &Palette, label: &str, detail: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(8), palette.raised);
    let mut marker = rect;
    marker.set_width(2.5);
    painter.rect_filled(
        marker.shrink2(Vec2::new(0.0, 8.0)),
        CornerRadius::same(2),
        palette.accent,
    );
    glyph::status_dot(
        painter,
        Pos2::new(rect.min.x + 18.0, rect.center().y),
        palette.good,
    );
    painter.text(
        Pos2::new(rect.min.x + 32.0, rect.center().y - 8.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        palette.text,
    );
    painter.text(
        Pos2::new(rect.min.x + 32.0, rect.center().y + 7.0),
        Align2::LEFT_CENTER,
        detail,
        FontId::proportional(10.5),
        palette.muted,
    );
}

fn place_row(ui: &mut egui::Ui, palette: &Palette, label: &str, hint: &str) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(
            rect,
            CornerRadius::same(7),
            palette.raised.gamma_multiply(0.55),
        );
    }
    glyph::folder(
        painter,
        Rect::from_min_size(
            Pos2::new(rect.min.x + 9.0, rect.center().y - 6.0),
            Vec2::new(13.0, 11.0),
        ),
        palette.folder.gamma_multiply(0.8),
        false,
    );
    painter.text(
        Pos2::new(rect.min.x + 30.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        palette.text.gamma_multiply(0.9),
    );
    painter.text(
        Pos2::new(rect.max.x - 4.0, rect.center().y),
        Align2::RIGHT_CENTER,
        hint,
        FontId::monospace(10.0),
        palette.faint,
    );
    response.clicked()
}

#[cfg(test)]
mod tests {
    use super::{Tree, crumbs, toggle};
    use crate::browser::{Expansion, Row, fixture::entry};

    fn tree() -> Tree {
        let mut tree = Tree::new("/root".into());
        tree.extend_root(vec![entry("dir", true), entry("note.txt", false)]);
        tree.finish_root(Ok(()));
        tree
    }

    fn names(tree: &Tree) -> Vec<&str> {
        tree.rows().iter().map(Row::name).collect()
    }

    #[test]
    fn a_closed_directory_opens_and_reports_the_path_to_list() {
        let mut tree = tree();
        assert_eq!(toggle(&mut tree, 0, 7).as_deref(), Some("/root/dir"));
        assert_eq!(
            tree.rows()[0].expansion(),
            &Expansion::Loading { request_id: 7 }
        );
    }

    #[test]
    fn a_file_has_nothing_to_open() {
        let mut tree = tree();
        assert_eq!(toggle(&mut tree, 1, 7), None);
        assert_eq!(tree.rows()[1].expansion(), &Expansion::Collapsed);
    }

    #[test]
    fn an_open_directory_closes_and_asks_for_no_listing() {
        let mut tree = tree();
        toggle(&mut tree, 0, 1);
        tree.extend_child(1, vec![entry("child.txt", false)]);
        tree.finish_child(1, Ok(()));
        assert_eq!(names(&tree), ["dir", "child.txt", "note.txt"]);

        assert_eq!(toggle(&mut tree, 0, 2), None);
        assert_eq!(names(&tree), ["dir", "note.txt"]);
        assert_eq!(tree.rows()[0].expansion(), &Expansion::Collapsed);
    }

    /// Retrying a failed expansion drops what the failed attempt managed to place
    /// first. Listing again over the top of it would land the same children beside
    /// the ones already there.
    #[test]
    fn a_failed_directory_retries_without_keeping_what_it_placed() {
        let mut tree = tree();
        toggle(&mut tree, 0, 1);
        tree.extend_child(1, vec![entry("half.txt", false)]);
        tree.finish_child(1, Err("permission denied".into()));
        assert_eq!(names(&tree), ["dir", "half.txt", "note.txt"]);

        assert_eq!(toggle(&mut tree, 0, 2).as_deref(), Some("/root/dir"));
        assert_eq!(names(&tree), ["dir", "note.txt"]);

        tree.extend_child(2, vec![entry("half.txt", false)]);
        assert_eq!(names(&tree), ["dir", "half.txt", "note.txt"]);
    }

    #[test]
    fn every_breadcrumb_but_the_last_points_at_its_own_ancestor() {
        assert_eq!(
            crumbs("/home/user/reports"),
            [
                ("home", Some("/home".to_owned())),
                ("user", Some("/home/user".to_owned())),
                ("reports", None),
            ]
        );
    }

    /// The root has no segments of its own; the leading separator the breadcrumb
    /// draws is what goes back to it.
    #[test]
    fn the_root_has_no_segments() {
        assert!(crumbs("/").is_empty());
    }
}
