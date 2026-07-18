use std::{
    collections::{HashMap, HashSet},
    error::Error,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use file_peeker_client::{
    Client, DirectoryEntry, FilePeekerError, Session, SessionConfig, SessionTarget,
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use tokio::{sync::mpsc, task::JoinHandle};

#[derive(Debug)]
enum AppEvent {
    RootBatch(u64, Vec<DirectoryEntry>),
    RootFinished(u64),
    Failed(u64, FilePeekerError),
    OpenFailed(u64, FilePeekerError),
    ExpansionBatch(u64, String, Vec<DirectoryEntry>),
    ExpansionFinished(u64, String),
    ExpansionFailed(u64, String, FilePeekerError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DisplayRow {
    entry: DirectoryEntry,
    parent_path: Option<String>,
    depth: u32,
    expanded: bool,
    error_message: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
enum OpenAction {
    Directory(String),
    File(String),
}

#[derive(Debug, Eq, PartialEq)]
enum RightAction {
    Expand,
    Select(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivePane {
    Sidebar,
    Browser,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputAction {
    Quit,
    TogglePane,
    OpenHome,
    MoveSelection(bool),
    MoveToParent,
    MoveRight,
    ToggleSelected,
    OpenSelected,
}

#[derive(Debug)]
struct App {
    session: Arc<Session>,
    path: String,
    home_path: String,
    active_pane: ActivePane,
    rows: Vec<DisplayRow>,
    selected: usize,
    loading: bool,
    loading_tree_paths: HashSet<String>,
    root_task: Option<JoinHandle<()>>,
    expansion_tasks: HashMap<String, JoinHandle<()>>,
    error: Option<String>,
    generation: u64,
}

impl App {
    fn open_directory(&mut self, path: String, events: mpsc::UnboundedSender<AppEvent>) {
        self.generation += 1;
        let generation = self.generation;
        if let Some(task) = self.root_task.take() {
            task.abort();
        }
        self.loading_tree_paths.clear();
        for (_, task) in self.expansion_tasks.drain() {
            task.abort();
        }
        self.loading = true;
        self.error = None;
        self.path.clone_from(&path);
        self.rows.clear();
        self.selected = 0;
        let session = Arc::clone(&self.session);

        self.root_task = Some(tokio::spawn(async move {
            match Arc::clone(&session).list(path).await {
                Ok(listing) => loop {
                    match listing.next_batch().await {
                        Ok(Some(batch)) => {
                            let _ = events.send(AppEvent::RootBatch(generation, batch));
                        }
                        Ok(None) => {
                            let _ = events.send(AppEvent::RootFinished(generation));
                            break;
                        }
                        Err(error) => {
                            let _ = events.send(AppEvent::Failed(generation, error));
                            break;
                        }
                    }
                },
                Err(error) => {
                    let _ = events.send(AppEvent::Failed(generation, error));
                }
            }
        }));
    }

    fn open_selected(&mut self, events: mpsc::UnboundedSender<AppEvent>) {
        let Some(action) = open_action(&self.rows, self.selected) else {
            return;
        };

        match action {
            OpenAction::Directory(path) => self.open_directory(path, events),
            OpenAction::File(path) => {
                self.error = None;
                let generation = self.generation;
                let session = Arc::clone(&self.session);
                tokio::spawn(async move {
                    if let Err(error) = session.open(path).await {
                        let _ = events.send(AppEvent::OpenFailed(generation, error));
                    }
                });
            }
        }
    }

    fn toggle_selected(&mut self, events: mpsc::UnboundedSender<AppEvent>) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if !row.entry.navigable
            || self.loading_tree_paths.contains(&row.entry.path) && !row.expanded
        {
            return;
        }

        if row.expanded {
            self.collapse(&row.entry.path.clone());
            return;
        }

        let path = row.entry.path.clone();
        if let Some(row) = self.rows.get_mut(self.selected) {
            row.expanded = true;
            row.error_message = None;
        }
        let task_path = path.clone();
        self.loading_tree_paths.insert(path.clone());
        let generation = self.generation;
        let session = Arc::clone(&self.session);
        let task = tokio::spawn(async move {
            match session.list(path.clone()).await {
                Ok(listing) => loop {
                    match listing.next_batch().await {
                        Ok(Some(batch)) => {
                            let _ = events.send(AppEvent::ExpansionBatch(
                                generation,
                                path.clone(),
                                batch,
                            ));
                        }
                        Ok(None) => {
                            let _ = events.send(AppEvent::ExpansionFinished(generation, path));
                            break;
                        }
                        Err(error) => {
                            let _ = events.send(AppEvent::ExpansionFailed(generation, path, error));
                            break;
                        }
                    }
                },
                Err(error) => {
                    let _ = events.send(AppEvent::ExpansionFailed(generation, path, error));
                }
            }
        });
        self.expansion_tasks.insert(task_path, task);
    }

    fn update(&mut self, event: AppEvent) {
        match event {
            AppEvent::RootBatch(generation, entries) if generation == self.generation => {
                self.merge_batch(None, entries);
            }
            AppEvent::RootFinished(generation) if generation == self.generation => {
                self.loading = false;
                self.root_task = None;
            }
            AppEvent::Failed(generation, error) if generation == self.generation => {
                self.loading = false;
                self.root_task = None;
                self.error = Some(error.to_string());
            }
            AppEvent::OpenFailed(generation, error) if generation == self.generation => {
                self.error = Some(error.to_string());
            }
            AppEvent::ExpansionBatch(generation, path, entries)
                if generation == self.generation =>
            {
                self.merge_batch(Some(&path), entries);
            }
            AppEvent::ExpansionFinished(generation, path) if generation == self.generation => {
                self.loading_tree_paths.remove(&path);
                self.expansion_tasks.remove(&path);
            }
            AppEvent::ExpansionFailed(generation, path, error) if generation == self.generation => {
                self.loading_tree_paths.remove(&path);
                self.expansion_tasks.remove(&path);
                if let Some(row) = self.rows.iter_mut().find(|row| row.entry.path == path) {
                    row.error_message = Some(error.to_string());
                }
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, down: bool) {
        if self.rows.is_empty() {
            self.selected = 0;
        } else if down {
            self.selected = (self.selected + 1).min(self.rows.len() - 1);
        } else {
            self.selected = self.selected.saturating_sub(1);
        }
    }

    fn move_to_parent(&mut self) {
        if let Some(index) = parent_index(&self.rows, self.selected) {
            self.selected = index;
        }
    }

    fn move_right(&mut self, events: mpsc::UnboundedSender<AppEvent>) {
        match right_action(&self.rows, self.selected, &self.loading_tree_paths) {
            Some(RightAction::Expand) => self.toggle_selected(events),
            Some(RightAction::Select(index)) => self.selected = index,
            None => {}
        }
    }

    fn merge_batch(&mut self, parent_path: Option<&str>, entries: Vec<DirectoryEntry>) {
        let selected_path = self
            .rows
            .get(self.selected)
            .map(|row| row.entry.path.clone());
        let depth = parent_path
            .and_then(|path| self.rows.iter().find(|row| row.entry.path == path))
            .map_or(0, |row| row.depth + 1);
        for entry in entries {
            if let Some(row) = self
                .rows
                .iter_mut()
                .find(|row| row.entry.path == entry.path)
            {
                row.entry = entry;
                continue;
            }
            let insert_at = parent_path.map_or(self.rows.len(), |path| {
                let parent_index = self
                    .rows
                    .iter()
                    .position(|row| row.entry.path == path)
                    .expect("expanded parent should remain visible");
                self.descendants_end(parent_index)
            });
            self.rows.insert(
                insert_at,
                DisplayRow {
                    entry,
                    parent_path: parent_path.map(str::to_owned),
                    depth,
                    expanded: false,
                    error_message: None,
                },
            );
        }
        self.selected = selected_path
            .and_then(|path| self.rows.iter().position(|row| row.entry.path == path))
            .unwrap_or_else(|| self.selected.min(self.rows.len().saturating_sub(1)));
    }

    fn descendants_end(&self, index: usize) -> usize {
        let depth = self.rows[index].depth;
        self.rows[index + 1..]
            .iter()
            .position(|row| row.depth <= depth)
            .map_or(self.rows.len(), |offset| index + 1 + offset)
    }

    fn collapse(&mut self, path: &str) {
        let Some(index) = self.rows.iter().position(|row| row.entry.path == path) else {
            return;
        };
        let end = self.descendants_end(index);
        let removed_paths: HashSet<String> = self.rows[index + 1..end]
            .iter()
            .map(|row| row.entry.path.clone())
            .collect();
        self.rows.drain(index + 1..end);
        self.rows[index].expanded = false;
        self.rows[index].error_message = None;
        let cancelled: Vec<String> = self
            .expansion_tasks
            .keys()
            .filter(|candidate| candidate.as_str() == path || removed_paths.contains(*candidate))
            .cloned()
            .collect();
        for cancelled_path in cancelled {
            self.loading_tree_paths.remove(&cancelled_path);
            if let Some(task) = self.expansion_tasks.remove(&cancelled_path) {
                task.abort();
            }
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn render(&self, frame: &mut Frame<'_>) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .areas(frame.area());
        frame.render_widget(
            Paragraph::new(self.path.as_str()).block(
                Block::default()
                    .title(" File Peeker ")
                    .borders(Borders::ALL),
            ),
            header,
        );

        let [sidebar, browser] = body_layout(body);
        let sidebar_style = pane_border_style(self.active_pane == ActivePane::Sidebar);
        let mut sidebar_state = ListState::default()
            .with_selected((self.active_pane == ActivePane::Sidebar).then_some(0));
        frame.render_stateful_widget(
            List::new([ListItem::new("Home")])
                .block(
                    Block::default()
                        .title(" Locations ")
                        .borders(Borders::ALL)
                        .border_style(sidebar_style),
                )
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("> "),
            sidebar,
            &mut sidebar_state,
        );

        let items = self.rows.iter().map(|row| {
            let indent = "  ".repeat(row.depth as usize);
            let marker = if self.loading_tree_paths.contains(&row.entry.path) {
                "… "
            } else if row.entry.navigable && row.expanded {
                "▾ "
            } else if row.entry.navigable {
                "▸ "
            } else {
                "  "
            };
            let warning = row.error_message.as_ref().map_or("", |_| " !");
            ListItem::new(Line::from(format!(
                "{indent}{marker}{}{warning}",
                row.entry.name
            )))
        });
        let mut state =
            ListState::default().with_selected((!self.rows.is_empty()).then_some(self.selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    Block::default()
                        .title(" Files ")
                        .borders(Borders::ALL)
                        .border_style(pane_border_style(self.active_pane == ActivePane::Browser)),
                )
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("> "),
            browser,
            &mut state,
        );

        let tree_error = self
            .rows
            .get(self.selected)
            .and_then(|row| row.error_message.as_deref());
        let status = tree_error.or(self.error.as_deref()).map_or_else(
            || {
                if self.loading {
                    "Loading…".into()
                } else {
                    match self.active_pane {
                        ActivePane::Sidebar => {
                            "Enter: open Home  Tab/Shift-Tab: files  q/Esc: quit".into()
                        }
                        ActivePane::Browser => "j/k: select  h/l: navigate  o: toggle  Enter: open  Tab/Shift-Tab: locations  q/Esc: quit".into(),
                    }
                }
            },
            |error| format!("Error: {error}"),
        );
        frame.render_widget(Paragraph::new(status), footer);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let server = sibling_server()?;
    let mut arguments = std::env::args_os().skip(1);
    let first = arguments.next();
    let smoke = first.as_deref() == Some(std::ffi::OsStr::new("--smoke"));
    let start_path = if smoke {
        arguments.next().map(PathBuf::from)
    } else {
        first.map(PathBuf::from)
    }
    .unwrap_or(std::env::current_dir()?);
    let path = start_path
        .to_str()
        .ok_or("start path must be valid UTF-8")?
        .to_owned();
    let session = Client::new()
        .connect(SessionConfig {
            target: SessionTarget::Local {
                server_executable_path: server.to_string_lossy().into_owned(),
            },
        })
        .await?;
    if smoke {
        let listing = Arc::clone(&session).list(path).await?;
        let mut count = 0;
        while let Some(batch) = listing.next_batch().await? {
            count += batch.len();
        }
        println!("PASS local TUI listing smoke test ({count} entries)");
        return Ok(());
    }

    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut app = App {
        session,
        path: path.clone(),
        home_path: home_path(&path),
        active_pane: ActivePane::Browser,
        rows: Vec::new(),
        selected: 0,
        loading: false,
        loading_tree_paths: HashSet::new(),
        root_task: None,
        expansion_tasks: HashMap::new(),
        error: None,
        generation: 0,
    };
    app.open_directory(path, sender.clone());

    let mut terminal = ratatui::try_init()?;
    let result = run(&mut terminal, &mut app, &sender, &mut receiver);
    ratatui::restore();
    result
}

fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    sender: &mpsc::UnboundedSender<AppEvent>,
    receiver: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> Result<(), Box<dyn Error>> {
    loop {
        terminal.draw(|frame| app.render(frame))?;
        while let Ok(app_event) = receiver.try_recv() {
            app.update(app_event);
        }

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match input_action(app.active_pane, key.code) {
                Some(InputAction::Quit) => return Ok(()),
                Some(InputAction::TogglePane) => {
                    app.active_pane = match app.active_pane {
                        ActivePane::Sidebar => ActivePane::Browser,
                        ActivePane::Browser => ActivePane::Sidebar,
                    };
                }
                Some(InputAction::OpenHome) => {
                    app.open_directory(app.home_path.clone(), sender.clone());
                    app.active_pane = ActivePane::Browser;
                }
                Some(InputAction::MoveSelection(down)) => app.move_selection(down),
                Some(InputAction::MoveToParent) => app.move_to_parent(),
                Some(InputAction::MoveRight) => app.move_right(sender.clone()),
                Some(InputAction::ToggleSelected) => app.toggle_selected(sender.clone()),
                Some(InputAction::OpenSelected) => app.open_selected(sender.clone()),
                None => {}
            }
        }
    }
}

fn home_path(fallback: &str) -> String {
    std::env::var("HOME")
        .ok()
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
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

fn pane_border_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}

fn input_action(active_pane: ActivePane, key_code: KeyCode) -> Option<InputAction> {
    match key_code {
        KeyCode::Char('q') | KeyCode::Esc => Some(InputAction::Quit),
        KeyCode::Tab | KeyCode::BackTab => Some(InputAction::TogglePane),
        KeyCode::Enter if active_pane == ActivePane::Sidebar => Some(InputAction::OpenHome),
        KeyCode::Down | KeyCode::Char('j') if active_pane == ActivePane::Browser => {
            Some(InputAction::MoveSelection(true))
        }
        KeyCode::Up | KeyCode::Char('k') if active_pane == ActivePane::Browser => {
            Some(InputAction::MoveSelection(false))
        }
        KeyCode::Char('h') if active_pane == ActivePane::Browser => Some(InputAction::MoveToParent),
        KeyCode::Char('l') if active_pane == ActivePane::Browser => Some(InputAction::MoveRight),
        KeyCode::Char('o') if active_pane == ActivePane::Browser => {
            Some(InputAction::ToggleSelected)
        }
        KeyCode::Enter if active_pane == ActivePane::Browser => Some(InputAction::OpenSelected),
        _ => None,
    }
}

fn sibling_server() -> Result<PathBuf, Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    Ok(executable
        .parent()
        .ok_or("UI executable has no parent directory")?
        .join("file-peeker-server"))
}

fn open_action(rows: &[DisplayRow], selected: usize) -> Option<OpenAction> {
    rows.get(selected).map(|row| {
        if row.entry.navigable {
            OpenAction::Directory(row.entry.path.clone())
        } else {
            OpenAction::File(row.entry.path.clone())
        }
    })
}

fn parent_index(rows: &[DisplayRow], selected: usize) -> Option<usize> {
    let parent_path = rows.get(selected)?.parent_path.as_deref()?;
    rows.iter().position(|row| row.entry.path == parent_path)
}

fn first_child_index(rows: &[DisplayRow], selected: usize) -> Option<usize> {
    let path = rows.get(selected)?.entry.path.as_str();
    rows.iter()
        .position(|row| row.parent_path.as_deref() == Some(path))
}

fn right_action(
    rows: &[DisplayRow],
    selected: usize,
    loading_paths: &HashSet<String>,
) -> Option<RightAction> {
    let row = rows.get(selected)?;
    if !row.entry.navigable || loading_paths.contains(&row.entry.path) {
        return None;
    }
    if row.expanded {
        first_child_index(rows, selected).map(RightAction::Select)
    } else {
        Some(RightAction::Expand)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crossterm::event::KeyCode;
    use file_peeker_client::{DirectoryEntry, EntryKind};

    use super::{
        ActivePane, DisplayRow, InputAction, OpenAction, RightAction, first_child_index,
        input_action, open_action, parent_index, right_action, sidebar_width,
    };

    fn row(path: &str, parent_path: Option<&str>, navigable: bool) -> DisplayRow {
        DisplayRow {
            entry: DirectoryEntry {
                path: path.into(),
                name: path.rsplit('/').next().unwrap_or(path).into(),
                kind: if navigable {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                navigable,
            },
            parent_path: parent_path.map(str::to_owned),
            depth: u32::from(parent_path.is_some()),
            expanded: navigable,
            error_message: None,
        }
    }

    #[test]
    fn selected_entries_choose_directory_navigation_or_file_opening() {
        let mut entries = vec![row("/tmp/file", None, false)];
        assert_eq!(
            open_action(&entries, 0),
            Some(OpenAction::File("/tmp/file".into()))
        );
        entries[0].entry.navigable = true;
        assert_eq!(
            open_action(&entries, 0),
            Some(OpenAction::Directory("/tmp/file".into()))
        );
        assert_eq!(open_action(&entries, 1), None);
    }

    #[test]
    fn horizontal_selection_moves_between_parent_and_first_child() {
        let rows = vec![
            row("/root/a", None, true),
            row("/root/a/one", Some("/root/a"), false),
            row("/root/a/two", Some("/root/a"), false),
        ];

        assert_eq!(first_child_index(&rows, 0), Some(1));
        assert_eq!(parent_index(&rows, 2), Some(0));
        assert_eq!(parent_index(&rows, 0), None);
        assert_eq!(first_child_index(&rows, 1), None);
    }

    #[test]
    fn right_navigation_expands_closed_directories_then_selects_their_first_child() {
        let mut rows = vec![
            row("/root/a", None, true),
            row("/root/a/one", Some("/root/a"), false),
        ];
        rows[0].expanded = false;

        assert_eq!(
            right_action(&rows[..1], 0, &HashSet::new()),
            Some(RightAction::Expand)
        );

        rows[0].expanded = true;
        assert_eq!(
            right_action(&rows, 0, &HashSet::new()),
            Some(RightAction::Select(1))
        );
    }

    #[test]
    fn right_navigation_ignores_files_empty_directories_and_loading_rows() {
        let mut directory = row("/root/a", None, true);
        directory.expanded = true;
        assert_eq!(right_action(&[directory], 0, &HashSet::new()), None);

        let file = row("/root/file", None, false);
        assert_eq!(right_action(&[file], 0, &HashSet::new()), None);
        assert_eq!(right_action(&[], 0, &HashSet::new()), None);

        let mut loading = row("/root/loading", None, true);
        loading.expanded = false;
        assert_eq!(
            right_action(&[loading], 0, &HashSet::from(["/root/loading".to_owned()])),
            None
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
    fn global_keys_work_from_either_pane() {
        for pane in [ActivePane::Sidebar, ActivePane::Browser] {
            assert_eq!(
                input_action(pane, KeyCode::Tab),
                Some(InputAction::TogglePane)
            );
            assert_eq!(
                input_action(pane, KeyCode::BackTab),
                Some(InputAction::TogglePane)
            );
            assert_eq!(input_action(pane, KeyCode::Esc), Some(InputAction::Quit));
        }
    }

    #[test]
    fn pane_specific_keys_are_routed_to_the_focused_pane() {
        assert_eq!(
            input_action(ActivePane::Sidebar, KeyCode::Enter),
            Some(InputAction::OpenHome)
        );
        assert_eq!(input_action(ActivePane::Sidebar, KeyCode::Char('j')), None);
        assert_eq!(
            input_action(ActivePane::Browser, KeyCode::Enter),
            Some(InputAction::OpenSelected)
        );
        assert_eq!(
            input_action(ActivePane::Browser, KeyCode::Char('j')),
            Some(InputAction::MoveSelection(true))
        );
    }
}
