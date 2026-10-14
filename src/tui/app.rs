//! The TUI's state and how input and connection events change it.
//!
//! Pure: nothing here touches the terminal, a socket or the clock, so every
//! transition is testable without either. The event loop in [`super`] feeds it
//! and [`super::view`] draws it.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

use crate::client::tasks::{TaskUpdate, TaskView};

/// How long a settled task stays in the pane: long enough to see how it
/// ended, short enough that the pane shows what is going on now.
pub(super) const SETTLED_LINGER: Duration = Duration::from_secs(5);

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
    /// The attachment is gone, or could not be made; the next attempt is at
    /// `retry_at`. `reason` says why when there is more to say than that.
    Disconnected {
        retry_at: Instant,
        reason: Option<String>,
    },
    /// The machine cannot be attached to until someone does something.
    /// Checked again at `retry_at`, in case they did it elsewhere.
    NeedsAttention {
        attention: Attention,
        retry_at: Instant,
    },
    /// What the attached server says about its tasks.
    Tasks(TaskUpdate),
}

/// What a machine needs before it can be attached to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Attention {
    /// No `fpk` on the host.
    NotInstalled,
    /// An `fpk` whose client protocol is not this client's.
    Incompatible { found: String },
    /// SSH wants a password or a login it may not ask for in the background.
    NeedsLogin,
}

/// What the UI asks one machine's connection to do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Command {
    StartServer,
}

/// What lies under the mouse, as [`super::view::spot`] finds it on screen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Spot {
    /// A machine's row in the sidebar.
    Machine(usize),
    /// What opens a menu: the `⋯` at the end of a machine's row, or `menu` in
    /// the machine pane's bottom-right corner.
    MenuButton(MenuOf),
    /// The machine list's empty space below its last row.
    Machines,
    /// A key hint under the main pane; clicking it presses its key.
    Hint(Hint),
    /// Inside the open menu.
    Menu,
}

/// Whose menu it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MenuOf {
    /// One machine's, from the `⋯` at the end of its row.
    Machine(usize),
    /// The whole client's, from `menu` in the machine pane's corner.
    Global,
}

/// A menu, opened from a button drawn for it, as herdr's visible controls
/// open what they name. It holds nothing yet: its items come with the actions they would take.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Menu {
    pub of: MenuOf,
    /// The cell clicked to open it; the view hangs it below that, on screen.
    pub column: u16,
    pub row: u16,
}

/// A key hint that does something when clicked, as herdr's visible controls do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Hint {
    Detach,
    StartServer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum MachineState {
    Attaching,
    Starting,
    Attached {
        server_version: String,
        pid: u32,
    },
    Disconnected {
        retry_at: Instant,
        reason: Option<String>,
    },
    NeedsAttention {
        attention: Attention,
        retry_at: Instant,
    },
}

#[derive(Clone, Debug)]
pub(super) struct Machine {
    pub label: String,
    /// The SSH target of a saved machine; `None` for `local`.
    pub ssh: Option<String>,
    pub state: MachineState,
    /// The server's tasks, oldest first. Kept through a disconnect, as herdr
    /// keeps a lost machine's last state on screen, and replaced by the next
    /// snapshot.
    pub tasks: Vec<TaskView>,
    /// When this client saw each settled task settle. The pane drops a task
    /// [`SETTLED_LINGER`] after it; the server keeps it regardless.
    ///
    /// The client's own clock, not the server's settle time: a remote server's
    /// clock need not agree with this one.
    settled_at: HashMap<String, Instant>,
}

/// The whole TUI.
#[derive(Debug)]
pub(super) struct App {
    machines: Vec<Machine>,
    selected: usize,
    menu: Option<Menu>,
    quit: bool,
}

