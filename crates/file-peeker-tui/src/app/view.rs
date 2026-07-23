use file_peeker_client::EntryKind;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use super::App;
use crate::browser_context::{
    BrowserContext, BrowserRow, ExpansionStatus, ListingStatus, OpenStatus,
};

const HELP_WIDTH: u16 = 56;
const HELP_HEIGHT: u16 = 15;

impl App {
    pub(crate) fn render(&mut self, frame: &mut Frame<'_>) {
        if self.active_context().is_none() {
            self.render_help(frame);
            return;
        }

        let [header, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .areas(frame.area());
        let context = self.active_context();
        frame.render_widget(
            Paragraph::new(context.map_or("", BrowserContext::root_path)).block(
                Block::default()
                    .title(" File Peeker ")
                    .borders(Borders::ALL),
            ),
            header,
        );
        let [sidebar, browser] = body_layout(body);
        frame.render_widget(
            List::new([ListItem::new("Home")])
                .block(Block::default().title(" Locations ").borders(Borders::ALL)),
            sidebar,
        );
        let context = self
            .active_context_mut()
            .expect("active context was checked before rendering");
        let (viewport_offset, viewport_height) = render_file_list(
            frame,
            browser,
            context.rows().iter().map(row_list_item),
            context.effective_selection(),
            context.viewport_offset(),
        );
        context.set_viewport(viewport_offset, viewport_height);
        let context = self.active_context();
        let controls = "j/k: select  Ctrl-D/U: half-page  o: expand/open  h/l: navigate  R: refresh  q/Esc: quit";
        let status = context.map_or_else(
            || "Not started  q/Esc: quit".into(),
            |context| {
                let message = match context.open_status() {
                    OpenStatus::Opening(path) => format!("Opening {path}…"),
                    OpenStatus::Opened(path) => format!("Opened {path}"),
                    OpenStatus::Failed { path, error } => {
                        format!("Error opening {path}: {error}")
                    }
                    OpenStatus::Idle => {
                        if let Some((path, error)) = context.selected_expansion_error() {
                            format!("Error expanding {path}: {error}")
                        } else {
                            match context.status() {
                                ListingStatus::Loading => "Loading…".into(),
                                ListingStatus::Complete => {
                                    format!("{} items", context.rows().len())
                                }
                                ListingStatus::Failed(error) => format!("Error: {error}"),
                            }
                        }
                    }
                };
                format!("{message}  {controls}")
            },
        );
        frame.render_widget(Paragraph::new(status), footer);

        if let Some(path) = context.and_then(BrowserContext::pending_open_path) {
            render_open_confirmation(frame, path);
        }
    }

    fn render_help(&self, frame: &mut Frame<'_>) {
        let area = centered_help_area(frame.area());
        let block = Block::default()
            .title(" File Peeker ")
            .borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [body, footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
        frame.render_widget(
            Paragraph::new(self.help.as_str()).wrap(Wrap { trim: false }),
            body,
        );
        frame.render_widget(
            Paragraph::new("q/Esc: quit").alignment(Alignment::Center),
            footer,
        );
    }
}

fn render_file_list<'a>(
    frame: &mut Frame<'_>,
    area: Rect,
    entries: impl IntoIterator<Item = ListItem<'a>>,
    selected: Option<usize>,
    viewport_offset: usize,
) -> (usize, usize) {
    let block = Block::default().title(" Files ").borders(Borders::ALL);
    let viewport_height = usize::from(block.inner(area).height);
    let mut list_state = ListState::default()
        .with_offset(viewport_offset)
        .with_selected(selected);
    frame.render_stateful_widget(
        List::new(entries)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ")
            .scroll_padding(1),
        area,
        &mut list_state,
    );
    (list_state.offset(), viewport_height)
}

fn row_list_item(row: &BrowserRow) -> ListItem<'static> {
    let (_, style) = entry_appearance(row.entry().kind);
    let prefix = if row.entry().navigable {
        match row.expansion() {
            ExpansionStatus::Collapsed => "▸ ",
            ExpansionStatus::Loading { .. } => "… ",
            ExpansionStatus::Expanded { .. } => "▾ ",
            ExpansionStatus::Failed { .. } => "! ",
        }
    } else {
        entry_appearance(row.entry().kind).0
    };
    ListItem::new(Line::styled(
        format!("{}{prefix}{}", "  ".repeat(row.depth()), row.entry().name),
        style,
    ))
}

fn render_open_confirmation(frame: &mut Frame<'_>, path: &str) {
    let area = centered_popup_area(frame.area(), path);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Confirm open ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from("Open this file?"),
            Line::from(""),
            Line::styled(path.to_owned(), Style::default().fg(Color::Cyan)),
            Line::from(""),
            Line::from("Press o again to open · Esc/q to cancel"),
        ]))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false }),
        inner,
    );
}

