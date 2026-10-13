//! Draws [`App`]. A pure function of the state and the time, so it is tested
//! against ratatui's test backend rather than a terminal.

use std::time::Instant;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use super::app::{App, Attention, Hint, Machine, MachineState, Spot};
use crate::client::tasks::{Measure, TaskStatus, TaskView};

const SIDEBAR_WIDTH: u16 = 26;

/// Where each part of the screen is. Drawing and finding what the mouse is
/// over both work from this, so a click lands on what was drawn there.
struct Areas {
    title: Rect,
    machines: Rect,
    tasks: Rect,
    main: Rect,
    status: Rect,
}

impl Areas {
    fn of(screen: Rect) -> Self {
        let [title, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(screen);
        let [sidebar, pane] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(body);
        // Split in half as herdr's is: machines above, the selected machine's
        // tasks below.
        let [machines, tasks] =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(sidebar);
        // The key hints belong to the main pane and sit under it alone; the
        // sidebar runs the full height beside them.
        let [main, status] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(pane);
        Self {
            title,
            machines,
            tasks,
            main,
            status,
        }
    }
}

pub(super) fn draw(frame: &mut Frame<'_>, app: &App, now: Instant) {
    let areas = Areas::of(frame.area());
    let machine = app.selected_machine();
    frame.render_widget(title_line(machine), areas.title);
    draw_machines(frame, app, now, areas.machines);
    draw_tasks(frame, machine, areas.tasks);
    draw_main(frame, machine, now, areas.main);
    frame.render_widget(status_line(machine), areas.status);
}

/// What lies at `column`, `row` on a `screen`-sized terminal showing `app`.
pub(super) fn spot(app: &App, screen: Rect, column: u16, row: u16) -> Option<Spot> {
    let areas = Areas::of(screen);
    let at = ratatui::layout::Position::new(column, row);
    let machines = machines_block().inner(areas.machines);
    if machines.contains(at) {
        let index = usize::from(row - machines.y);
        return Some(if index < app.machines().len() {
            Spot::Machine(index)
        } else {
            Spot::Machines
        });
    }
    if areas.status.contains(at) {
        let mut x = areas.status.x;
        for (span, hint) in hints(app.selected_machine()) {
            let width = u16::try_from(span.width()).unwrap_or(u16::MAX);
            if column < x.saturating_add(width) {
                return hint.map(Spot::Hint);
            }
            x = x.saturating_add(width);
        }
    }
    None
}

fn title_line(machine: &Machine) -> Paragraph<'static> {
    let detail = match &machine.state {
        MachineState::Attached {
            server_version,
            pid,
        } => format!("server {server_version} · pid {pid}"),
        MachineState::Attaching => "attaching".to_owned(),
        MachineState::Starting => "starting the server".to_owned(),
        MachineState::Disconnected { .. } => "disconnected".to_owned(),
        MachineState::NeedsAttention { .. } => "needs attention".to_owned(),
    };
    Paragraph::new(Line::from(vec![
        Span::styled(" fpk ", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(format!("· {} · {detail}", machine.label)),
    ]))
    .style(Style::new().add_modifier(Modifier::REVERSED))
}

fn machines_block() -> Block<'static> {
    Block::new()
        .borders(Borders::RIGHT)
        .title(Span::styled(" Machines", BOLD))
}

/// One row per machine, its first just under the title, as [`spot`] counts.
fn draw_machines(frame: &mut Frame<'_>, app: &App, now: Instant, area: Rect) {
    let block = machines_block();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows: Vec<Line<'_>> = app
        .machines()
        .iter()
        .enumerate()
        .map(|(index, machine)| machine_row(machine, now, index == app.selected()))
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}

fn draw_tasks(frame: &mut Frame<'_>, machine: &Machine, tasks: Rect) {
    // The divider meets the right edge that runs on above it, so it joins it
    // rather than turning a corner.
    let block = Block::new()
        .borders(Borders::TOP | Borders::RIGHT)
        .border_set(symbols::border::Set {
            top_right: symbols::line::VERTICAL_LEFT,
            ..symbols::border::PLAIN
        })
        .title(Span::styled(" Tasks", BOLD));
    let inner = block.inner(tasks);
    frame.render_widget(block, tasks);
    let rows: Vec<Line<'_>> = if machine.tasks.is_empty() {
        vec![Line::styled(" No tasks", DIM)]
    } else {
        // In the order they were submitted, as a queue reads: a new task joins
        // the end.
        machine.tasks.iter().map(task_row).collect()
    };
    let dim = matches!(machine.state, MachineState::Disconnected { .. });
    frame.render_widget(
        Paragraph::new(rows).style(if dim { DIM } else { Style::new() }),
        inner,
    );
}