impl App {
    /// `local` first, then each saved machine — label and SSH target — in the
    /// order given.
    pub(super) fn new(remotes: &[(String, String)]) -> Self {
        let machine = |label: &str, ssh: Option<&String>| Machine {
            label: label.to_owned(),
            ssh: ssh.cloned(),
            state: MachineState::Attaching,
            tasks: Vec::new(),
            settled_at: HashMap::new(),
        };
        let mut machines = vec![machine("local", None)];
        machines.extend(remotes.iter().map(|(label, ssh)| machine(label, Some(ssh))));
        Self {
            machines,
            selected: 0,
            menu: None,
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

    pub(super) fn menu(&self) -> Option<Menu> {
        self.menu
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
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return None;
        }
        // An open menu has the keyboard: it closes, and nothing behind it moves.
        if self.menu.is_some() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                self.menu = None;
            }
            return None;
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_previous(),
            KeyCode::Char('s') => return self.start_server(),
            _ => {}
        }
        None
    }

    /// Applies one mouse event at `column`, `row`, over `spot`, and returns the
    /// command it asks of the selected machine's connection, if any. A click
    /// acts on press, the wheel over the machine list moves the selection as
    /// `j` and `k` do, and a click on a menu's button opens it.
    pub(super) fn on_mouse(
        &mut self,
        kind: MouseEventKind,
        spot: Option<Spot>,
        column: u16,
        row: u16,
    ) -> Option<Command> {
        if let Some(open) = self.menu {
            // A press outside the menu closes it and does nothing else, except
            // that another menu's button opens that one instead; its own closes
            // it, as a toggle.
            if matches!(kind, MouseEventKind::Down(_)) && spot != Some(Spot::Menu) {
                self.menu = None;
                if let (MouseEventKind::Down(MouseButton::Left), Some(Spot::MenuButton(of))) =
                    (kind, spot)
                    && of != open.of
                {
                    self.open_menu(of, column, row);
                }
            }
            return None;
        }
        match (kind, spot?) {
            (MouseEventKind::Down(MouseButton::Left), Spot::MenuButton(of)) => {
                self.open_menu(of, column, row);
            }
            (MouseEventKind::Down(MouseButton::Left), Spot::Machine(index)) => {
                self.selected = index.min(self.machines.len() - 1);
            }
            (MouseEventKind::Down(MouseButton::Left), Spot::Hint(Hint::Detach)) => {
                self.quit = true;
            }
            (MouseEventKind::Down(MouseButton::Left), Spot::Hint(Hint::StartServer)) => {
                return self.start_server();
            }
            (
                MouseEventKind::ScrollDown,
                Spot::Machine(_) | Spot::MenuButton(_) | Spot::Machines,
            ) => self.select_next(),
            (MouseEventKind::ScrollUp, Spot::Machine(_) | Spot::MenuButton(_) | Spot::Machines) => {
                self.select_previous();
            }
            _ => {}
        }
        None
    }

    /// A machine's menu selects that machine too; the global one leaves the
    /// selection where it is.
    fn open_menu(&mut self, of: MenuOf, column: u16, row: u16) {
        let of = match of {
            MenuOf::Machine(index) => {
                let machine = index.min(self.machines.len() - 1);
                self.selected = machine;
                MenuOf::Machine(machine)
            }
            MenuOf::Global => MenuOf::Global,
        };
        self.menu = Some(Menu { of, column, row });
    }

    fn select_next(&mut self) {
        self.selected = (self.selected + 1).min(self.machines.len() - 1);
    }

    fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Only a disconnected machine has a server to start; anywhere else this
    /// does nothing rather than start a second one.
    fn start_server(&self) -> Option<Command> {
        matches!(
            self.selected_machine().state,
            MachineState::Disconnected { .. }
        )
        .then_some(Command::StartServer)
    }

    /// Applies what machine `index`'s connection reported, as of now.
    pub(super) fn on_connection(&mut self, index: usize, event: ConnectionEvent) {
        self.on_connection_at(index, event, Instant::now());
    }

    pub(super) fn on_connection_at(&mut self, index: usize, event: ConnectionEvent, now: Instant) {
        let Some(machine) = self.machines.get_mut(index) else {
            return;
        };
        machine.state = match event {
            ConnectionEvent::Tasks(update) => {
                machine.apply(update, now);
                return;
            }
            ConnectionEvent::Attaching => MachineState::Attaching,
            ConnectionEvent::Starting => MachineState::Starting,
            ConnectionEvent::Attached {
                server_version,
                pid,
            } => MachineState::Attached {
                server_version,
                pid,
            },
            ConnectionEvent::Disconnected { retry_at, reason } => {
                MachineState::Disconnected { retry_at, reason }
            }
            ConnectionEvent::NeedsAttention {
                attention,
                retry_at,
            } => MachineState::NeedsAttention {
                attention,
                retry_at,
            },
        };
    }

    /// Drops every settled task that has lingered its while. Called each tick.
    pub(super) fn expire(&mut self, now: Instant) {
        for machine in &mut self.machines {
            machine.expire(now);
        }
    }
}

