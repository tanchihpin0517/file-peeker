use crate::EntryKind;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use super::{
    App, BrowserTab, ConnectionRecordId, NewBrowserDialog, Overlay, connection::ConnectionStatus,
};
use crate::tui::browser_context::{BrowserRow, ExpansionStatus, ListingStatus};

impl App {
    pub(crate) fn render(&mut self, frame: &mut Frame<'_>) {
        let [header, tabs, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .areas(frame.area());

        let header_text = self.active_context_id.and_then(|id| {
            let tab = self.contexts.get(&id)?;
            let label = &self.connections.get(&tab.connection_id)?.label;
            Some(format!("{label}  {}", tab.context.root_path()))
        });
        frame.render_widget(
            Paragraph::new(header_text.as_deref().unwrap_or("No active browser")).block(
                Block::default()
                    .title(" File Peeker ")
                    .borders(Borders::ALL),
            ),
            header,
        );
        self.render_tabs(frame, tabs);

        if self.active_context().is_some() {
            let block = file_list_block();
            let height = usize::from(block.inner(body).height);
            let context = self
                .active_context_mut()
                .expect("active context was checked before rendering");
            // Only the visible rows become list items, so the frame costs what is
            // on screen rather than what the tree holds.
            let viewport = context.viewport(height);
            render_file_list(
                frame,
                body,
                block,
                viewport.rows.iter().map(row_list_item),
                viewport.selected,
            );
        } else {
            frame.render_widget(
                Paragraph::new("No browser contexts\n\nS: connections  q/Esc: quit")
                    .alignment(Alignment::Center)
                    .block(Block::default().title(" Files ").borders(Borders::ALL)),
                body,
            );
        }

        frame.render_widget(Paragraph::new(self.footer_text()), footer);
        self.render_overlay(frame);
    }

    fn render_tabs(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.width == 0 {
            return;
        }
        let labels: Vec<_> = self
            .context_order
            .iter()
            .filter_map(|id| {
                let tab = self.contexts.get(id)?;
                let connection = self.connections.get(&tab.connection_id)?;
                Some((*id, tab_label(&connection.label, tab)))
            })
            .collect();
        if labels.is_empty() {
            frame.render_widget(Paragraph::new(" (no tabs) "), area);
            return;
        }
        let active = self
            .active_context_id
            .and_then(|id| labels.iter().position(|(candidate, _)| *candidate == id))
            .unwrap_or(0);
        let width = usize::from(area.width);
        let mut start = active;
        let mut used = labels
            .get(active)
            .map_or(0, |(_, label)| label.chars().count() + 3);
        while start > 0 {
            let needed = labels[start - 1].1.chars().count() + 3;
            if used + needed > width {
                break;
            }
            start -= 1;
            used += needed;
        }
        let mut end = active.saturating_add(1);
        while end < labels.len() {
            let needed = labels[end].1.chars().count() + 3;
            if used + needed > width {
                break;
            }
            used += needed;
            end += 1;
        }
        let spans = labels[start..end].iter().map(|(id, label)| {
            let style = if Some(*id) == self.active_context_id {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Span::styled(format!(" {label} "), style)
        });
        frame.render_widget(Paragraph::new(spans.collect::<Line>()), area);
    }

    fn footer_text(&self) -> String {
        let controls = match self.input_mode() {
            super::InputMode::Main if self.active_context_id.is_none() => {
                "S: connections  q/Esc: quit"
            }
            super::InputMode::Main => {
                "y: copy  p: paste  [: previous  ]: next  n: new browser  x: close  S: connections  q/Esc: quit"
            }
            super::InputMode::ConnectionManager => {
                "j/k: select  Enter: activate  n: new browser  a: add  d: close  q/Esc: dismiss"
            }
            super::InputMode::TargetChooser => "j/k: select  Enter: choose  Esc: cancel",
            super::InputMode::RemoteInput | super::InputMode::NewBrowser => {
                "Enter: submit  Backspace: edit  Esc: cancel"
            }
            super::InputMode::ConnectionCloseConfirmation => {
                "Enter/y: close connection  q/Esc: cancel"
            }
            super::InputMode::TransferOverwriteConfirmation => "Enter/y: overwrite  q/Esc: cancel",
        };
        let browser_status = self.active_context().map_or_else(
            || "Ready".into(),
            |context| {
                if let Some((path, error)) = context.selected_expansion_error() {
                    format!("Error expanding {path}: {error}")
                } else {
                    match context.status() {
                        ListingStatus::Loading => "Loading…".into(),
                        ListingStatus::Complete => format!("{} items", context.rows().len()),
                        ListingStatus::Failed(error) => format!("Error: {error}"),
                    }
                }
            },
        );
        let transfer_status = self.status_message.as_deref().unwrap_or(&browser_status);
        let status = self.status_error.as_deref().map_or_else(
            || transfer_status.to_owned(),
            |error| format!("{error}  Current browser: {browser_status}"),
        );
        format!("{status}  {controls}")
    }

    fn render_overlay(&self, frame: &mut Frame<'_>) {
        match &self.overlay {
            Overlay::None => {}
            Overlay::ConnectionManager => self.render_connection_manager(frame),
            Overlay::TargetChooser { selected_ssh } => {
                render_choice(frame, *selected_ssh);
            }
            Overlay::RemoteInput { input } => render_text_dialog(
                frame,
                " Add SSH Connection ",
                &[
                    Line::from("SSH destination:"),
                    Line::styled(input, input_style()),
                ],
            ),
            Overlay::NewBrowser(dialog) => self.render_new_browser(frame, dialog),
            Overlay::CloseConfirmation { connection_id } => {
                self.render_close_confirmation(frame, *connection_id);
            }
            Overlay::TransferOverwriteConfirmation(request) => {
                self.render_transfer_overwrite_confirmation(frame, request);
            }
        }
    }

    fn render_connection_manager(&self, frame: &mut Frame<'_>) {
        let area = popup_area(frame.area(), 76, 18);
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title(" Connection Manager ")
            .borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if self.connection_order.is_empty() {
            frame.render_widget(
                Paragraph::new("No connections\n\na: add connection").alignment(Alignment::Center),
                inner,
            );
            return;
        }
        let [list_area, detail_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(inner);
        let items = self.connection_order.iter().filter_map(|id| {
            let record = self.connections.get(id)?;
            debug_assert_eq!(record.id, *id);
            let count = self
                .contexts
                .values()
                .filter(|tab| tab.connection_id == *id)
                .count();
            let status = match &record.status {
                ConnectionStatus::Starting => "Starting".into(),
                ConnectionStatus::Ready => "Ready".into(),
                ConnectionStatus::Failed(error) => format!("Failed: {error}"),
                ConnectionStatus::Closing => "Closing".into(),
            };
            Some(ListItem::new(format!(
                "{}  {}  {status}  {count} browser(s)",
                record.label,
                record.target_summary()
            )))
        });
        let selected = self.selected_connection_id.and_then(|id| {
            self.connection_order
                .iter()
                .position(|candidate| *candidate == id)
        });
        let mut state = ListState::default().with_selected(selected);
        frame.render_stateful_widget(
            List::new(items)
                .highlight_symbol("> ")
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
            list_area,
            &mut state,
        );
        if let Some(error) = self
            .selected_connection_id
            .and_then(|id| self.connections.get(&id))
            .and_then(|record| match &record.status {
                ConnectionStatus::Failed(error) => Some(error.as_str()),
                _ => None,
            })
        {
            frame.render_widget(
                Paragraph::new(format!("Startup error: {error}"))
                    .style(Style::default().fg(Color::Red))
                    .wrap(Wrap { trim: false }),
                detail_area,
            );
        }
    }

    fn render_new_browser(&self, frame: &mut Frame<'_>, dialog: &NewBrowserDialog) {
        let label = self
            .connections
            .get(&dialog.connection_id)
            .map_or("Unknown", |record| record.label.as_str());
        let mut lines = vec![
            Line::from(format!("Connection: {label}")),
            Line::from("Path:"),
            Line::styled(dialog.input.as_str(), input_style()),
        ];
        if dialog.request_id.is_some() {
            lines.push(Line::from(""));
            lines.push(Line::from("Resolving…"));
        }
        if let Some(error) = &dialog.error {
            lines.push(Line::from(""));
            lines.push(Line::styled(
                format!("Error: {error}"),
                Style::default().fg(Color::Red),
            ));
        }
        render_text_dialog(frame, " New Browser ", &lines);
    }

    fn render_close_confirmation(&self, frame: &mut Frame<'_>, id: ConnectionRecordId) {
        let Some(record) = self.connections.get(&id) else {
            return;
        };
        let count = self
            .contexts
            .values()
            .filter(|tab| tab.connection_id == id)
            .count();
        render_text_dialog(
            frame,
            " Close Connection ",
            &[
                Line::from(format!("Close connection \"{}\"?", record.label)),
                Line::from(format!("{count} browser context(s) will close.")),
                Line::from(""),
                Line::from("Press Enter or y to confirm."),
            ],
        );
    }

    fn render_transfer_overwrite_confirmation(
        &self,
        frame: &mut Frame<'_>,
        request: &super::TransferRequest,
    ) {
        let label = self
            .connections
            .get(&request.destination_connection_id)
            .map_or("Unknown", |record| record.label.as_str());
        render_text_dialog(
            frame,
            " Replace Existing File ",
            &[
                Line::from(format!(
                    "\"{}\" already exists in connection \"{label}\".",
                    request.source_name
                )),
                Line::from(format!("Destination: {}", request.destination_root)),
                Line::from(""),
                Line::from("Press Enter or y to overwrite it."),
            ],
        );
    }
}

fn tab_label(connection_label: &str, tab: &BrowserTab) -> String {
    let path = tab.context.root_path();
    let compact = if path == "/" {
        "/"
    } else {
        path.trim_end_matches('/')
            .rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or(path)
    };
    format!("{connection_label}:{compact}")
}

fn file_list_block() -> Block<'static> {
    Block::default().title(" Files ").borders(Borders::ALL)
}

/// Renders exactly the rows it is given, at offset zero.
///
/// Scrolling lives in [`BrowserContext::viewport`](crate::tui::browser_context::BrowserContext::viewport)
/// rather than in `ListState`: the widget cannot scroll a window it can only see
/// one screenful of, and computing the offset there is what lets the caller pass
/// a window at all.
fn render_file_list<'a>(
    frame: &mut Frame<'_>,
    area: Rect,
    block: Block<'static>,
    entries: impl IntoIterator<Item = ListItem<'a>>,
    selected: Option<usize>,
) {
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(
        List::new(entries)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> "),
        area,
        &mut state,
    );
}

fn row_list_item(row: &BrowserRow) -> ListItem<'static> {
    let (_, style) = entry_appearance(row.entry().kind);
    let prefix = if row.entry().navigable {
        match row.expansion() {
            ExpansionStatus::Collapsed => "▸ ",
            ExpansionStatus::Loading { .. } => "… ",
            ExpansionStatus::Expanded { .. } => "▾ ",
            ExpansionStatus::Failed { .. } => "! ",
        }
    } else {
        entry_appearance(row.entry().kind).0
    };
    ListItem::new(Line::styled(
        format!("{}{prefix}{}", "  ".repeat(row.depth()), row.entry().name),
        style,
    ))
}

