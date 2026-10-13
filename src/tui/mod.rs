//! The terminal front end: `fpk` with no command.
//!
//! Step 2 of the build order: the shell later steps draw into. It attaches to
//! the server — starting one if none runs — then takes the terminal and shows a
//! machine list holding `local` beside an empty body. Quitting detaches; the
//! server keeps running.
//!
//! - [`app`] is the state and its transitions, pure.
//! - [`view`] draws it, pure.
//! - [`connection`] holds one machine's attachment and reconnects it.

mod app;
mod connection;
mod view;

use std::{
    io::{self, IsTerminal as _},
    process::ExitCode,
    time::{Duration, Instant},
};

use crossterm::event::{Event, EventStream};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;

use self::{
    app::{App, MachineState},
    connection::Backoff,
};
use crate::{client, home::Home};

/// How often a disconnected screen redraws, so its countdown moves.
const TICK: Duration = Duration::from_millis(250);

pub(crate) fn run(home: &Home) -> ExitCode {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!(
            "fpk: the terminal UI needs a terminal; use `fpk server start` to start the server \
             without one"
        );
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("fpk: cannot start the runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        // Attach before taking the terminal, so a server that cannot start says
        // so on a normal screen rather than inside one that is about to vanish.
        let attached = match client::attach_or_start(home).await {
            Ok(attached) => attached,
            Err(error) => {
                eprintln!("fpk: {error}");
                return ExitCode::FAILURE;
            }
        };
        if attached.started {
            println!(
                "started fpk server {} (pid {})",
                attached.server_version, attached.pid
            );
        }

        // `init` also installs a panic hook that restores the terminal first.
        let mut terminal = ratatui::init();
        let outcome = event_loop(&mut terminal, home).await;
        ratatui::restore();

        match outcome {
            Ok(Some(pid)) => {
                println!("detached; the server is still running (pid {pid})");
                ExitCode::SUCCESS
            }
            Ok(None) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("fpk: {error}");
                ExitCode::FAILURE
            }
        }
    })
}

/// Runs until the user quits, and returns the pid of the server it was
/// attached to when it did, if it was.
async fn event_loop(terminal: &mut DefaultTerminal, home: &Home) -> io::Result<Option<u32>> {
    let mut app = App::new();
    let (events_tx, mut connection_events) = mpsc::channel(16);
    let (commands, commands_rx) = mpsc::channel(4);
    let connection = tokio::spawn(connection::run(
        home.clone(),
        Backoff::DEFAULT,
        events_tx,
        commands_rx,
    ));
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(TICK);

    let outcome = loop {
        terminal.draw(|frame| view::draw(frame, &app, Instant::now()))?;
        tokio::select! {
            event = input.next() => match event {
                Some(Ok(Event::Key(key))) => {
                    if let Some(command) = app.on_key(key) {
                        let _ = commands.try_send(command);
                    }
                }
                // A resize, focus change or paste only needs the redraw above.
                Some(Ok(_)) => {}
                Some(Err(error)) => break Err(error),
                None => break Ok(()),
            },
            Some(event) = connection_events.recv() => app.on_connection(event),
            _ = tick.tick() => {}
        }
        if app.should_quit() {
            break Ok(());
        }
    };
    connection.abort();
    outcome?;
    Ok(match app.selected_machine().state {
        MachineState::Attached { pid, .. } => Some(pid),
        _ => None,
    })
}
