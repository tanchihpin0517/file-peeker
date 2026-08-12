//! The `file-peeker` binary: a Ratatui frontend over
//! [`Client`](crate::Client) and its connections.

use std::{error::Error, io, time::Duration};

use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt as _;
use ratatui::DefaultTerminal;
use tokio::{sync::mpsc, task::JoinHandle};

mod app;
mod browser_context;

use self::app::{App, InputMode};

/// Browser listing events stay on a bounded channel to preserve
/// filesystem/network backpressure, but the bound is deep enough for a listing
/// task to run a frame ahead of the renderer. A directory the server can stream
/// faster than the terminal repaints then arrives in one redraw instead of being
/// metered out over many.
///
/// A slot carries a group of entries rather than a single one, so this is a far
/// larger number of entries than it looks: the bound in entries is this multiplied
/// by the group size.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// How long the loop keeps collecting events after the first one wakes it, so a
/// streaming listing costs one redraw per frame rather than one per entry.
const FRAME_BUDGET: Duration = Duration::from_millis(16);

/// Deep enough to hold the auto-repeat burst of a held scroll key, so the loop
/// applies the whole burst before it redraws.
const TERMINAL_EVENT_CAPACITY: usize = 256;

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
/// Returns an error if the initial connection fails to open, the terminal cannot
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
    let run_result = event_loop(&mut terminal, &mut app).await;
    ratatui::restore();
    let shutdown_result = app.shutdown().await;
    run_result?;
    shutdown_result.map_err(Into::into)
}

async fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let (mut terminal_events, reader) = spawn_terminal_reader();
    let result = draw_loop(terminal, app, &mut terminal_events).await;
    // Aborting drops the EventStream, which is what shuts down its reader thread.
    reader.abort();
    let _ = reader.await;
    result
}

/// Reads terminal events in their own task, forwarding them over a channel.
///
/// One task owns the stream and only ever awaits it to completion, which is
/// deliberate. [`EventStream`] registers a wake task on its first `Pending` and
/// suppresses further registrations until that one fires, so polling it from a
/// `select!` arm that loses the race -- or peeking at it with `now_or_never` --
/// can strand the wake-up on a waker that is already gone and leave the UI
/// frozen. A channel is something the loop can drain without that hazard.
fn spawn_terminal_reader() -> (mpsc::Receiver<io::Result<Event>>, JoinHandle<()>) {
    let (sender, receiver) = mpsc::channel(TERMINAL_EVENT_CAPACITY);
    let reader = tokio::spawn(async move {
        let mut events = EventStream::new();
        while let Some(event) = events.next().await {
            if sender.send(event).await.is_err() {
                break;
            }
        }
    });
    (receiver, reader)
}

/// Draws, then sleeps until something actually happens: a terminal event, or a
/// browser or lifecycle event. Nothing polls on a fixed interval, so an idle
/// browser costs no redraws and an arriving listing waits on no timer.
async fn draw_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    terminal_events: &mut mpsc::Receiver<io::Result<Event>>,
) -> io::Result<()> {
    loop {
        terminal.draw(|frame| app.render(frame))?;
        let next_event = tokio::select! {
            // Both branches are cancel safe: a dropped `recv` leaves the message
            // it did not take queued for the next call.
            terminal_event = terminal_events.recv() => terminal_event,
            () = app.next_events(FRAME_BUDGET) => continue,
        };
        // The terminal event stream ends when stdin closes, which leaves no way
        // to quit.
        let Some(next_event) = next_event else {
            return Ok(());
        };
        if apply_terminal_events(app, next_event, terminal_events)? == Flow::Quit {
            return Ok(());
        }
    }
}

/// Applies one terminal event and every one already queued behind it, so the
/// caller redraws once for the batch.
///
/// A held scroll key can deliver a clump of presses between two frames. Drawing
/// per press makes the screen chase the backlog for as long as the key is down,
/// which reads as scrolling that lags the finger and overshoots on release.
fn apply_terminal_events(
    app: &mut App,
    first: io::Result<Event>,
    terminal_events: &mut mpsc::Receiver<io::Result<Event>>,
) -> io::Result<Flow> {
    let mut pending = Some(first);
    while let Some(terminal_event) = pending {
        if let Event::Key(key) = terminal_event?
            && key.kind == KeyEventKind::Press
            && apply_key(app, key) == Flow::Quit
        {
            return Ok(Flow::Quit);
        }
        pending = terminal_events.try_recv().ok();
    }
    Ok(Flow::Continue)
}