impl Machine {
    fn apply(&mut self, update: TaskUpdate, now: Instant) {
        match update {
            TaskUpdate::Snapshot(snapshot) => {
                // A settled task this client never saw running settled while it
                // was not looking, longer ago than it can tell; it is not shown.
                // One it did see keeps the time it was first seen settled.
                let mut settled_at = HashMap::new();
                let mut tasks = Vec::new();
                for task in snapshot {
                    if task.status.is_terminal() {
                        let known = self.tasks.iter().find(|held| held.id == task.id);
                        let since = match (known, self.settled_at.get(&task.id)) {
                            // Already lingering: keep its time.
                            (Some(_), Some(since)) => *since,
                            // Seen running, settled while this was not looking.
                            (Some(known), None) if !known.status.is_terminal() => now,
                            // Never seen, or seen settled with no time: past it.
                            _ => continue,
                        };
                        settled_at.insert(task.id.clone(), since);
                    }
                    tasks.push(task);
                }
                self.tasks = tasks;
                self.settled_at = settled_at;
            }
            TaskUpdate::Changed(task) => {
                if task.status.is_terminal() {
                    self.settled_at.entry(task.id.clone()).or_insert(now);
                } else {
                    self.settled_at.remove(&task.id);
                }
                match self.tasks.iter_mut().find(|held| held.id == task.id) {
                    Some(held) => *held = task,
                    None => self.tasks.push(task),
                }
            }
        }
        self.tasks.sort_by_key(|task| task.submitted_unix_millis);
        self.expire(now);
    }

    fn expire(&mut self, now: Instant) {
        let settled_at = &self.settled_at;
        self.tasks.retain(|task| {
            settled_at
                .get(&task.id)
                .is_none_or(|since| now.duration_since(*since) < SETTLED_LINGER)
        });
        let tasks = &self.tasks;
        self.settled_at
            .retain(|id, _| tasks.iter().any(|task| &task.id == id));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::client::tasks::{Measure, TaskStatus};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn disconnected(app: &mut App) {
        app.on_connection(
            0,
            ConnectionEvent::Disconnected {
                retry_at: Instant::now() + Duration::from_secs(1),
                reason: None,
            },
        );
    }

    #[test]
    fn starts_with_local_attaching() {
        let app = App::new(&[]);
        assert_eq!(app.machines().len(), 1);
        assert_eq!(app.selected_machine().label, "local");
        assert_eq!(app.selected_machine().state, MachineState::Attaching);
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        let mut app = App::new(&[]);
        app.on_key(press(KeyCode::Char('q')));
        assert!(app.should_quit());

        let mut app = App::new(&[]);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit());

        let mut app = App::new(&[]);
        app.on_key(press(KeyCode::Char('c')));
        assert!(!app.should_quit());
    }

    #[test]
    fn selection_stays_inside_the_list() {
        let mut app = App::new(&[]);
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
        let mut app = App::new(&[]);
        assert_eq!(app.on_key(press(KeyCode::Char('s'))), None);
        app.on_connection(
            0,
            ConnectionEvent::Attached {
                server_version: "0.0.0".to_owned(),
                pid: 1,
            },
        );
        assert_eq!(app.on_key(press(KeyCode::Char('s'))), None);
        disconnected(&mut app);
        assert_eq!(
            app.on_key(press(KeyCode::Char('s'))),
            Some(Command::StartServer)
        );
    }

