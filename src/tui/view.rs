//! Draws [`App`]. A pure function of the state and the time, so it is tested
//! against ratatui's test backend rather than a terminal.

use std::time::Instant;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use super::app::{App, Machine, MachineState};

const SIDEBAR_WIDTH: u16 = 24;

pub(super) fn draw(frame: &mut Frame<'_>, app: &App, now: Instant) {
    let [title, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(body);

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

fn draw_sidebar(frame: &mut Frame<'_>, app: &App, now: Instant, area: Rect) {
    let block = Block::new().borders(Borders::RIGHT).title(Span::styled(
        " Machines",
        Style::new().add_modifier(Modifier::BOLD),
    ));
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
    use crate::tui::app::ConnectionEvent;

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
    fn countdown_rounds_up_and_stops_at_zero() {
        let now = Instant::now();
        assert_eq!(countdown(now + Duration::from_millis(1), now), "1s");
        assert_eq!(countdown(now + Duration::from_secs(2), now), "2s");
        assert_eq!(countdown(now, now + Duration::from_secs(5)), "0s");
    }
}