/// Applies one key press, reporting whether the loop should keep running.
fn apply_key(app: &mut App, key: KeyEvent) -> Flow {
    match key_action(key.code, key.modifiers, app.input_mode()) {
        KeyAction::Quit => return Flow::Quit,
        KeyAction::Refresh => app.refresh_active_context(),
        KeyAction::MoveDown => app.move_active_selection(true),
        KeyAction::MoveUp => app.move_active_selection(false),
        KeyAction::HalfPageDown => app.scroll_active_half_page(true),
        KeyAction::HalfPageUp => app.scroll_active_half_page(false),
        KeyAction::LeaveDirectory => app.leave_active_directory(),
        KeyAction::EnterSelection => app.enter_active_selection(),
        KeyAction::ActivateSelection => app.activate_active_selection(),
        KeyAction::PreviousContext => app.switch_context(false),
        KeyAction::NextContext => app.switch_context(true),
        KeyAction::NewBrowser => app.open_new_browser_for_active(),
        KeyAction::CloseContext => app.close_active_context(),
        KeyAction::OpenConnections => app.open_connection_manager(),
        KeyAction::DismissOverlay => app.dismiss_overlay(),
        KeyAction::MoveConnectionDown => app.move_connection_selection(true),
        KeyAction::MoveConnectionUp => app.move_connection_selection(false),
        KeyAction::ActivateConnection => app.activate_selected_connection(),
        KeyAction::NewBrowserForConnection => {
            app.open_new_browser_for_selected_connection();
        }
        KeyAction::AddConnection => app.open_add_connection(),
        KeyAction::DeleteConnection => app.delete_selected_connection(),
        KeyAction::MoveTargetDown => app.move_target_selection(true),
        KeyAction::MoveTargetUp => app.move_target_selection(false),
        KeyAction::SubmitTarget => app.submit_target_choice(),
        KeyAction::SubmitText => app.submit_text_input(),
        KeyAction::Backspace => app.input_backspace(),
        KeyAction::Input(character) => app.input_character(character),
        KeyAction::CancelDialog => app.cancel_dialog(),
        KeyAction::ConfirmConnectionClose => app.confirm_connection_close(),
        KeyAction::Yank => app.yank_active_selection(),
        KeyAction::Paste => app.paste_into_active_context(),
        KeyAction::OpenFilter => app.open_filter(),
        KeyAction::FilterInput(character) => app.extend_filter(character),
        KeyAction::FilterBackspace => app.shorten_filter(),
        KeyAction::CompleteFilter => app.complete_filter(),
        KeyAction::DescendFilter => app.descend_filter(),
        KeyAction::SubmitFilter => app.submit_filter(),
        KeyAction::AbandonFilter => app.abandon_filter(),
        KeyAction::ConfirmTransferOverwrite => app.confirm_transfer_overwrite(),
        KeyAction::Ignore => {}
    }
    Flow::Continue
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flow {
    Continue,
    Quit,
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
    PreviousContext,
    NextContext,
    NewBrowser,
    CloseContext,
    OpenConnections,
    DismissOverlay,
    MoveConnectionDown,
    MoveConnectionUp,
    ActivateConnection,
    NewBrowserForConnection,
    AddConnection,
    DeleteConnection,
    MoveTargetDown,
    MoveTargetUp,
    SubmitTarget,
    SubmitText,
    Backspace,
    Input(char),
    CancelDialog,
    ConfirmConnectionClose,
    Yank,
    Paste,
    ConfirmTransferOverwrite,
    OpenFilter,
    FilterInput(char),
    FilterBackspace,
    CompleteFilter,
    DescendFilter,
    SubmitFilter,
    AbandonFilter,
    Ignore,
}

fn key_action(code: KeyCode, modifiers: KeyModifiers, mode: InputMode) -> KeyAction {
    match mode {
        InputMode::ConnectionCloseConfirmation => {
            return match code {
                KeyCode::Enter | KeyCode::Char('y') => KeyAction::ConfirmConnectionClose,
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
                KeyCode::Char(character) if types_text(modifiers) => KeyAction::Input(character),
                _ => KeyAction::Ignore,
            };
        }
        // The filter prompt takes the letters, so moving and scrolling the matches
        // without leaving it is left to the arrows and to the readline chords a
        // prompt is expected to answer.
        InputMode::Filter => {
            if modifiers.contains(KeyModifiers::CONTROL) {
                return match code {
                    KeyCode::Char('n') => KeyAction::MoveDown,
                    KeyCode::Char('p') => KeyAction::MoveUp,
                    KeyCode::Char('d') => KeyAction::HalfPageDown,
                    KeyCode::Char('u') => KeyAction::HalfPageUp,
                    _ => KeyAction::Ignore,
                };
            }
            return match code {
                KeyCode::Enter => KeyAction::SubmitFilter,
                KeyCode::Esc => KeyAction::AbandonFilter,
                KeyCode::Backspace => KeyAction::FilterBackspace,
                KeyCode::Tab => KeyAction::CompleteFilter,
                // A name never contains a separator, so the key that could only
                // match nothing steps into the selected directory instead.
                KeyCode::Char('/') => KeyAction::DescendFilter,
                KeyCode::Down => KeyAction::MoveDown,
                KeyCode::Up => KeyAction::MoveUp,
                KeyCode::Char(character) if types_text(modifiers) => {
                    KeyAction::FilterInput(character)
                }
                _ => KeyAction::Ignore,
            };
        }
        InputMode::ConnectionManager => {
            return match code {
                KeyCode::Char('q' | 'S') | KeyCode::Esc => KeyAction::DismissOverlay,
                KeyCode::Down | KeyCode::Char('j') => KeyAction::MoveConnectionDown,
                KeyCode::Up | KeyCode::Char('k') => KeyAction::MoveConnectionUp,
                KeyCode::Enter => KeyAction::ActivateConnection,
                KeyCode::Char('n') => KeyAction::NewBrowserForConnection,
                KeyCode::Char('a') => KeyAction::AddConnection,
                KeyCode::Char('d') => KeyAction::DeleteConnection,
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
        KeyCode::Char('S') => KeyAction::OpenConnections,
        KeyCode::Char('y') => KeyAction::Yank,
        KeyCode::Char('p') => KeyAction::Paste,
        KeyCode::Char('/') => KeyAction::OpenFilter,
        _ => KeyAction::Ignore,
    }
}

/// Whether a character key is the user typing text rather than reaching for a
/// modified shortcut.
fn types_text(modifiers: KeyModifiers) -> bool {
    !modifiers.intersects(
        KeyModifiers::CONTROL
            | KeyModifiers::ALT
            | KeyModifiers::SUPER
            | KeyModifiers::HYPER
            | KeyModifiers::META,
    )
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser, error::ErrorKind};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc;

    use super::{
        App, Cli, FRAME_BUDGET, Flow, InputMode, KeyAction, TERMINAL_EVENT_CAPACITY,
        apply_terminal_events, key_action,
    };
    use crate::tui::browser_context::ListingStatus;

    /// One draw covers one call to [`apply_terminal_events`], so a burst that
    /// arrives between two frames has to be applied in a single call. Applying one
    /// press per draw makes the screen chase the backlog for as long as the key is
    /// down, which reads as scrolling that lags and then overshoots.
    #[tokio::test]
    async fn a_burst_of_scroll_keys_is_applied_before_the_next_draw() {
        const PRESSES: usize = 60;

        let directory = tempfile::tempdir().unwrap();
        for index in 0..200 {
            tokio::fs::write(directory.path().join(format!("entry-{index:03}")), b"")
                .await
                .unwrap();
        }
        let mut app = App::new();
        app.start(Some(directory.path().to_str().unwrap()))
            .await
            .unwrap();
        while app
            .active_context()
            .is_some_and(|context| context.status() != &ListingStatus::Complete)
        {
            app.next_events(FRAME_BUDGET).await;
        }

        let (sender, mut terminal_events) = mpsc::channel(TERMINAL_EVENT_CAPACITY);
        for _ in 0..PRESSES {
            sender.send(Ok(press(KeyCode::Char('j')))).await.unwrap();
        }

        let first = terminal_events.recv().await.unwrap();
        let flow = apply_terminal_events(&mut app, first, &mut terminal_events).unwrap();

        assert_eq!(flow, Flow::Continue);
        assert_eq!(
            app.active_context().unwrap().effective_selection(),
            Some(PRESSES),
            "one call should apply every press already queued"
        );
        assert!(
            terminal_events.try_recv().is_err(),
            "the burst should be drained, leaving nothing for the next frame"
        );

        // Quitting mid-burst reports back rather than applying the rest.
        sender.send(Ok(press(KeyCode::Char('q')))).await.unwrap();
        sender.send(Ok(press(KeyCode::Char('j')))).await.unwrap();
        let first = terminal_events.recv().await.unwrap();
        assert_eq!(
            apply_terminal_events(&mut app, first, &mut terminal_events).unwrap(),
            Flow::Quit
        );
        assert_eq!(
            app.active_context().unwrap().effective_selection(),
            Some(PRESSES),
            "the press behind the quit should not have moved the selection"
        );

        app.shutdown().await.unwrap();
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
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
        Cli::command().debug_assert();
    }

    #[test]
    fn confirmation_keys_cancel_or_confirm_without_quitting() {
        assert_eq!(
            key_action(
                KeyCode::Esc,
                KeyModifiers::NONE,
                InputMode::ConnectionCloseConfirmation
            ),
            KeyAction::CancelDialog
        );
        assert_eq!(
            key_action(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                InputMode::ConnectionCloseConfirmation
            ),
            KeyAction::CancelDialog
        );
        assert_eq!(
            key_action(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
                InputMode::ConnectionCloseConfirmation
            ),
            KeyAction::ConfirmConnectionClose
        );
        assert_eq!(
            key_action(
                KeyCode::Char('j'),
                KeyModifiers::NONE,
                InputMode::ConnectionCloseConfirmation
            ),
            KeyAction::Ignore
        );
        assert_eq!(
            key_action(
                KeyCode::Char('d'),
                KeyModifiers::CONTROL,
                InputMode::ConnectionCloseConfirmation
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

    /// The prompt has to take the letters that are commands in Main mode, or
    /// filtering for a name with a `q` or an `x` in it would quit or close a tab.
    /// The arrows stay navigation, which is what walks the matches without
    /// leaving the prompt.
    #[test]
    fn the_filter_prompt_takes_the_letters_and_leaves_the_arrows_alone() {
        assert_eq!(
            key_action(KeyCode::Char('/'), KeyModifiers::NONE, InputMode::Main),
            KeyAction::OpenFilter
        );
        for character in ['q', 'x', 'j', 'R'] {
            assert_eq!(
                key_action(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                    InputMode::Filter
                ),
                KeyAction::FilterInput(character)
            );
        }
        for (code, action) in [
            (KeyCode::Tab, KeyAction::CompleteFilter),
            (KeyCode::Char('/'), KeyAction::DescendFilter),
            (KeyCode::Down, KeyAction::MoveDown),
            (KeyCode::Up, KeyAction::MoveUp),
            (KeyCode::Enter, KeyAction::SubmitFilter),
            (KeyCode::Esc, KeyAction::AbandonFilter),
            (KeyCode::Backspace, KeyAction::FilterBackspace),
        ] {
            assert_eq!(
                key_action(code, KeyModifiers::NONE, InputMode::Filter),
                action
            );
        }
    }

    /// A prompt is expected to answer the readline chords, and the list under it
    /// still has to scroll, so both keep working while the letters are taken.
    #[test]
    fn the_filter_prompt_answers_the_readline_and_scroll_chords() {
        for (character, action) in [
            ('n', KeyAction::MoveDown),
            ('p', KeyAction::MoveUp),
            ('d', KeyAction::HalfPageDown),
            ('u', KeyAction::HalfPageUp),
        ] {
            assert_eq!(
                key_action(
                    KeyCode::Char(character),
                    KeyModifiers::CONTROL,
                    InputMode::Filter
                ),
                action
            );
            assert_eq!(
                key_action(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                    InputMode::Filter
                ),
                KeyAction::FilterInput(character),
                "unmodified, the same key is typed into the query"
            );
        }
        assert_eq!(
            key_action(KeyCode::Char('x'), KeyModifiers::CONTROL, InputMode::Filter),
            KeyAction::Ignore,
            "a chord the prompt has no use for is not typed text"
        );
    }

    #[test]
    fn overlays_capture_keys_that_quit_or_navigate_in_main_mode() {
        assert_eq!(
            key_action(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                InputMode::ConnectionManager
            ),
            KeyAction::DismissOverlay
        );
        assert_eq!(
            key_action(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
                InputMode::ConnectionManager
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
                InputMode::ConnectionCloseConfirmation
            ),
            KeyAction::CancelDialog
        );
    }
}