    #[test]
    fn a_key_release_does_nothing() {
        let mut app = App::new(&[]);
        let mut release = press(KeyCode::Char('q'));
        release.kind = KeyEventKind::Release;
        app.on_key(release);
        assert!(!app.should_quit());
    }

    fn click(app: &mut App, spot: Spot) -> Option<Command> {
        app.on_mouse(MouseEventKind::Down(MouseButton::Left), Some(spot), 0, 0)
    }

    #[test]
    fn clicking_a_machine_selects_it() {
        let mut app = App::new(&[
            ("workbox".to_owned(), "me@workbox".to_owned()),
            ("nano4".to_owned(), "nano4".to_owned()),
        ]);
        click(&mut app, Spot::Machine(2));
        assert_eq!(app.selected_machine().label, "nano4");
        click(&mut app, Spot::Machine(0));
        assert_eq!(app.selected_machine().label, "local");
        // The middle button, or a release, selects nothing.
        app.on_mouse(
            MouseEventKind::Down(MouseButton::Middle),
            Some(Spot::Machine(1)),
            0,
            0,
        );
        app.on_mouse(
            MouseEventKind::Up(MouseButton::Left),
            Some(Spot::Machine(1)),
            0,
            0,
        );
        assert_eq!(app.selected(), 0);
    }

    fn open(app: &mut App, machine: usize, row: u16) {
        app.on_mouse(
            MouseEventKind::Down(MouseButton::Left),
            Some(Spot::MenuButton(MenuOf::Machine(machine))),
            23,
            row,
        );
    }

    fn open_global(app: &mut App) {
        app.on_mouse(
            MouseEventKind::Down(MouseButton::Left),
            Some(Spot::MenuButton(MenuOf::Global)),
            23,
            9,
        );
    }

    #[test]
    fn the_global_button_opens_the_global_menu_and_leaves_the_selection() {
        let mut app = App::new(&[("workbox".to_owned(), "me@workbox".to_owned())]);
        app.on_key(press(KeyCode::Char('j')));
        open_global(&mut app);
        assert_eq!(app.menu().map(|menu| menu.of), Some(MenuOf::Global));
        assert_eq!(app.selected(), 1);
        // Its own button closes it; a machine's swaps it for that machine's.
        open_global(&mut app);
        assert_eq!(app.menu(), None);
        open_global(&mut app);
        open(&mut app, 0, 2);
        assert_eq!(app.menu().map(|menu| menu.of), Some(MenuOf::Machine(0)));
        open_global(&mut app);
        assert_eq!(app.menu().map(|menu| menu.of), Some(MenuOf::Global));
    }

    #[test]
    fn clicking_a_machines_dots_opens_its_menu_there_and_a_right_click_does_not() {
        let mut app = App::new(&[("workbox".to_owned(), "me@workbox".to_owned())]);
        open(&mut app, 1, 3);
        assert_eq!(
            app.menu(),
            Some(Menu {
                of: MenuOf::Machine(1),
                column: 23,
                row: 3
            })
        );
        assert_eq!(app.selected(), 1);

        let mut app = App::new(&[]);
        for spot in [Spot::Machine(0), Spot::MenuButton(MenuOf::Machine(0))] {
            app.on_mouse(MouseEventKind::Down(MouseButton::Right), Some(spot), 23, 2);
        }
        assert_eq!(app.menu(), None);
    }