fn centered_popup_area(area: Rect, path: &str) -> Rect {
    let width = area.width.saturating_sub(4).clamp(1, 72);
    let inner_width = usize::from(width.saturating_sub(2).max(1));
    let path_lines = path.chars().count().div_ceil(inner_width).max(1);
    let height = u16::try_from(path_lines)
        .unwrap_or(u16::MAX)
        .saturating_add(6)
        .min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn entry_appearance(kind: EntryKind) -> (&'static str, Style) {
    match kind {
        EntryKind::File => ("  ", Style::default()),
        EntryKind::Directory => (
            "▸ ",
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ),
        EntryKind::Symlink => ("@ ", Style::default().fg(Color::Cyan)),
        EntryKind::Other => ("? ", Style::default().fg(Color::Yellow)),
    }
}

fn sidebar_width(total_width: u16) -> u16 {
    if total_width < 38 {
        return total_width / 3;
    }
    (total_width / 4)
        .clamp(18, 30)
        .min(total_width.saturating_sub(20))
}

fn centered_help_area(area: Rect) -> Rect {
    let width = HELP_WIDTH.min(area.width);
    let height = HELP_HEIGHT.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn body_layout(area: Rect) -> [Rect; 2] {
    Layout::horizontal([
        Constraint::Length(sidebar_width(area.width)),
        Constraint::Min(1),
    ])
    .areas(area)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use file_peeker_client::EntryKind;
    use ratatui::{
        Terminal,
        backend::TestBackend,
        style::{Color, Modifier},
        widgets::ListItem,
    };
    use tokio::sync::mpsc;

    use super::{
        centered_help_area, centered_popup_area, entry_appearance, render_file_list,
        render_open_confirmation, sidebar_width,
    };
    use crate::{Cli, EVENT_CHANNEL_CAPACITY, app::App};

    fn app() -> App {
        let (events, _receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        App::new(events, Cli::command().render_help().to_string())
    }

    #[test]
    fn startup_screen_renders_help_inside_ratatui() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();

        assert!(
            rendered.contains("Usage: file-peeker [PATH]"),
            "rendered startup screen: {rendered:?}"
        );
        assert!(rendered.contains("Directory to browse"));
        assert!(rendered.contains("q/Esc: quit"));
    }

    #[test]
    fn startup_help_area_is_centered_and_responsive() {
        assert_eq!(
            centered_help_area(ratatui::layout::Rect::new(0, 0, 80, 24)),
            ratatui::layout::Rect::new(12, 4, 56, 15)
        );
        assert_eq!(
            centered_help_area(ratatui::layout::Rect::new(3, 2, 40, 10)),
            ratatui::layout::Rect::new(3, 2, 40, 10)
        );
    }

    fn render_test_list(
        terminal: &mut Terminal<TestBackend>,
        selected: usize,
        viewport_offset: usize,
    ) -> usize {
        let mut rendered_offset = viewport_offset;
        terminal
            .draw(|frame| {
                let (offset, viewport_height) = render_file_list(
                    frame,
                    frame.area(),
                    (0..10).map(|index| ListItem::new(format!("Item {index}"))),
                    Some(selected),
                    viewport_offset,
                );
                assert_eq!(viewport_height, 5);
                rendered_offset = offset;
            })
            .unwrap();
        rendered_offset
    }

    #[test]
    fn file_list_scrolls_only_near_the_viewport_border() {
        let mut terminal = Terminal::new(TestBackend::new(20, 7)).unwrap();

        let mut offset = render_test_list(&mut terminal, 0, 0);
        offset = render_test_list(&mut terminal, 1, offset);
        offset = render_test_list(&mut terminal, 2, offset);
        offset = render_test_list(&mut terminal, 3, offset);
        assert_eq!(offset, 0);

        offset = render_test_list(&mut terminal, 4, offset);
        assert_eq!(offset, 1);
        offset = render_test_list(&mut terminal, 5, offset);
        assert_eq!(offset, 2);

        offset = render_test_list(&mut terminal, 4, offset);
        offset = render_test_list(&mut terminal, 3, offset);
        assert_eq!(offset, 2);

        offset = render_test_list(&mut terminal, 2, offset);
        assert_eq!(offset, 1);
    }

    #[test]
    fn entry_kinds_have_distinct_aligned_appearances() {
        let (file_prefix, file_style) = entry_appearance(EntryKind::File);
        let (directory_prefix, directory_style) = entry_appearance(EntryKind::Directory);
        let (symlink_prefix, symlink_style) = entry_appearance(EntryKind::Symlink);
        let (other_prefix, other_style) = entry_appearance(EntryKind::Other);

        assert_eq!(file_prefix, "  ");
        assert_eq!(file_style.fg, None);
        assert_eq!(directory_prefix, "▸ ");
        assert_eq!(directory_style.fg, Some(Color::Blue));
        assert!(directory_style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(symlink_prefix, "@ ");
        assert_eq!(symlink_style.fg, Some(Color::Cyan));
        assert_eq!(other_prefix, "? ");
        assert_eq!(other_style.fg, Some(Color::Yellow));
        assert_eq!(
            [file_prefix, directory_prefix, symlink_prefix, other_prefix]
                .map(str::chars)
                .map(Iterator::count),
            [2; 4]
        );
    }

    #[test]
    fn sidebar_width_is_responsive_and_capped() {
        assert_eq!(sidebar_width(30), 10);
        assert_eq!(sidebar_width(40), 18);
        assert_eq!(sidebar_width(80), 20);
        assert_eq!(sidebar_width(120), 30);
        assert_eq!(sidebar_width(200), 30);
    }

    #[test]
    fn open_confirmation_shows_the_full_path_and_required_keys() {
        let path = "/home/reports/annual-summary.txt";
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| render_open_confirmation(frame, path))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();

        assert!(rendered.contains("Confirm open"));
        assert!(rendered.contains(path));
        assert!(rendered.contains("Press o again to open"));
        assert!(rendered.contains("Esc/q to cancel"));
    }

    #[test]
    fn open_confirmation_expands_vertically_for_wrapped_paths() {
        let short = centered_popup_area(ratatui::layout::Rect::new(0, 0, 40, 24), "/short.txt");
        let long = centered_popup_area(
            ratatui::layout::Rect::new(0, 0, 40, 24),
            "/a/very/long/path/that/wraps/across/multiple/lines/report.txt",
        );

        assert!(long.height > short.height);
        assert!(long.height <= 24);
    }
}