const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);
const DIM: Style = Style::new().add_modifier(Modifier::DIM);
/// The kind column's width; a longer kind is cut rather than wrapped.
const KIND_WIDTH: usize = 11;

fn task_row(task: &TaskView) -> Line<'static> {
    let (glyph, colour) = match task.status {
        TaskStatus::Queued { .. } => ("○", Color::DarkGray),
        TaskStatus::Running => ("◐", Color::Yellow),
        TaskStatus::Cancelling => ("◌", Color::Yellow),
        TaskStatus::Succeeded => ("✓", Color::Green),
        TaskStatus::Failed(_) => ("✗", Color::Red),
        TaskStatus::Cancelled => ("–", Color::DarkGray),
    };
    let indent = if task.parent.is_some() { "  " } else { "" };
    let kind: String = task.kind.chars().take(KIND_WIDTH).collect();
    Line::from(vec![
        Span::styled(format!(" {indent}{glyph} "), Style::new().fg(colour)),
        Span::raw(format!("{kind:<KIND_WIDTH$} ")),
        Span::styled(task_status(task), DIM),
    ])
}

/// What a task row says after its kind: a share when the task knows its total,
/// a count when it does not, and its state otherwise.
fn task_status(task: &TaskView) -> String {
    match &task.status {
        TaskStatus::Queued { on_resource: true } => "waiting".to_owned(),
        TaskStatus::Queued { on_resource: false } => "queued".to_owned(),
        TaskStatus::Running => match task.measure {
            Measure::Items {
                done,
                total: Some(total),
            }
            | Measure::Bytes {
                done,
                total: Some(total),
            } if total > 0 => format!("{}%", done.saturating_mul(100) / total),
            Measure::Items { done, total: None } => done.to_string(),
            Measure::Bytes { done, total: None } => format!("{done} B"),
            _ => task.phase.clone().unwrap_or_else(|| "running".to_owned()),
        },
        TaskStatus::Cancelling => "cancelling".to_owned(),
        TaskStatus::Succeeded => "done".to_owned(),
        TaskStatus::Failed(_) => "failed".to_owned(),
        TaskStatus::Cancelled => "cancelled".to_owned(),
    }
}

fn machine_row(machine: &Machine, now: Instant, selected: bool) -> Line<'static> {
    let (dot, state) = match &machine.state {
        MachineState::Attached { .. } => (Color::Green, "attached".to_owned()),
        MachineState::Attaching => (Color::Yellow, "attaching".to_owned()),
        MachineState::Starting => (Color::Yellow, "starting".to_owned()),
        MachineState::Disconnected { retry_at, .. } => {
            (Color::Red, format!("retry {}", countdown(*retry_at, now)))
        }
        MachineState::NeedsAttention { .. } => (Color::Magenta, "attention".to_owned()),
    };
    let mut row = Line::from(vec![
        Span::styled(" ● ", Style::new().fg(dot)),
        Span::raw(format!("{:<8}", machine.label)),
        Span::styled(state, Style::new().add_modifier(Modifier::DIM)),
    ]);
    if selected {
        row = row.style(Style::new().add_modifier(Modifier::REVERSED));
    }
    row
}

fn draw_main(frame: &mut Frame<'_>, machine: &Machine, now: Instant, area: Rect) {
    let lines = match &machine.state {
        MachineState::Attached { .. } => vec![Line::from("Nothing to browse yet.")],
        MachineState::Attaching => vec![Line::from("Attaching to the server…")],
        MachineState::Starting => vec![Line::from("Starting the server…")],
        MachineState::Disconnected { retry_at, reason } => {
            let mut lines = vec![Line::from("Disconnected from the server.")];
            if let Some(reason) = reason {
                lines.push(Line::from(reason.clone()));
            }
            lines.push(Line::from(format!(
                "Retrying in {}. Press s to start it.",
                countdown(*retry_at, now)
            )));
            lines
        }
        MachineState::NeedsAttention {
            attention,
            retry_at,
        } => {
            let label = &machine.label;
            let add = format!(
                "fpk machine add {label} {}",
                machine.ssh.as_deref().unwrap_or("<ssh-target>")
            );
            let (what, run) = match attention {
                Attention::NotInstalled => (format!("fpk is not installed on {label}."), add),
                Attention::Incompatible { found } => (
                    format!("{label} has {found}, which this client cannot attach to."),
                    add,
                ),
                Attention::NeedsLogin => (
                    format!("{label} wants you to sign in."),
                    format!("fpk machine login {label}"),
                ),
            };
            vec![
                Line::from(what),
                Line::from(format!("Run `{run}` in a terminal.")),
                Line::styled(
                    format!("Checking again in {}.", countdown(*retry_at, now)),
                    DIM,
                ),
            ]
        }
    };
    let dim = matches!(machine.state, MachineState::Disconnected { .. });
    let style = if dim {
        Style::new().add_modifier(Modifier::DIM)
    } else {
        Style::new()
    };
    // Roughly centred: the top padding is half of what the text leaves over.
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let top = area.height.saturating_sub(height) / 2;
    let [_, text] = Layout::vertical([Constraint::Length(top), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .style(style),
        text,
    );
}

fn status_line(machine: &Machine) -> Paragraph<'static> {
    let spans: Vec<Span<'static>> = hints(machine).into_iter().map(|(span, _)| span).collect();
    Paragraph::new(Line::from(spans)).style(Style::new().add_modifier(Modifier::DIM))
}