    #[test]
    fn an_open_menu_closes_on_escape_or_a_press_outside_it_and_holds_everything_else() {
        let mut app = App::new(&[("workbox".to_owned(), "me@workbox".to_owned())]);
        open(&mut app, 0, 2);
        // Behind it, nothing moves and nothing quits.
        app.on_key(press(KeyCode::Char('j')));
        app.on_mouse(MouseEventKind::ScrollDown, Some(Spot::Machines), 4, 6);
        click(&mut app, Spot::Menu);
        assert_eq!(app.selected(), 0);
        assert!(app.menu().is_some());
        app.on_key(press(KeyCode::Char('q')));
        assert_eq!(app.menu(), None);
        assert!(!app.should_quit());

        // A click outside closes it without landing on what is there.
        open(&mut app, 0, 2);
        assert_eq!(click(&mut app, Spot::Hint(Hint::Detach)), None);
        assert!(!app.should_quit());
        assert_eq!(app.menu(), None);
        open(&mut app, 0, 2);
        click(&mut app, Spot::Machine(1));
        assert_eq!(app.menu(), None);
        assert_eq!(app.selected(), 0);

        // Its own dots close it; another machine's move it there.
        open(&mut app, 0, 2);
        open(&mut app, 0, 2);
        assert_eq!(app.menu(), None);
        open(&mut app, 0, 2);
        open(&mut app, 1, 3);
        assert_eq!(app.menu().map(|menu| menu.of), Some(MenuOf::Machine(1)));
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.menu(), None);

