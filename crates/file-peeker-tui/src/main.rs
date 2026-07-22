use std::{
    collections::{HashMap, HashSet},
    error::Error,
    io,
    sync::Arc,
    time::Duration,
};

mod browser_context;

use clap::{CommandFactory, Parser};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use file_peeker_client::{
    Client, CloseSessionError, DirectoryEntry, EntryKind, Session, SessionConfig, SessionTarget,
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use tokio::sync::mpsc;

use crate::browser_context::{
    BrowserContext, BrowserContextEvent, BrowserContextId, ListingStatus,
};

const EVENT_CHANNEL_CAPACITY: usize = 8;
const HELP_WIDTH: u16 = 56;
const HELP_HEIGHT: u16 = 15;

#[derive(Debug, Eq, Parser, PartialEq)]
#[command(
    name = "file-peeker",
    version,
    about = "Browse a directory with File Peeker"
)]
struct Cli {
    /// Directory to browse
    path: Option<String>,
}

#[derive(Debug)]
struct App {
    client: Arc<Client>,
    help: String,
    session_ids: HashSet<String>,
    contexts: HashMap<BrowserContextId, BrowserContext>,
    active_context_id: Option<BrowserContextId>,
    events: mpsc::Sender<BrowserContextEvent>,
}

impl App {
    fn new(events: mpsc::Sender<BrowserContextEvent>) -> Self {
        Self {
            client: Client::new(),
            help: Cli::command().render_help().to_string(),
            session_ids: HashSet::new(),
            contexts: HashMap::new(),
            active_context_id: None,
            events,
        }
    }

    async fn start(&mut self, path: Option<&str>) -> Result<(), Box<dyn Error>> {
        let Some(path) = path else {
            return Ok(());
        };
        let session_id = self
            .client
            .start_session(SessionConfig {
                target: SessionTarget::Local,
            })
            .await?;
        self.session_ids.insert(session_id.clone());
        let session = self
            .client
            .get_session(session_id.clone())
            .await
            .ok_or_else(|| io::Error::other("started Session was not retained"))?;
        let context_id = self.add_context(session, path.to_owned());
        self.active_context_id = Some(context_id);
        Ok(())
    }

    fn add_context(&mut self, session: Arc<Session>, path: String) -> BrowserContextId {
        let context = BrowserContext::new(session, path, self.events.clone());
        let id = context.id();
        self.contexts.insert(id, context);
        id
    }

    fn update(&mut self, event: BrowserContextEvent) {
        if let Some(context) = self.contexts.get_mut(&event.context_id()) {
            context.apply(event);
        }
    }

    fn restart_active_context(&mut self) {
        if let Some(context_id) = self.active_context_id {
            self.restart_context(context_id);
        }
    }

    fn move_active_selection(&mut self, down: bool) {
        if let Some(context_id) = self.active_context_id
            && let Some(context) = self.contexts.get_mut(&context_id)
        {
            context.move_selection(down);
        }
    }

    fn restart_context(&mut self, context_id: BrowserContextId) {
        if let Some(context) = self.contexts.get_mut(&context_id) {
            context.refresh();
        }
    }

    fn active_context(&self) -> Option<&BrowserContext> {
        self.active_context_id.and_then(|id| self.contexts.get(&id))
    }

    fn cancel_all_listings(&mut self) {
        for context in self.contexts.values_mut() {
            context.cancel();
        }
    }

