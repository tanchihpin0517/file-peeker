//! The `file-peeker` binary: a Ratatui frontend over
//! [`Client`](crate::Client) and [`Session`](crate::Session).

use std::{error::Error, io, time::Duration};

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

mod app;
mod browser_context;

use self::app::{App, InputMode};

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

/// Runs the `file-peeker` terminal browser.
///
/// # Errors
///
/// Returns an error if the initial Session fails to start, the terminal cannot
/// be initialized, or the event loop or shutdown fails.
///
/// # Panics
///
/// Panics if restoring the terminal after the event loop fails.
#[tokio::main]
pub async fn run() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let mut app = App::new();
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
    let run_result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    let shutdown_result = app.shutdown().await;
    run_result?;
    shutdown_result.map_err(Into::into)
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        app.pump_events(EVENT_CHANNEL_CAPACITY);
        terminal.draw(|frame| app.render(frame))?;
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key_action(key.code, key.modifiers, app.input_mode()) {
                KeyAction::Quit => return Ok(()),
                KeyAction::Refresh => app.refresh_active_context(),
                KeyAction::MoveDown => app.move_active_selection(true),
                KeyAction::MoveUp => app.move_active_selection(false),
                KeyAction::HalfPageDown => app.scroll_active_half_page(true),
                KeyAction::HalfPageUp => app.scroll_active_half_page(false),
                KeyAction::LeaveDirectory => app.leave_active_directory(),
                KeyAction::EnterSelection => app.enter_active_selection(),
                KeyAction::ActivateSelection => app.activate_active_selection(),
                KeyAction::ConfirmOpen => app.confirm_active_open(),
                KeyAction::CancelOpen => app.cancel_active_open_confirmation(),
                KeyAction::PreviousContext => app.switch_context(false),
                KeyAction::NextContext => app.switch_context(true),
                KeyAction::NewBrowser => app.open_new_browser_for_active(),
                KeyAction::CloseContext => app.close_active_context(),
                KeyAction::OpenSessions => app.open_session_manager(),
                KeyAction::DismissOverlay => app.dismiss_overlay(),
                KeyAction::MoveSessionDown => app.move_session_selection(true),
                KeyAction::MoveSessionUp => app.move_session_selection(false),
                KeyAction::ActivateSession => app.activate_selected_session(),
                KeyAction::NewBrowserForSession => {
                    app.open_new_browser_for_selected_session();
                }
                KeyAction::AddSession => app.open_add_session(),
                KeyAction::DeleteSession => app.delete_selected_session(),
                KeyAction::MoveTargetDown => app.move_target_selection(true),
                KeyAction::MoveTargetUp => app.move_target_selection(false),
                KeyAction::SubmitTarget => app.submit_target_choice(),
                KeyAction::SubmitText => app.submit_text_input(),
                KeyAction::Backspace => app.input_backspace(),
                KeyAction::Input(character) => app.input_character(character),
                KeyAction::CancelDialog => app.cancel_dialog(),
                KeyAction::ConfirmSessionClose => app.confirm_session_close(),
                KeyAction::Yank => app.yank_active_selection(),
                KeyAction::Paste => app.paste_into_active_context(),
                KeyAction::ConfirmTransferOverwrite => app.confirm_transfer_overwrite(),
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
    HalfPageDown,
    HalfPageUp,
    LeaveDirectory,
    EnterSelection,
    ActivateSelection,
    ConfirmOpen,
    CancelOpen,
    PreviousContext,
    NextContext,
    NewBrowser,
    CloseContext,
    OpenSessions,
    DismissOverlay,
    MoveSessionDown,
    MoveSessionUp,
    ActivateSession,
    NewBrowserForSession,
    AddSession,
    DeleteSession,
    MoveTargetDown,
    MoveTargetUp,
    SubmitTarget,
    SubmitText,
    Backspace,
    Input(char),
    CancelDialog,
    ConfirmSessionClose,
    Yank,
    Paste,
    ConfirmTransferOverwrite,
    Ignore,
}

