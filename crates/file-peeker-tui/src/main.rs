use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ActivePane {
    Sidebar,
    #[default]
    Browser,
}

#[derive(Debug, Default)]
struct App {
    active_pane: ActivePane,
}

impl App {
    fn toggle_pane(&mut self) {
        self.active_pane = match self.active_pane {
            ActivePane::Sidebar => ActivePane::Browser,
            ActivePane::Browser => ActivePane::Sidebar,
        };
    }

    fn render(&self, frame: &mut Frame<'_>) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .areas(frame.area());

        frame.render_widget(
            Paragraph::new("Home").block(
                Block::default()
                    .title(" File Peeker ")
                    .borders(Borders::ALL),
            ),
            header,
        );

        let [sidebar, browser] = body_layout(body);
        let mut sidebar_state = ListState::default()
            .with_selected((self.active_pane == ActivePane::Sidebar).then_some(0));
        frame.render_stateful_widget(
            List::new([ListItem::new("Home")])
                .block(
                    Block::default()
                        .title(" Locations ")
                        .borders(Borders::ALL)
                        .border_style(pane_border_style(self.active_pane == ActivePane::Sidebar)),
                )
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("> "),
            sidebar,
            &mut sidebar_state,
        );

        frame.render_widget(
            List::new(Vec::<ListItem>::new()).block(
                Block::default()
                    .title(" Files ")
                    .borders(Borders::ALL)
                    .border_style(pane_border_style(self.active_pane == ActivePane::Browser)),
            ),
            browser,
        );

        frame.render_widget(
            Paragraph::new("Tab/Shift-Tab: switch pane  q/Esc: quit"),
            footer,
        );
    }
}

fn main() -> io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut app = App::default();
    loop {
        terminal.draw(|frame| app.render(frame))?;
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Tab | KeyCode::BackTab => app.toggle_pane(),
                _ => {}
            }
        }
    }
}

fn sidebar_width(total_width: u16) -> u16 {
    if total_width < 38 {
        return total_width / 3;
    }

    (total_width / 4)
        .clamp(18, 30)
        .min(total_width.saturating_sub(20))
}

fn body_layout(area: Rect) -> [Rect; 2] {
    Layout::horizontal([
        Constraint::Length(sidebar_width(area.width)),
        Constraint::Min(1),
    ])
    .areas(area)
}

fn pane_border_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::{ActivePane, App, sidebar_width};

    #[test]
    fn browser_is_focused_initially() {
        assert_eq!(App::default().active_pane, ActivePane::Browser);
    }

    #[test]
    fn pane_focus_toggles() {
        let mut app = App::default();
        app.toggle_pane();
        assert_eq!(app.active_pane, ActivePane::Sidebar);
        app.toggle_pane();
        assert_eq!(app.active_pane, ActivePane::Browser);
    }

    #[test]
    fn sidebar_width_is_responsive_and_capped() {
        assert_eq!(sidebar_width(30), 10);
        assert_eq!(sidebar_width(40), 18);
        assert_eq!(sidebar_width(80), 20);
        assert_eq!(sidebar_width(120), 30);
        assert_eq!(sidebar_width(200), 30);
    }
}
