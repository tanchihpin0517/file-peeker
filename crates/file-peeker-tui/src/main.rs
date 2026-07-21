use std::{
    collections::{HashMap, HashSet},
    error::Error,
    io,
    sync::Arc,
    time::Duration,
};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use file_peeker_client::{
    Client, CloseSessionError, DirectoryEntry, EntryKind, SessionConfig, SessionTarget,
};
use futures::TryStreamExt;
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use tokio::{sync::mpsc, task::JoinHandle};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct BrowserContextId(Uuid);

impl BrowserContextId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Debug)]
struct BrowserContext {
    id: BrowserContextId,
    session_id: String,
    path: String,
    entries: Vec<DirectoryEntry>,
    selected_index: Option<usize>,
    loading: bool,
    error: Option<String>,
    listing_task: Option<JoinHandle<()>>,
    generation: u64,
}

impl BrowserContext {
    fn new(session_id: String, path: String) -> Self {
        Self {
            id: BrowserContextId::new(),
            session_id,
            path,
            entries: Vec::new(),
            selected_index: None,
            loading: false,
            error: None,
            listing_task: None,
            generation: 0,
        }
    }

    fn prepare_listing(&mut self) -> u64 {
        self.cancel_listing();
        self.generation = self.generation.wrapping_add(1);
        self.entries.clear();
        self.loading = true;
        self.error = None;
        self.generation
    }

    fn cancel_listing(&mut self) {
        if let Some(task) = self.listing_task.take() {
            task.abort();
        }
        self.loading = false;
    }

    fn append_entries(&mut self, entries: Vec<DirectoryEntry>) {
        self.entries.extend(entries);
        if self.selected_index.is_none() && !self.entries.is_empty() {
            self.selected_index = Some(0);
        }
    }

    fn clamp_selection(&mut self) {
        self.selected_index = match (self.selected_index, self.entries.len()) {
            (_, 0) => None,
            (Some(index), length) => Some(index.min(length - 1)),
            (None, _) => Some(0),
        };
    }

    fn move_selection(&mut self, down: bool) {
        if self.entries.is_empty() {
            self.selected_index = None;
            return;
        }
        let last = self.entries.len() - 1;
        let current = self.selected_index.unwrap_or(0).min(last);
        self.selected_index = Some(if down {
            (current + 1).min(last)
        } else {
            current.saturating_sub(1)
        });
    }

    fn effective_selection(&self) -> Option<usize> {
        (!self.entries.is_empty())
            .then(|| self.selected_index.unwrap_or(0).min(self.entries.len() - 1))
    }
}

#[derive(Debug)]
enum AppEvent {
    Batch {
        context_id: BrowserContextId,
        generation: u64,
        entries: Vec<DirectoryEntry>,
    },
    Finished {
        context_id: BrowserContextId,
        generation: u64,
    },
    Failed {
        context_id: BrowserContextId,
        generation: u64,
        error: String,
    },
}

#[derive(Debug)]
struct App {
    client: Arc<Client>,
    session_ids: HashSet<String>,
    contexts: HashMap<BrowserContextId, BrowserContext>,
    active_context_id: Option<BrowserContextId>,
    events: mpsc::UnboundedSender<AppEvent>,
}

impl App {
    fn new(events: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self {
            client: Client::new(),
            session_ids: HashSet::new(),
            contexts: HashMap::new(),
            active_context_id: None,
            events,
        }
    }

    async fn start(&mut self) -> Result<(), Box<dyn Error>> {
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
        let path = session.op_current_root().await?;
        let context_id = self.add_context(session_id, path);
        self.active_context_id = Some(context_id);
        self.restart_context(context_id);
        Ok(())
    }

    fn add_context(&mut self, session_id: String, path: String) -> BrowserContextId {
        let context = BrowserContext::new(session_id, path);
        let id = context.id;
        self.contexts.insert(id, context);
        id
    }

    fn update(&mut self, event: AppEvent) {
        match event {
            AppEvent::Batch {
                context_id,
                generation,
                entries,
            } => {
                if let Some(context) = self.current_generation(context_id, generation) {
                    context.append_entries(entries);
                }
            }
            AppEvent::Finished {
                context_id,
                generation,
            } => {
                if let Some(context) = self.current_generation(context_id, generation) {
                    context.loading = false;
                    context.listing_task = None;
                    context.clamp_selection();
                }
            }
            AppEvent::Failed {
                context_id,
                generation,
                error,
            } => {
                if let Some(context) = self.current_generation(context_id, generation) {
                    context.loading = false;
                    context.error = Some(error);
                    context.listing_task = None;
                    context.clamp_selection();
                }
            }
        }
    }