fn key_action(code: KeyCode, modifiers: KeyModifiers, mode: InputMode) -> KeyAction {
    match mode {
        InputMode::FileOpenConfirmation => {
            return match code {
                KeyCode::Char('q') | KeyCode::Esc => KeyAction::CancelOpen,
                KeyCode::Char('o') => KeyAction::ConfirmOpen,
                _ => KeyAction::Ignore,
            };
        }
        InputMode::SessionCloseConfirmation => {
            return match code {
                KeyCode::Enter | KeyCode::Char('y') => KeyAction::ConfirmSessionClose,
                KeyCode::Char('q') | KeyCode::Esc => KeyAction::CancelDialog,
                _ => KeyAction::Ignore,
            };
        }
        InputMode::TransferOverwriteConfirmation => {
            return match code {
                KeyCode::Enter | KeyCode::Char('y') => KeyAction::ConfirmTransferOverwrite,
                KeyCode::Char('q') | KeyCode::Esc => KeyAction::CancelDialog,
                _ => KeyAction::Ignore,
            };
        }
        InputMode::TargetChooser => {
            return match code {
                KeyCode::Down | KeyCode::Char('j') => KeyAction::MoveTargetDown,
                KeyCode::Up | KeyCode::Char('k') => KeyAction::MoveTargetUp,
                KeyCode::Enter => KeyAction::SubmitTarget,
                KeyCode::Esc => KeyAction::CancelDialog,
                _ => KeyAction::Ignore,
            };
        }
        InputMode::RemoteInput | InputMode::NewBrowser => {
            return match code {
                KeyCode::Enter => KeyAction::SubmitText,
                KeyCode::Backspace => KeyAction::Backspace,
                KeyCode::Esc => KeyAction::CancelDialog,
                KeyCode::Char(character)
                    if !modifiers.intersects(
                        KeyModifiers::CONTROL
                            | KeyModifiers::ALT
                            | KeyModifiers::SUPER
                            | KeyModifiers::HYPER
                            | KeyModifiers::META,
                    ) =>
                {
                    KeyAction::Input(character)
                }
                _ => KeyAction::Ignore,
            };
        }
        InputMode::SessionManager => {
            return match code {
                KeyCode::Char('q' | 'S') | KeyCode::Esc => KeyAction::DismissOverlay,
                KeyCode::Down | KeyCode::Char('j') => KeyAction::MoveSessionDown,
                KeyCode::Up | KeyCode::Char('k') => KeyAction::MoveSessionUp,
                KeyCode::Enter => KeyAction::ActivateSession,
                KeyCode::Char('n') => KeyAction::NewBrowserForSession,
                KeyCode::Char('a') => KeyAction::AddSession,
                KeyCode::Char('d') => KeyAction::DeleteSession,
                _ => KeyAction::Ignore,
            };
        }
        InputMode::Main => {}
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        match code {
            KeyCode::Char('d') => return KeyAction::HalfPageDown,
            KeyCode::Char('u') => return KeyAction::HalfPageUp,
            _ => {}
        }
    }
    match code {
        KeyCode::Char('q') | KeyCode::Esc => KeyAction::Quit,
        KeyCode::Char('R') => KeyAction::Refresh,
        KeyCode::Down | KeyCode::Char('j') => KeyAction::MoveDown,
        KeyCode::Up | KeyCode::Char('k') => KeyAction::MoveUp,
        KeyCode::Char('h') => KeyAction::LeaveDirectory,
        KeyCode::Char('l') => KeyAction::EnterSelection,
        KeyCode::Char('o') => KeyAction::ActivateSelection,
        KeyCode::Char('[') => KeyAction::PreviousContext,
        KeyCode::Char(']') => KeyAction::NextContext,
        KeyCode::Char('n') => KeyAction::NewBrowser,
        KeyCode::Char('x') => KeyAction::CloseContext,
        KeyCode::Char('S') => KeyAction::OpenSessions,
        KeyCode::Char('y') => KeyAction::Yank,
        KeyCode::Char('p') => KeyAction::Paste,
        _ => KeyAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser, error::ErrorKind};
    use crossterm::event::{KeyCode, KeyModifiers};

    use super::{Cli, InputMode, KeyAction, key_action};

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
        assert_eq!(
            key_action(
                KeyCode::Esc,
                KeyModifiers::NONE,
                InputMode::FileOpenConfirmation
            ),
            KeyAction::CancelOpen
        );
        assert_eq!(
            key_action(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                InputMode::FileOpenConfirmation
            ),
            KeyAction::CancelOpen
        );
        assert_eq!(
            key_action(
                KeyCode::Char('o'),
                KeyModifiers::NONE,
                InputMode::FileOpenConfirmation
            ),
            KeyAction::ConfirmOpen
        );
        assert_eq!(
            key_action(
                KeyCode::Char('j'),
                KeyModifiers::NONE,
                InputMode::FileOpenConfirmation
            ),
            KeyAction::Ignore
        );
        assert_eq!(
            key_action(
                KeyCode::Char('d'),
                KeyModifiers::CONTROL,
                InputMode::FileOpenConfirmation
            ),
            KeyAction::Ignore
        );
    }

    #[test]
    fn quit_keys_still_exit_without_a_confirmation() {
        assert_eq!(
            key_action(KeyCode::Esc, KeyModifiers::NONE, InputMode::Main),
            KeyAction::Quit
        );
        assert_eq!(
            key_action(KeyCode::Char('q'), KeyModifiers::NONE, InputMode::Main),
            KeyAction::Quit
        );
    }

    #[test]
    fn main_copy_keys_and_overwrite_modal_are_isolated() {
        assert_eq!(
            key_action(KeyCode::Char('y'), KeyModifiers::NONE, InputMode::Main),
            KeyAction::Yank
        );
        assert_eq!(
            key_action(KeyCode::Char('p'), KeyModifiers::NONE, InputMode::Main),
            KeyAction::Paste
        );
        assert_eq!(
            key_action(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
                InputMode::TransferOverwriteConfirmation
            ),
            KeyAction::ConfirmTransferOverwrite
        );
        assert_eq!(
            key_action(
                KeyCode::Char('p'),
                KeyModifiers::NONE,
                InputMode::TransferOverwriteConfirmation
            ),
            KeyAction::Ignore
        );
        assert_eq!(
            key_action(
                KeyCode::Esc,
                KeyModifiers::NONE,
                InputMode::TransferOverwriteConfirmation
            ),
            KeyAction::CancelDialog
        );
    }

    #[test]
    fn control_d_and_u_scroll_by_half_a_page() {
        assert_eq!(
            key_action(KeyCode::Char('d'), KeyModifiers::CONTROL, InputMode::Main),
            KeyAction::HalfPageDown
        );
        assert_eq!(
            key_action(KeyCode::Char('u'), KeyModifiers::CONTROL, InputMode::Main),
            KeyAction::HalfPageUp
        );
        assert_eq!(
            key_action(KeyCode::Char('d'), KeyModifiers::NONE, InputMode::Main),
            KeyAction::Ignore
        );
        assert_eq!(
            key_action(KeyCode::Char('u'), KeyModifiers::NONE, InputMode::Main),
            KeyAction::Ignore
        );
    }

    #[test]
    fn overlays_capture_keys_that_quit_or_navigate_in_main_mode() {
        assert_eq!(
            key_action(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                InputMode::SessionManager
            ),
            KeyAction::DismissOverlay
        );
        assert_eq!(
            key_action(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
                InputMode::SessionManager
            ),
            KeyAction::Ignore
        );
        assert_eq!(
            key_action(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                InputMode::RemoteInput
            ),
            KeyAction::Input('q')
        );
        assert_eq!(
            key_action(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                InputMode::SessionCloseConfirmation
            ),
            KeyAction::CancelDialog
        );
    }
}
