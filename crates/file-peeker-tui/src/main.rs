use std::{
    collections::{HashMap, HashSet},
    error::Error,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use file_peeker_client::{
    Client, FilePeekerError, Session, SessionConfig, SessionTarget, State, StateRow, StateSnapshot,
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use tokio::{sync::mpsc, task::JoinHandle};

#[derive(Debug)]
enum AppEvent {
    StateOpened(u64, Arc<State>, StateSnapshot),
    Failed(u64, FilePeekerError),
    OpenFailed(u64, FilePeekerError),
    ExpansionFinished(u64, String, StateSnapshot),
    ExpansionFailed(u64, String, StateSnapshot),
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

#[derive(Debug)]
struct App {
    session: Arc<Session>,
    state: Option<Arc<State>>,
    path: String,
    rows: Vec<StateRow>,
    selected: usize,
    loading: bool,
    loading_tree_paths: HashSet<String>,
    expansion_tasks: HashMap<String, JoinHandle<()>>,
    error: Option<String>,
    generation: u64,
}

impl App {
    fn open_directory(&mut self, path: String, events: mpsc::UnboundedSender<AppEvent>) {
        self.generation += 1;
        let generation = self.generation;
        self.loading_tree_paths.clear();
        for (_, task) in self.expansion_tasks.drain() {
            task.abort();
        }
        self.loading = true;
        self.error = None;
        let session = Arc::clone(&self.session);

        tokio::spawn(async move {
            match session.open_state(path).await {
                Ok(state) => {
                    let snapshot = state.snapshot();
                    let _ = events.send(AppEvent::StateOpened(generation, state, snapshot));
                }
                Err(error) => {
                    let _ = events.send(AppEvent::Failed(generation, error));
                }
            }
        });
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
        if !row.entry.navigable || self.loading_tree_paths.contains(&row.entry.path) {
            return;
        }

        let Some(state) = self.state.as_ref().map(Arc::clone) else {
            return;
        };
        if row.expanded {
            match state.collapse(row.entry.path.clone()) {
                Ok(snapshot) => self.replace_snapshot(snapshot),
                Err(error) => self.error = Some(error.to_string()),
            }
            return;
        }

        let path = row.entry.path.clone();
        let task_path = path.clone();
        self.loading_tree_paths.insert(path.clone());
        let generation = self.generation;
        let state = Arc::clone(&state);
        let task = tokio::spawn(async move {
            let event = match state.expand(path.clone()).await {
                Ok(snapshot) => AppEvent::ExpansionFinished(generation, path, snapshot),
                Err(_) => AppEvent::ExpansionFailed(generation, path, state.snapshot()),
            };
            let _ = events.send(event);
        });
        self.expansion_tasks.insert(task_path, task);
    }

    fn update(&mut self, event: AppEvent) {
        match event {
            AppEvent::StateOpened(generation, state, snapshot) if generation == self.generation => {
                self.state = Some(state);
                self.selected = 0;
                self.replace_snapshot(snapshot);
                self.loading = false;
            }
            AppEvent::Failed(generation, error) if generation == self.generation => {
                self.loading = false;
                self.error = Some(error.to_string());
            }
            AppEvent::OpenFailed(generation, error) if generation == self.generation => {
                self.error = Some(error.to_string());
            }
            AppEvent::ExpansionFinished(generation, path, snapshot)
                if generation == self.generation =>
            {
                self.loading_tree_paths.remove(&path);
                self.expansion_tasks.remove(&path);
                self.replace_snapshot(snapshot);
            }
            AppEvent::ExpansionFailed(generation, path, snapshot)
                if generation == self.generation =>
            {
                self.loading_tree_paths.remove(&path);
                self.expansion_tasks.remove(&path);
                self.replace_snapshot(snapshot);
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

    fn replace_snapshot(&mut self, snapshot: StateSnapshot) {
        let selected_path = self
            .rows
            .get(self.selected)
            .map(|row| row.entry.path.clone());
        let visible_paths: HashSet<&str> = snapshot
            .rows
            .iter()
            .map(|row| row.entry.path.as_str())
            .collect();
        let removed_loading_paths: Vec<String> = self
            .expansion_tasks
            .keys()
            .filter(|path| !visible_paths.contains(path.as_str()))
            .cloned()
            .collect();
        for path in removed_loading_paths {
            self.loading_tree_paths.remove(&path);
            if let Some(task) = self.expansion_tasks.remove(&path) {
                task.abort();
            }
        }
        self.path = snapshot.path;
        self.rows = snapshot.rows;
        self.selected = selected_path
            .and_then(|path| self.rows.iter().position(|row| row.entry.path == path))
            .unwrap_or_else(|| self.selected.min(self.rows.len().saturating_sub(1)));
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
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("> "),
            body,
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
                    "j/k: select  h: parent  l: expand/child  o: toggle  Enter: open  q/Esc: quit"
                        .into()
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
        let count = Arc::clone(&session)
            .open_state(path)
            .await?
            .snapshot()
            .rows
            .len();
        println!("PASS local TUI listing smoke test ({count} entries)");
        return Ok(());
    }

    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut app = App {
        session,
        state: None,
        path: path.clone(),
        rows: Vec::new(),
        selected: 0,
        loading: false,
        loading_tree_paths: HashSet::new(),
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
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Down | KeyCode::Char('j') => app.move_selection(true),
                KeyCode::Up | KeyCode::Char('k') => app.move_selection(false),
                KeyCode::Char('h') => app.move_to_parent(),
                KeyCode::Char('l') => app.move_right(sender.clone()),
                KeyCode::Char('o') => app.toggle_selected(sender.clone()),
                KeyCode::Enter => app.open_selected(sender.clone()),
                _ => {}
            }
        }
    }
}

fn sibling_server() -> Result<PathBuf, Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    Ok(executable
        .parent()
        .ok_or("UI executable has no parent directory")?
        .join("file-peeker-server"))
}

fn open_action(rows: &[StateRow], selected: usize) -> Option<OpenAction> {
    rows.get(selected).map(|row| {
        if row.entry.navigable {
            OpenAction::Directory(row.entry.path.clone())
        } else {
            OpenAction::File(row.entry.path.clone())
        }
    })
}

fn parent_index(rows: &[StateRow], selected: usize) -> Option<usize> {
    let parent_path = rows.get(selected)?.parent_path.as_deref()?;
    rows.iter().position(|row| row.entry.path == parent_path)
}

fn first_child_index(rows: &[StateRow], selected: usize) -> Option<usize> {
    let path = rows.get(selected)?.entry.path.as_str();
    rows.iter()
        .position(|row| row.parent_path.as_deref() == Some(path))
}

fn right_action(
    rows: &[StateRow],
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

    use file_peeker_client::{DirectoryEntry, EntryKind, StateRow};

    use super::{
        OpenAction, RightAction, first_child_index, open_action, parent_index, right_action,
    };

    fn row(path: &str, parent_path: Option<&str>, navigable: bool) -> StateRow {
        StateRow {
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
}
