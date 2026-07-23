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
            match key_action(key.code, app.has_open_confirmation()) {
                KeyAction::Quit => return Ok(()),
                KeyAction::Refresh => app.refresh_active_context(),
                KeyAction::MoveDown => app.move_active_selection(true),
                KeyAction::MoveUp => app.move_active_selection(false),
                KeyAction::LeaveDirectory => app.leave_active_directory(),
                KeyAction::EnterSelection => app.enter_active_selection(),
                KeyAction::ActivateSelection => app.activate_active_selection(),
                KeyAction::ConfirmOpen => app.confirm_active_open(),
                KeyAction::CancelOpen => app.cancel_active_open_confirmation(),
                KeyAction::Ignore => {}
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyAction {
    Quit,
    Refresh,
    MoveDown,
    MoveUp,
    LeaveDirectory,
    EnterSelection,
    ActivateSelection,
    ConfirmOpen,
    CancelOpen,
    Ignore,
}

fn key_action(code: KeyCode, has_open_confirmation: bool) -> KeyAction {
    if has_open_confirmation {
        return match code {
            KeyCode::Char('q') | KeyCode::Esc => KeyAction::CancelOpen,
            KeyCode::Char('o') => KeyAction::ConfirmOpen,
            _ => KeyAction::Ignore,
        };
    }
    match code {
        KeyCode::Char('q') | KeyCode::Esc => KeyAction::Quit,
        KeyCode::Char('R') => KeyAction::Refresh,
        KeyCode::Down | KeyCode::Char('j') => KeyAction::MoveDown,
        KeyCode::Up | KeyCode::Char('k') => KeyAction::MoveUp,
        KeyCode::Char('h') => KeyAction::LeaveDirectory,
        KeyCode::Char('l') => KeyAction::EnterSelection,
        KeyCode::Char('o') => KeyAction::ActivateSelection,
        _ => KeyAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser, error::ErrorKind};
    use crossterm::event::KeyCode;

    use super::{Cli, KeyAction, key_action};

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

    #[test]
    fn confirmation_keys_cancel_or_confirm_without_quitting() {
        assert_eq!(key_action(KeyCode::Esc, true), KeyAction::CancelOpen);
        assert_eq!(key_action(KeyCode::Char('q'), true), KeyAction::CancelOpen);
        assert_eq!(key_action(KeyCode::Char('o'), true), KeyAction::ConfirmOpen);
        assert_eq!(key_action(KeyCode::Char('j'), true), KeyAction::Ignore);
    }

    #[test]
    fn quit_keys_still_exit_without_a_confirmation() {
        assert_eq!(key_action(KeyCode::Esc, false), KeyAction::Quit);
        assert_eq!(key_action(KeyCode::Char('q'), false), KeyAction::Quit);
    }
}
