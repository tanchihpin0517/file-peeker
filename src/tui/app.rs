//! The TUI's state and how input and connection events change it.
//!
//! Pure: nothing here touches the terminal, a socket or the clock, so every
//! transition is testable without either. The event loop in [`super`] feeds it
//! and [`super::view`] draws it.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// What one machine's connection reports to the UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ConnectionEvent {
    /// Trying to attach.
    Attaching,
    /// Starting a server because the user asked to.
    Starting,
    Attached {
        server_version: String,
        pid: u32,
    },
    /// The attachment is gone; the next attempt is at `retry_at`.
    Disconnected {
        retry_at: Instant,
    },
}

/// What the UI asks one machine's connection to do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Command {
    StartServer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum MachineState {
    Attaching,
    Starting,
    Attached { server_version: String, pid: u32 },
    Disconnected { retry_at: Instant },
}

#[derive(Clone, Debug)]
pub(super) struct Machine {
    pub label: String,
    pub state: MachineState,
}

/// The whole TUI.
#[derive(Debug)]
pub(super) struct App {
    machines: Vec<Machine>,
    selected: usize,
    quit: bool,
}

impl App {
    /// The machine list starts with `local` and only `local`; saved SSH machines
    /// join it in a later step, as rows rather than as a new layout.
    pub(super) fn new() -> Self {
        Self {
            machines: vec![Machine {
                label: "local".to_owned(),
                state: MachineState::Attaching,
            }],
            selected: 0,
            quit: false,
        }
    }

    pub(super) fn machines(&self) -> &[Machine] {
        &self.machines
    }

    pub(super) fn selected(&self) -> usize {
        self.selected
    }

    pub(super) fn selected_machine(&self) -> &Machine {
        &self.machines[self.selected]
    }

    pub(super) fn should_quit(&self) -> bool {
        self.quit
    }

    /// Applies one key press, and returns the command it asks of the selected
    /// machine's connection, if any.
    pub(super) fn on_key(&mut self, key: KeyEvent) -> Option<Command> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.machines.len() - 1);
            }
            KeyCode::Char('k') | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            // Only a disconnected machine has a server to start; anywhere else
            // the key does nothing rather than start a second one.
            KeyCode::Char('s')
                if matches!(
                    self.selected_machine().state,
                    MachineState::Disconnected { .. }
                ) =>
            {
                return Some(Command::StartServer);
            }
            _ => {}
        }
        None
    }

    /// Applies what the local machine's connection reported.
    pub(super) fn on_connection(&mut self, event: ConnectionEvent) {
        let machine = &mut self.machines[0];
        machine.state = match event {
            ConnectionEvent::Attaching => MachineState::Attaching,
            ConnectionEvent::Starting => MachineState::Starting,
            ConnectionEvent::Attached {
                server_version,
                pid,
            } => MachineState::Attached {
                server_version,
                pid,
            },
            ConnectionEvent::Disconnected { retry_at } => MachineState::Disconnected { retry_at },
        };
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn disconnected(app: &mut App) {
        app.on_connection(ConnectionEvent::Disconnected {
            retry_at: Instant::now() + Duration::from_secs(1),
        });
    }

    #[test]
    fn starts_with_local_attaching() {
        let app = App::new();
        assert_eq!(app.machines().len(), 1);
        assert_eq!(app.selected_machine().label, "local");
        assert_eq!(app.selected_machine().state, MachineState::Attaching);
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        let mut app = App::new();
        app.on_key(press(KeyCode::Char('q')));
        assert!(app.should_quit());

        let mut app = App::new();
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit());

        let mut app = App::new();
        app.on_key(press(KeyCode::Char('c')));
        assert!(!app.should_quit());
    }

    #[test]
    fn selection_stays_inside_the_list() {
        let mut app = App::new();
        for code in [
            KeyCode::Char('j'),
            KeyCode::Down,
            KeyCode::Char('k'),
            KeyCode::Up,
        ] {
            app.on_key(press(code));
            assert_eq!(app.selected(), 0);
        }
    }

    #[test]
    fn s_starts_a_server_only_when_disconnected() {
        let mut app = App::new();
        assert_eq!(app.on_key(press(KeyCode::Char('s'))), None);
        app.on_connection(ConnectionEvent::Attached {
            server_version: "0.0.0".to_owned(),
            pid: 1,
        });
        assert_eq!(app.on_key(press(KeyCode::Char('s'))), None);
        disconnected(&mut app);
        assert_eq!(
            app.on_key(press(KeyCode::Char('s'))),
            Some(Command::StartServer)
        );
    }

    #[test]
    fn a_key_release_does_nothing() {
        let mut app = App::new();
        let mut release = press(KeyCode::Char('q'));
        release.kind = KeyEventKind::Release;
        app.on_key(release);
        assert!(!app.should_quit());
    }

    #[test]
    fn connection_events_set_the_machine_state() {
        let mut app = App::new();
        app.on_connection(ConnectionEvent::Attached {
            server_version: "0.0.0".to_owned(),
            pid: 42,
        });
        assert_eq!(
            app.selected_machine().state,
            MachineState::Attached {
                server_version: "0.0.0".to_owned(),
                pid: 42
            }
        );
        disconnected(&mut app);
        assert!(matches!(
            app.selected_machine().state,
            MachineState::Disconnected { .. }
        ));
        app.on_connection(ConnectionEvent::Starting);
        assert_eq!(app.selected_machine().state, MachineState::Starting);
    }
}