    fn current_generation(
        &mut self,
        context_id: BrowserContextId,
        generation: u64,
    ) -> Option<&mut BrowserContext> {
        self.contexts
            .get_mut(&context_id)
            .filter(|context| context.generation == generation)
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
        let Some(context) = self.contexts.get_mut(&context_id) else {
            return;
        };
        let generation = context.prepare_listing();
        let session_id = context.session_id.clone();
        let path = context.path.clone();
        let task = start_listing(
            Arc::clone(&self.client),
            session_id,
            path,
            context_id,
            generation,
            self.events.clone(),
        );
        if let Some(context) = self.contexts.get_mut(&context_id) {
            context.listing_task = Some(task);
        }
    }

    fn active_context(&self) -> Option<&BrowserContext> {
        self.active_context_id.and_then(|id| self.contexts.get(&id))
    }

    fn cancel_all_listings(&mut self) {
        for context in self.contexts.values_mut() {
            context.cancel_listing();
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
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .areas(frame.area());
        let context = self.active_context();
        frame.render_widget(
            Paragraph::new(context.map_or("", |context| context.path.as_str())).block(
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
            .flat_map(|context| context.entries.iter())
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
            |context| {
                context.error.as_ref().map_or_else(
                    || {
                        if context.loading {
                            "Loading…  j/k: select  R: refresh  q/Esc: quit".into()
                        } else {
                            format!(
                                "{} items  j/k: select  R: refresh  q/Esc: quit",
                                context.entries.len()
                            )
                        }
                    },
                    |error| format!("Error: {error}  j/k: select  R: refresh  q/Esc: quit"),
                )
            },
        );
        frame.render_widget(Paragraph::new(status), footer);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut app = App::new(sender);
    if let Err(error) = app.start().await {
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

fn start_listing(
    client: Arc<Client>,
    session_id: String,
    path: String,
    context_id: BrowserContextId,
    generation: u64,
    events: mpsc::UnboundedSender<AppEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Some(session) = client.get_session(session_id).await else {
            let _ = events.send(AppEvent::Failed {
                context_id,
                generation,
                error: "Session is no longer available".into(),
            });
            return;
        };
        match session.op_list(&path).await {
            Ok(mut listing) => loop {
                match listing.try_next().await {
                    Ok(Some(entries)) => {
                        let _ = events.send(AppEvent::Batch {
                            context_id,
                            generation,
                            entries,
                        });
                    }
                    Ok(None) => {
                        let _ = events.send(AppEvent::Finished {
                            context_id,
                            generation,
                        });
                        break;
                    }
                    Err(error) => {
                        let _ = events.send(AppEvent::Failed {
                            context_id,
                            generation,
                            error: error.to_string(),
                        });
                        break;
                    }
                }
            },
            Err(error) => {
                let _ = events.send(AppEvent::Failed {
                    context_id,
                    generation,
                    error: error.to_string(),
                });
            }
        }
    })
}

fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    receiver: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> io::Result<()> {
    loop {
        while let Ok(app_event) = receiver.try_recv() {
            app.update(app_event);
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

fn body_layout(area: Rect) -> [Rect; 2] {
    Layout::horizontal([
        Constraint::Length(sidebar_width(area.width)),
        Constraint::Min(1),
    ])
    .areas(area)
}

#[cfg(test)]
mod tests {
    use super::{App, AppEvent, entry_appearance, sidebar_width};
    use file_peeker_client::{DirectoryEntry, EntryKind};
    use ratatui::style::{Color, Modifier};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn entry(name: &str) -> DirectoryEntry {
        DirectoryEntry {
            name: name.into(),
            kind: EntryKind::File,
            navigable: false,
        }
    }
    fn app() -> App {
        let (events, _receiver) = mpsc::unbounded_channel();
        App::new(events)
    }
    fn context(app: &mut App, session: &str, path: &str) -> super::BrowserContextId {
        app.add_context(session.into(), path.into())
    }

    #[test]
    fn new_app_owns_an_empty_client() {
        let app = app();
        assert_eq!(Arc::strong_count(&app.client), 1);
        assert!(app.session_ids.is_empty());
        assert!(app.contexts.is_empty());
        assert_eq!(app.active_context_id, None);
    }

    #[tokio::test]
    async fn shutdown_without_a_session_is_harmless() {
        app().shutdown().await.unwrap();
    }

    #[test]
    fn contexts_have_unique_ids_and_can_share_a_session() {
        let mut app = app();
        let first = context(&mut app, "session", "/one");
        let second = context(&mut app, "session", "/two");
        assert_ne!(first, second);
        assert_eq!(
            app.contexts[&first].session_id,
            app.contexts[&second].session_id
        );
    }

    #[test]
    fn concurrent_events_are_routed_to_their_context() {
        let mut app = app();
        let first = context(&mut app, "session", "/one");
        let second = context(&mut app, "session", "/two");
        let first_generation = app.contexts.get_mut(&first).unwrap().prepare_listing();
        let second_generation = app.contexts.get_mut(&second).unwrap().prepare_listing();
        app.update(AppEvent::Batch {
            context_id: first,
            generation: first_generation,
            entries: vec![entry("one")],
        });
        app.update(AppEvent::Batch {
            context_id: second,
            generation: second_generation,
            entries: vec![entry("two")],
        });
        assert_eq!(app.contexts[&first].entries, [entry("one")]);
        assert_eq!(app.contexts[&second].entries, [entry("two")]);
        assert_eq!(app.contexts[&first].selected_index, Some(0));
        assert_eq!(app.contexts[&second].selected_index, Some(0));
    }

    #[test]
    fn selection_moves_within_the_active_context() {
        let mut app = app();
        let active = context(&mut app, "session", "/one");
        let inactive = context(&mut app, "session", "/two");
        app.active_context_id = Some(active);
        app.contexts.get_mut(&active).unwrap().append_entries(vec![
            entry("one"),
            entry("two"),
            entry("three"),
        ]);
        app.contexts
            .get_mut(&inactive)
            .unwrap()
            .append_entries(vec![entry("other")]);

        app.move_active_selection(true);
        app.move_active_selection(true);
        app.move_active_selection(true);
        assert_eq!(app.contexts[&active].selected_index, Some(2));
        app.move_active_selection(false);
        assert_eq!(app.contexts[&active].selected_index, Some(1));
        assert_eq!(app.contexts[&inactive].selected_index, Some(0));
    }

    #[test]
    fn listing_batches_and_errors_preserve_partial_content() {
        let mut app = app();
        let id = context(&mut app, "session", "/home");
        let generation = app.contexts.get_mut(&id).unwrap().prepare_listing();
        app.update(AppEvent::Batch {
            context_id: id,
            generation,
            entries: vec![entry("one"), entry("two")],
        });
        app.update(AppEvent::Failed {
            context_id: id,
            generation,
            error: "failed".into(),
        });
        assert_eq!(app.contexts[&id].entries, [entry("one"), entry("two")]);
        assert_eq!(app.contexts[&id].error.as_deref(), Some("failed"));
        assert!(!app.contexts[&id].loading);
    }

    #[test]
    fn refresh_only_clears_active_context_and_rejects_stale_events() {
        let mut app = app();
        let active = context(&mut app, "session", "/one");
        let inactive = context(&mut app, "session", "/two");
        app.active_context_id = Some(active);
        let old = app.contexts.get_mut(&active).unwrap().prepare_listing();
        app.contexts
            .get_mut(&active)
            .unwrap()
            .entries
            .push(entry("old"));
        app.contexts
            .get_mut(&inactive)
            .unwrap()
            .entries
            .push(entry("kept"));
        let current = app.contexts.get_mut(&active).unwrap().prepare_listing();
        app.update(AppEvent::Batch {
            context_id: active,
            generation: old,
            entries: vec![entry("stale")],
        });
        assert_ne!(old, current);
        assert!(app.contexts[&active].entries.is_empty());
        assert_eq!(app.contexts[&inactive].entries, [entry("kept")]);
    }

    #[test]
    fn refresh_preserves_index_until_the_replacement_listing_finishes() {
        let mut app = app();
        let id = context(&mut app, "session", "/home");
        let context = app.contexts.get_mut(&id).unwrap();
        context.append_entries(vec![entry("a"), entry("b"), entry("c")]);
        context.selected_index = Some(2);
        let generation = context.prepare_listing();
        assert_eq!(context.selected_index, Some(2));

        app.update(AppEvent::Batch {
            context_id: id,
            generation,
            entries: vec![entry("new-a"), entry("new-b")],
        });
        assert_eq!(app.contexts[&id].effective_selection(), Some(1));
        assert_eq!(app.contexts[&id].selected_index, Some(2));
        app.update(AppEvent::Finished {
            context_id: id,
            generation,
        });
        assert_eq!(app.contexts[&id].selected_index, Some(1));
    }

    #[test]
    fn empty_terminal_listing_clears_selection() {
        let mut app = app();
        let id = context(&mut app, "session", "/home");
        let context = app.contexts.get_mut(&id).unwrap();
        context.selected_index = Some(4);
        let generation = context.prepare_listing();

        app.update(AppEvent::Finished {
            context_id: id,
            generation,
        });

        assert_eq!(app.contexts[&id].selected_index, None);
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
