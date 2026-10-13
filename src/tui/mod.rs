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

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream},
    execute,
};
use ratatui::{DefaultTerminal, layout::Rect};
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;

use self::{
    app::App,
    connection::{Backoff, Target},
};
use crate::{
    client::{
        self, machines,
        ssh::{Prompt, Ssh},
    },
    config::Config,
    home::Home,
};

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
    // Read before the terminal is taken, so a broken file says so plainly.
    let config = match Config::load(home) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("fpk: {error}");
            return ExitCode::FAILURE;
        }
    };
    let targets = match targets(home, &config) {
        Ok(targets) => targets,
        Err(error) => {
            eprintln!("fpk: {error}");
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
        let mouse = config.ui.mouse_capture;
        if mouse {
            capture_mouse();
        }
        let outcome = event_loop(&mut terminal, targets).await;
        if mouse {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        ratatui::restore();

        match outcome {
            Ok(()) => {
                println!("detached; every server keeps running");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("fpk: {error}");
                ExitCode::FAILURE
            }
        }
    })
}

/// Takes the mouse, as herdr does unless `ui.mouse_capture` is off, and gives
/// it back before a panic's message as `init`'s hook gives back the rest.
fn capture_mouse() {
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        restore(info);
    }));
    // A terminal that cannot report the mouse is still usable by keyboard.
    let _ = execute!(io::stdout(), EnableMouseCapture);
}

/// `local`, then each enabled saved machine, with the label and SSH target
/// the app lists for each.
fn targets(home: &Home, config: &Config) -> Result<Vec<(String, String, Target)>, String> {
    let mut targets = vec![(
        "local".to_owned(),
        String::new(),
        Target::Local(home.clone()),
    )];
    for machine in machines::load(home)?
        .into_iter()
        .filter(|machine| machine.enabled)
    {
        let ssh = Ssh::new(home, config, &machine.ssh, Prompt::Never)
            .map_err(|error| error.to_string())?;
        targets.push((machine.label, machine.ssh, Target::Remote(ssh)));
    }
    Ok(targets)
}

/// Runs until the user quits.
///
/// Each machine has a connection task of its own, reconnecting on its own
/// backoff, so a slow or lost remote holds up nothing else; `local` opens at
/// once rather than waiting for the remote ones.
async fn event_loop(
    terminal: &mut DefaultTerminal,
    targets: Vec<(String, String, Target)>,
) -> io::Result<()> {
    let remotes: Vec<(String, String)> = targets
        .iter()
        .skip(1)
        .map(|(label, ssh, _)| (label.clone(), ssh.clone()))
        .collect();
    let mut app = App::new(&remotes);
    let (merged_tx, mut connection_events) = mpsc::channel(64);
    let mut commands = Vec::new();
    let mut tasks = Vec::new();
    for (index, (_, _, target)) in targets.into_iter().enumerate() {
        let (events_tx, mut events) = mpsc::channel(16);
        let (command_tx, command_rx) = mpsc::channel(4);
        commands.push(command_tx);
        tasks.push(tokio::spawn(connection::run(
            target,
            Backoff::DEFAULT,
            events_tx,
            command_rx,
        )));
        // Tags each machine's events with its row, so one loop serves them all.
        let merged = merged_tx.clone();
        tasks.push(tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if merged.send((index, event)).await.is_err() {
                    return;
                }
            }
        }));
    }
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(TICK);

    let outcome = loop {
        let now = Instant::now();
        app.expire(now);
        terminal.draw(|frame| view::draw(frame, &app, now))?;
        tokio::select! {
            event = input.next() => match event {
                Some(Ok(Event::Key(key))) => {
                    if let Some(command) = app.on_key(key) {
                        let _ = commands[app.selected()].try_send(command);
                    }
                }
                Some(Ok(Event::Mouse(mouse))) => {
                    let size = terminal.size()?;
                    let screen = Rect::new(0, 0, size.width, size.height);
                    let spot = view::spot(&app, screen, mouse.column, mouse.row);
                    if let Some(command) = app.on_mouse(mouse.kind, spot) {
                        let _ = commands[app.selected()].try_send(command);
                    }
                }
                // A resize, focus change or paste only needs the redraw above.
                Some(Ok(_)) => {}
                Some(Err(error)) => break Err(error),
                None => break Ok(()),
            },
            Some((index, event)) = connection_events.recv() => app.on_connection(index, event),
            _ = tick.tick() => {}
        }
        if app.should_quit() {
            break Ok(());
        }
    };
    for task in tasks {
        task.abort();
    }
    outcome
}