    async fn shutdown(&mut self) -> Result<(), CloseSessionError> {
        self.cancel_all_listings();
        let session_ids: Vec<_> = self.session_ids.drain().collect();
        let mut first_error = None;
        for session_id in session_ids {
            if let Err(error) = self.client.close_session(session_id).await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn render(&self, frame: &mut Frame<'_>) {
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
            Paragraph::new(context.map_or("", BrowserContext::path)).block(
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
        let entries = context
            .into_iter()
            .flat_map(BrowserContext::entries)
            .map(entry_list_item);
        let mut list_state = ListState::default()
            .with_selected(context.and_then(BrowserContext::effective_selection));
        frame.render_stateful_widget(
            List::new(entries)
                .block(Block::default().title(" Files ").borders(Borders::ALL))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("> "),
            browser,
            &mut list_state,
        );
        let status = context.map_or_else(
            || "Not started  q/Esc: quit".into(),
            |context| match context.status() {
                ListingStatus::Loading => "Loading…  j/k: select  R: refresh  q/Esc: quit".into(),
                ListingStatus::Complete => format!(
                    "{} items  j/k: select  R: refresh  q/Esc: quit",
                    context.entries().len()
                ),
                ListingStatus::Failed(error) => {
                    format!("Error: {error}  j/k: select  R: refresh  q/Esc: quit")
                }
            },
        );
        frame.render_widget(Paragraph::new(status), footer);
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let (sender, mut receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let mut app = App::new(sender);
    if let Err(error) = app.start(cli.path.as_deref()).await {
        let _ = app.shutdown().await;
        return Err(error);
    }
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            let _ = app.shutdown().await;
            return Err(error.into());
        }
    };
    let run_result = run(&mut terminal, &mut app, &mut receiver);
    ratatui::restore();
    let shutdown_result = app.shutdown().await;
    run_result?;
    shutdown_result.map_err(Into::into)
}

fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    receiver: &mut mpsc::Receiver<BrowserContextEvent>,
) -> io::Result<()> {
    loop {
        for _ in 0..EVENT_CHANNEL_CAPACITY {
            let Ok(context_event) = receiver.try_recv() else {
                break;
            };
            app.update(context_event);
        }
        terminal.draw(|frame| app.render(frame))?;
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('R') => app.restart_active_context(),
                KeyCode::Down | KeyCode::Char('j') => app.move_active_selection(true),
                KeyCode::Up | KeyCode::Char('k') => app.move_active_selection(false),
                _ => {}
            }
        }
    }
}

fn entry_list_item(entry: &DirectoryEntry) -> ListItem<'_> {
    let (prefix, style) = entry_appearance(entry.kind);
    ListItem::new(Line::styled(format!("{prefix}{}", entry.name), style))
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
    use super::{
        App, Cli, EVENT_CHANNEL_CAPACITY, centered_help_area, entry_appearance, sidebar_width,
    };
    use clap::{Parser, error::ErrorKind};
    use file_peeker_client::EntryKind;
    use ratatui::{
        Terminal,
        backend::TestBackend,
        style::{Color, Modifier},
    };
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn app() -> App {
        let (events, _receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        App::new(events)
    }

    #[test]
    fn new_app_owns_an_empty_client() {
        let app = app();
        assert_eq!(Arc::strong_count(&app.client), 1);
        assert!(app.session_ids.is_empty());
        assert!(app.contexts.is_empty());
        assert_eq!(app.active_context_id, None);
    }

    #[test]
    fn parses_an_optional_startup_path() {
        assert_eq!(
            Cli::try_parse_from(["file-peeker"]).unwrap(),
            Cli { path: None }
        );
        assert_eq!(
            Cli::try_parse_from(["file-peeker", "/tmp/reports"]).unwrap(),
            Cli {
                path: Some("/tmp/reports".into())
            }
        );
    }

    #[test]
    fn clap_help_is_available() {
        let error = Cli::try_parse_from(["file-peeker", "--help"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        assert!(error.to_string().contains("Usage: file-peeker [PATH]"));
    }

    #[tokio::test]
    async fn starting_without_a_path_keeps_the_app_sessionless() {
        let mut app = app();
        app.start(None).await.unwrap();

        assert!(app.session_ids.is_empty());
        assert!(app.contexts.is_empty());
        assert_eq!(app.active_context_id, None);
    }

    #[test]
    fn startup_screen_renders_help_inside_ratatui() {
        let app = app();
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

    #[tokio::test]
    async fn shutdown_without_a_session_is_harmless() {
        app().shutdown().await.unwrap();
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
}