/// The key hints, left to right, each with what clicking it does. [`spot`]
/// measures the same spans [`status_line`] draws.
fn hints(machine: &Machine) -> Vec<(Span<'static>, Option<Hint>)> {
    let mut hints = vec![
        (Span::raw(" j/k move"), None),
        (Span::raw(" · "), None),
        (Span::raw("q detach"), Some(Hint::Detach)),
    ];
    if matches!(machine.state, MachineState::Disconnected { .. }) {
        hints.push((Span::raw(" · "), None));
        hints.push((Span::raw("s start the server"), Some(Hint::StartServer)));
    }
    hints
}

/// Whole seconds until `at`, rounded up so a countdown never shows `0s` while
/// there is still time left.
fn countdown(at: Instant, now: Instant) -> String {
    let remaining = at.saturating_duration_since(now);
    let seconds = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
    format!("{seconds}s")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::{client::tasks::TaskUpdate, tui::app::ConnectionEvent};

    fn render(app: &App, now: Instant) -> String {
        let mut terminal = Terminal::new(TestBackend::new(72, 12)).unwrap();
        terminal.draw(|frame| draw(frame, app, now)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn attached_shows_the_server_and_an_empty_body() {
        let mut app = App::new(&[]);
        app.on_connection(
            0,
            ConnectionEvent::Attached {
                server_version: "0.0.0".to_owned(),
                pid: 4242,
            },
        );
        let screen = render(&app, Instant::now());
        assert!(
            screen.contains("fpk · local · server 0.0.0 · pid 4242"),
            "{screen}"
        );
        assert!(screen.contains("Machines"), "{screen}");
        assert!(screen.contains("● local   attached"), "{screen}");
        assert!(screen.contains("Nothing to browse yet."), "{screen}");
        assert!(screen.contains("j/k move · q detach"), "{screen}");
        assert!(!screen.contains("start the server"), "{screen}");
    }

    #[test]
    fn disconnected_counts_down_and_offers_to_start() {
        let now = Instant::now();
        let mut app = App::new(&[]);
        app.on_connection(
            0,
            ConnectionEvent::Disconnected {
                retry_at: now + Duration::from_millis(3_500),
                reason: None,
            },
        );
        let screen = render(&app, now);
        assert!(screen.contains("fpk · local · disconnected"), "{screen}");
        assert!(screen.contains("local   retry 4s"), "{screen}");
        assert!(
            screen.contains("Retrying in 4s. Press s to start it."),
            "{screen}"
        );
        assert!(screen.contains("s start the server"), "{screen}");
    }

    #[test]
    fn the_lower_half_lists_tasks_in_submission_order() {
        let mut app = App::new(&[]);
        app.on_connection(
            0,
            ConnectionEvent::Attached {
                server_version: "0.0.0".to_owned(),
                pid: 1,
            },
        );
        let screen = render(&app, Instant::now());
        assert!(screen.contains("Tasks"), "{screen}");
        assert!(screen.contains("No tasks"), "{screen}");

        let task = |id: &str, submitted, status, measure| TaskView {
            id: id.to_owned(),
            parent: None,
            kind: "demo.count".to_owned(),
            subject: Vec::new(),
            status,
            phase: None,
            measure,
            submitted_unix_millis: submitted,
        };
        // Fed as live changes: a settled task arriving in a first snapshot is
        // one this client never saw end, and is not shown.
        for task in [
            task("a", 1, TaskStatus::Succeeded, Measure::Indeterminate),
            task(
                "b",
                2,
                TaskStatus::Failed("no".to_owned()),
                Measure::Indeterminate,
            ),
            task(
                "c",
                3,
                TaskStatus::Running,
                Measure::Items {
                    done: 21,
                    total: Some(50),
                },
            ),
            task(
                "d",
                4,
                TaskStatus::Queued { on_resource: false },
                Measure::Indeterminate,
            ),
        ] {
            app.on_connection(0, ConnectionEvent::Tasks(TaskUpdate::Changed(task)));
        }
        let screen = render(&app, Instant::now());
        let rows: Vec<&str> = screen
            .lines()
            .filter(|line| line.contains("demo.count"))
            .collect();
        assert_eq!(rows.len(), 4, "{screen}");
        assert!(rows[0].contains("✓ demo.count  done"), "{screen}");
        assert!(rows[1].contains("✗ demo.count  failed"), "{screen}");
        assert!(rows[2].contains("◐ demo.count  42%"), "{screen}");
        assert!(rows[3].contains("○ demo.count  queued"), "{screen}");
        assert!(!screen.contains("No tasks"), "{screen}");
    }

    #[test]
    fn several_machines_list_in_order_and_each_shows_its_own_state() {
        let now = Instant::now();
        let mut app = App::new(&[
            ("workbox".to_owned(), "me@workbox".to_owned()),
            ("nano4".to_owned(), "nano4".to_owned()),
        ]);
        app.on_connection(
            0,
            ConnectionEvent::Attached {
                server_version: "0.0.0".to_owned(),
                pid: 1,
            },
        );
        app.on_connection(
            1,
            ConnectionEvent::NeedsAttention {
                attention: Attention::NotInstalled,
                retry_at: now + Duration::from_secs(8),
            },
        );
        app.on_connection(
            2,
            ConnectionEvent::NeedsAttention {
                attention: Attention::NeedsLogin,
                retry_at: now + Duration::from_secs(2),
            },
        );
        let screen = render(&app, now);
        let rows: Vec<&str> = screen.lines().filter(|line| line.contains("● ")).collect();
        assert!(rows[0].contains("local   attached"), "{screen}");
        assert!(rows[1].contains("workbox attention"), "{screen}");
        assert!(rows[2].contains("nano4   attention"), "{screen}");

        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('j'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let screen = render(&app, now);
        assert!(
            screen.contains("fpk · workbox · needs attention"),
            "{screen}"
        );
        assert!(
            screen.contains("fpk is not installed on workbox."),
            "{screen}"
        );
        assert!(
            screen.contains("fpk machine add workbox me@workbox"),
            "{screen}"
        );

        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('j'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let screen = render(&app, now);
        assert!(screen.contains("nano4 wants you to sign in."), "{screen}");
        assert!(screen.contains("fpk machine login nano4"), "{screen}");
    }

    #[test]
    fn a_disconnect_says_why_when_it_knows() {
        let now = Instant::now();
        let mut app = App::new(&[]);
        app.on_connection(
            0,
            ConnectionEvent::Disconnected {
                retry_at: now + Duration::from_secs(1),
                reason: Some("its server is not running".to_owned()),
            },
        );
        let screen = render(&app, now);
        assert!(screen.contains("its server is not running"), "{screen}");
    }

    /// The column and row where `text` is drawn, counted in cells.
    fn find(screen: &str, text: &str) -> (u16, u16) {
        for (row, line) in screen.lines().enumerate() {
            if let Some(byte) = line.find(text) {
                let column = line[..byte].chars().count();
                return (u16::try_from(column).unwrap(), u16::try_from(row).unwrap());
            }
        }
        panic!("{text:?} is not on screen:\n{screen}");
    }

    #[test]
    fn the_mouse_finds_what_was_drawn_under_it() {
        let now = Instant::now();
        let screen_area = Rect::new(0, 0, 72, 12);
        let mut app = App::new(&[("workbox".to_owned(), "me@workbox".to_owned())]);
        app.on_connection(
            0,
            ConnectionEvent::Disconnected {
                retry_at: now + Duration::from_secs(1),
                reason: None,
            },
        );
        let screen = render(&app, now);
        let at = |text: &str| {
            let (column, row) = find(&screen, text);
            spot(&app, screen_area, column, row)
        };

        assert_eq!(at("● local"), Some(Spot::Machine(0)));
        assert_eq!(at("● workbox"), Some(Spot::Machine(1)));
        let (column, row) = find(&screen, "● workbox");
        assert_eq!(
            spot(&app, screen_area, column, row + 1),
            Some(Spot::Machines)
        );
        assert_eq!(at("Machines"), None);
        assert_eq!(at("Tasks"), None);

        assert_eq!(at("q detach"), Some(Spot::Hint(Hint::Detach)));
        assert_eq!(
            at("s start the server"),
            Some(Spot::Hint(Hint::StartServer))
        );
        let (column, row) = find(&screen, "s start the server");
        assert_eq!(
            spot(&app, screen_area, column + 17, row),
            Some(Spot::Hint(Hint::StartServer))
        );
        assert_eq!(at("j/k move"), None);
        assert_eq!(spot(&app, screen_area, 71, row), None);
    }

    #[test]
    fn countdown_rounds_up_and_stops_at_zero() {
        let now = Instant::now();
        assert_eq!(countdown(now + Duration::from_millis(1), now), "1s");
        assert_eq!(countdown(now + Duration::from_secs(2), now), "2s");
        assert_eq!(countdown(now, now + Duration::from_secs(5)), "0s");
    }
}
