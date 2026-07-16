use std::{error::Error, path::PathBuf, sync::Arc, time::Duration};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use file_peeker_client::{BrowserClient, ClientConfig, ClientError, DirectoryEntry};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use tokio::sync::mpsc;

#[derive(Debug)]
enum AppEvent {
    Entry(u64, DirectoryEntry),
    Finished(u64),
    Failed(u64, ClientError),
}

#[derive(Debug)]
struct App {
    client: Arc<BrowserClient>,
    path: String,
    entries: Vec<DirectoryEntry>,
    selected: usize,
    loading: bool,
    error: Option<String>,
    generation: u64,
}

impl App {
    fn open(&mut self, path: String, events: mpsc::UnboundedSender<AppEvent>) {
        self.generation += 1;
        let generation = self.generation;
        self.path.clone_from(&path);
        self.entries.clear();
        self.selected = 0;
        self.loading = true;
        self.error = None;
        let client = Arc::clone(&self.client);

        tokio::spawn(async move {
            let result = async {
                let listing = client.start_listing(path).await?;
                while let Some(entry) = listing.next_entry().await? {
                    if events.send(AppEvent::Entry(generation, entry)).is_err() {
                        return Ok(());
                    }
                }
                let _ = events.send(AppEvent::Finished(generation));
                Ok::<(), ClientError>(())
            }
            .await;
            if let Err(error) = result {
                let _ = events.send(AppEvent::Failed(generation, error));
            }
        });
    }

    fn update(&mut self, event: AppEvent) {
        match event {
            AppEvent::Entry(generation, entry) if generation == self.generation => {
                self.entries.push(entry);
            }
            AppEvent::Finished(generation) if generation == self.generation => {
                self.loading = false;
            }
            AppEvent::Failed(generation, error) if generation == self.generation => {
                self.loading = false;
                self.error = Some(error.to_string());
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, down: bool) {
        if self.entries.is_empty() {
            self.selected = 0;
        } else if down {
            self.selected = (self.selected + 1).min(self.entries.len() - 1);
        } else {
            self.selected = self.selected.saturating_sub(1);
        }
    }

    fn selected_directory(&self) -> Option<String> {
        selected_directory(&self.entries, self.selected)
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

        let items = self.entries.iter().map(|entry| {
            let marker = if entry.navigable { "▸ " } else { "  " };
            ListItem::new(Line::from(format!("{marker}{}", entry.name)))
        });
        let mut state =
            ListState::default().with_selected((!self.entries.is_empty()).then_some(self.selected));
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("> "),
            body,
            &mut state,
        );

        let status = self.error.as_deref().map_or_else(
            || {
                if self.loading {
                    "Loading…".into()
                } else {
                    "↑/↓ or j/k: select  Enter: open  q/Esc: quit".into()
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
    let client = BrowserClient::start(ClientConfig {
        server_executable_path: server.to_string_lossy().into_owned(),
    })
    .await?;
    if smoke {
        let listing = client.start_listing(path).await?;
        let mut count = 0_usize;
        while listing.next_entry().await?.is_some() {
            count += 1;
        }
        println!("PASS local TUI listing smoke test ({count} entries)");
        return Ok(());
    }

    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut app = App {
        client,
        path: path.clone(),
        entries: Vec::new(),
        selected: 0,
        loading: false,
        error: None,
        generation: 0,
    };
    app.open(path, sender.clone());

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
                KeyCode::Enter => {
                    if let Some(path) = app.selected_directory() {
                        app.open(path, sender.clone());
                    }
                }
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

fn selected_directory(entries: &[DirectoryEntry], selected: usize) -> Option<String> {
    entries
        .get(selected)
        .filter(|entry| entry.navigable)
        .map(|entry| entry.path.clone())
}

#[cfg(test)]
mod tests {
    use file_peeker_client::{DirectoryEntry, EntryKind};

    use super::selected_directory;

    #[test]
    fn only_navigable_entries_can_be_opened() {
        let entry = DirectoryEntry {
            path: "/tmp/file".into(),
            name: "file".into(),
            kind: EntryKind::File,
            navigable: false,
        };
        let mut entries = vec![entry];
        assert!(selected_directory(&entries, 0).is_none());
        entries[0].navigable = true;
        assert_eq!(
            selected_directory(&entries, 0).as_deref(),
            Some("/tmp/file")
        );
    }
}
