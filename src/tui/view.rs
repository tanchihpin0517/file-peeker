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

use super::app::{App, Machine, MachineState};
use crate::client::tasks::{Measure, TaskStatus, TaskView};

const SIDEBAR_WIDTH: u16 = 26;

pub(super) fn draw(frame: &mut Frame<'_>, app: &App, now: Instant) {
    let [title, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(frame.area());
    let [sidebar, pane] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(body);
    // The key hints belong to the main pane and sit under it alone; the sidebar
    // runs the full height beside them.
    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(pane);

    let machine = app.selected_machine();
    frame.render_widget(title_line(machine), title);
    draw_sidebar(frame, app, now, sidebar);
    draw_main(frame, machine, now, main);
    frame.render_widget(status_line(machine), status);
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
    };
    Paragraph::new(Line::from(vec![
        Span::styled(" fpk ", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(format!("· {} · {detail}", machine.label)),
    ]))
    .style(Style::new().add_modifier(Modifier::REVERSED))
}

/// Split in half as herdr's is: machines above, the selected machine's tasks
/// below.
fn draw_sidebar(frame: &mut Frame<'_>, app: &App, now: Instant, area: Rect) {
    let [machines, tasks] =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);

    let block = Block::new()
        .borders(Borders::RIGHT)
        .title(Span::styled(" Machines", BOLD));
    let inner = block.inner(machines);
    frame.render_widget(block, machines);
    let rows: Vec<Line<'_>> = app
        .machines()
        .iter()
        .enumerate()
        .map(|(index, machine)| machine_row(machine, now, index == app.selected()))
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);

    let machine = app.selected_machine();
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
        MachineState::Disconnected { retry_at } => {
            (Color::Red, format!("retry {}", countdown(*retry_at, now)))
        }
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
        MachineState::Disconnected { retry_at } => vec![
            Line::from("Disconnected from the server."),
            Line::from(format!(
                "Retrying in {}. Press s to start it.",
                countdown(*retry_at, now)
            )),
        ],
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
    let mut keys = String::from(" j/k move · q detach");
    if matches!(machine.state, MachineState::Disconnected { .. }) {
        keys.push_str(" · s start the server");
    }
    Paragraph::new(keys).style(Style::new().add_modifier(Modifier::DIM))
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
        let mut app = App::new();
        app.on_connection(ConnectionEvent::Attached {
            server_version: "0.0.0".to_owned(),
            pid: 4242,
        });
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
        let mut app = App::new();
        app.on_connection(ConnectionEvent::Disconnected {
            retry_at: now + Duration::from_millis(3_500),
        });
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
        let mut app = App::new();
        app.on_connection(ConnectionEvent::Attached {
            server_version: "0.0.0".to_owned(),
            pid: 1,
        });
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
            app.on_connection(ConnectionEvent::Tasks(TaskUpdate::Changed(task)));
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
    fn countdown_rounds_up_and_stops_at_zero() {
        let now = Instant::now();
        assert_eq!(countdown(now + Duration::from_millis(1), now), "1s");
        assert_eq!(countdown(now + Duration::from_secs(2), now), "2s");
        assert_eq!(countdown(now, now + Duration::from_secs(5)), "0s");
    }
}