fn entry_appearance(kind: EntryKind) -> (&'static str, Style) {
    match kind {
        EntryKind::File => ("  ", Style::default()),
        EntryKind::Directory => (
            "▸ ",
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ),
        EntryKind::Symlink => ("@ ", Style::default().fg(Color::Cyan)),
        EntryKind::Other => ("? ", Style::default().fg(Color::Yellow)),
    }
}

fn render_choice(frame: &mut Frame<'_>, selected_ssh: bool) {
    let area = popup_area(frame.area(), 34, 8);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Add Connection ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut state = ListState::default().with_selected(Some(usize::from(selected_ssh)));
    frame.render_stateful_widget(
        List::new(["Local", "SSH"])
            .highlight_symbol("> ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        inner,
        &mut state,
    );
}

fn render_text_dialog(frame: &mut Frame<'_>, title: &str, lines: &[Line<'_>]) {
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(4)
        .clamp(6, 18);
    let area = popup_area(frame.area(), 72, height);
    frame.render_widget(Clear, area);
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(Text::from(lines.to_vec())).wrap(Wrap { trim: false }),
        inner,
    );
}

fn popup_area(area: Rect, preferred_width: u16, preferred_height: u16) -> Rect {
    let width = preferred_width.min(area.width.saturating_sub(2).max(1));
    let height = preferred_height.min(area.height.saturating_sub(2).max(1));
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn input_style() -> Style {
    Style::default().fg(Color::Cyan)
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::App;

    fn rendered(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn connectionless_startup_has_actionable_manager_and_shell() {
        let mut app = App::new();
        let output = rendered(&mut app, 80, 24);
        assert!(output.contains("Connection Manager"));
        assert!(output.contains("a: add connection"));
        assert!(output.contains("File Peeker"));
    }

    #[tokio::test]
    async fn active_tab_header_and_path_render() {
        let mut app = App::new();
        app.start(Some(".")).await.unwrap();
        let output = rendered(&mut app, 80, 24);
        assert!(output.contains("Local:"));
        assert!(output.contains("Local  "));
        assert!(output.contains("Files"));
        assert!(!output.contains("Locations"));
        app.shutdown().await.unwrap();
    }
}
