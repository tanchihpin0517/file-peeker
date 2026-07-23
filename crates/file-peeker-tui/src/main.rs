use std::{error::Error, io, time::Duration};

use clap::{CommandFactory, Parser};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

mod app;
mod browser_context;

use crate::{app::App, browser_context::BrowserContextEvent};

const EVENT_CHANNEL_CAPACITY: usize = 8;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let (sender, mut receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let help = Cli::command().render_help().to_string();
    let mut app = App::new(sender, help);
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
                KeyCode::Char('R') => app.refresh_active_context(),
                KeyCode::Down | KeyCode::Char('j') => app.move_active_selection(true),
                KeyCode::Up | KeyCode::Char('k') => app.move_active_selection(false),
                KeyCode::Char('h') => app.leave_active_directory(),
                KeyCode::Char('l') => app.enter_active_selection(),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser, error::ErrorKind};

    use super::Cli;

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
        Cli::command().debug_assert();
    }
}