        // Ctrl-C still quits from inside a menu.
        open(&mut app, 0, 2);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit());
    }

    #[test]
    fn the_wheel_over_the_machines_moves_the_selection() {
        let mut app = App::new(&[("workbox".to_owned(), "me@workbox".to_owned())]);
        app.on_mouse(MouseEventKind::ScrollDown, Some(Spot::Machines), 0, 0);
        assert_eq!(app.selected(), 1);
        app.on_mouse(MouseEventKind::ScrollDown, Some(Spot::Machine(0)), 0, 0);
        assert_eq!(app.selected(), 1);
        app.on_mouse(MouseEventKind::ScrollUp, Some(Spot::Machine(1)), 0, 0);
        assert_eq!(app.selected(), 0);
        // Anywhere else the wheel leaves it.
        app.on_mouse(MouseEventKind::ScrollDown, None, 0, 0);
        assert_eq!(app.selected(), 0);
    }

    #[test]
    fn clicking_a_hint_presses_its_key() {
        let mut app = App::new(&[]);
        assert_eq!(click(&mut app, Spot::Hint(Hint::StartServer)), None);
        disconnected(&mut app);
        assert_eq!(
            click(&mut app, Spot::Hint(Hint::StartServer)),
            Some(Command::StartServer)
        );
        assert!(!app.should_quit());
        click(&mut app, Spot::Hint(Hint::Detach));
        assert!(app.should_quit());
    }

    fn task(id: &str, submitted: i64, status: TaskStatus) -> TaskView {
        TaskView {
            id: id.to_owned(),
            parent: None,
            kind: "demo.count".to_owned(),
            subject: Vec::new(),
            status,
            phase: None,
            measure: Measure::Indeterminate,
            submitted_unix_millis: submitted,
        }
    }

    fn ids(app: &App) -> Vec<String> {
        app.selected_machine()
            .tasks
            .iter()
            .map(|task| task.id.clone())
            .collect()
    }

    fn changed(app: &mut App, task: TaskView, now: Instant) {
        app.on_connection_at(0, ConnectionEvent::Tasks(TaskUpdate::Changed(task)), now);
    }

    #[test]
    fn a_snapshot_replaces_the_tasks_and_a_change_updates_one() {
        let now = Instant::now();
        let mut app = App::new(&[]);
        app.on_connection_at(
            0,
            ConnectionEvent::Tasks(TaskUpdate::Snapshot(vec![
                task("b", 2, TaskStatus::Running),
                task("a", 1, TaskStatus::Queued { on_resource: false }),
            ])),
            now,
        );
        assert_eq!(ids(&app), ["a", "b"]);

        changed(
            &mut app,
            task("b", 2, TaskStatus::Failed("no".to_owned())),
            now,
        );
        changed(
            &mut app,
            task("c", 3, TaskStatus::Queued { on_resource: false }),
            now,
        );
        assert_eq!(ids(&app), ["a", "b", "c"]);
        assert_eq!(
            app.selected_machine().tasks[1].status,
            TaskStatus::Failed("no".to_owned())
        );

        // A disconnect keeps them; the next snapshot replaces them.
        disconnected(&mut app);
        assert_eq!(app.selected_machine().tasks.len(), 3);
        app.on_connection_at(
            0,
            ConnectionEvent::Tasks(TaskUpdate::Snapshot(Vec::new())),
            now,
        );
        assert_eq!(app.selected_machine().tasks, Vec::new());
    }

    #[test]
    fn a_settled_task_lingers_five_seconds_then_goes() {
        let start = Instant::now();
        let mut app = App::new(&[]);
        changed(&mut app, task("a", 1, TaskStatus::Running), start);
        changed(&mut app, task("b", 2, TaskStatus::Running), start);
        changed(&mut app, task("a", 1, TaskStatus::Succeeded), start);
        let later = start + Duration::from_secs(2);
        changed(
            &mut app,
            task("b", 2, TaskStatus::Failed("no".to_owned())),
            later,
        );

        app.expire(start + SETTLED_LINGER.saturating_sub(Duration::from_millis(1)));
        assert_eq!(ids(&app), ["a", "b"]);
        // Each goes five seconds after it settled, not after the first did.
        app.expire(start + SETTLED_LINGER);
        assert_eq!(ids(&app), ["b"]);
        app.expire(later + SETTLED_LINGER);
        assert_eq!(ids(&app), Vec::<String>::new());
    }

    #[test]
    fn a_task_finishing_between_two_running_ones_leaves_alone() {
        let start = Instant::now();
        let mut app = App::new(&[]);
        for (submitted, id) in (0..).zip(["first", "middle", "last"]) {
            changed(&mut app, task(id, submitted, TaskStatus::Running), start);
        }
        let done = start + Duration::from_secs(1);
        changed(&mut app, task("middle", 1, TaskStatus::Succeeded), done);

        app.expire(done + SETTLED_LINGER.saturating_sub(Duration::from_millis(1)));
        assert_eq!(ids(&app), ["first", "middle", "last"]);
        app.expire(done + SETTLED_LINGER);
        // The two still running keep their places, in order.
        assert_eq!(ids(&app), ["first", "last"]);
    }

    #[test]
    fn a_running_task_never_expires() {
        let start = Instant::now();
        let mut app = App::new(&[]);
        changed(&mut app, task("a", 1, TaskStatus::Running), start);
        app.expire(start + Duration::from_secs(3600));
        assert_eq!(ids(&app), ["a"]);
    }

    #[test]
    fn a_snapshot_shows_only_settled_tasks_this_client_saw_end() {
        let start = Instant::now();
        let mut app = App::new(&[]);
        changed(&mut app, task("lingering", 1, TaskStatus::Running), start);
        changed(&mut app, task("lingering", 1, TaskStatus::Succeeded), start);
        changed(&mut app, task("running", 2, TaskStatus::Running), start);

        // Reattached two seconds later: "running" finished while this was away,
        // and "old" finished before it ever looked.
        let back = start + Duration::from_secs(2);
        app.on_connection_at(
            0,
            ConnectionEvent::Tasks(TaskUpdate::Snapshot(vec![
                task("old", 0, TaskStatus::Succeeded),
                task("lingering", 1, TaskStatus::Succeeded),
                task("running", 2, TaskStatus::Succeeded),
            ])),
            back,
        );
        assert_eq!(ids(&app), ["lingering", "running"]);
        // "lingering" keeps the time it first settled; "running" starts now.
        app.expire(start + SETTLED_LINGER);
        assert_eq!(ids(&app), ["running"]);
        app.expire(back + SETTLED_LINGER);
        assert_eq!(ids(&app), Vec::<String>::new());
    }

    #[test]
    fn connection_events_set_the_machine_state() {
        let mut app = App::new(&[]);
        app.on_connection(
            0,
            ConnectionEvent::Attached {
                server_version: "0.0.0".to_owned(),
                pid: 42,
            },
        );
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
        app.on_connection(0, ConnectionEvent::Starting);
        assert_eq!(app.selected_machine().state, MachineState::Starting);
    }
}
