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
        // The prompt takes a line above the list it narrows, and none at all when
        // it is closed, so the list reads downwards from what was typed.
        let prompt_height = u16::from(
            self.active_context()
                .is_some_and(|context| context.filter().is_some()),
        );
        let [header, tabs, prompt, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(prompt_height),
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
        self.render_filter_prompt(frame, prompt);

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
                viewport.rows.iter().copied().map(row_list_item),
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

    /// Draws the filter query above the list, with what it matched beside it.
    ///
    /// `area` is empty unless a prompt is open, so this draws nothing the rest of
    /// the time.
    fn render_filter_prompt(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.height == 0 {
            return;
        }
        let Some(context) = self.active_context() else {
            return;
        };
        let Some(query) = context.filter() else {
            return;
        };
        // Each `/` that stepped into a directory shows as the path segment it
        // committed, so the prompt reads as the place the query is being applied.
        let entered = context
            .filter_scope()
            .map_or_else(|| "/".to_owned(), |scope| format!("/{scope}/"));
        // What the last `/` refused to do outranks the count, since it is the answer
        // to the key just pressed.
        let matches = context.filter_matches();
        let warning = context.filter_warning();
        let summary = match (warning, matches) {
            (Some(warning), _) => format!("  {warning}"),
            (None, None) => String::new(),
            (None, Some(0)) => "  no matches".to_owned(),
            (None, Some(1)) => "  1 match".to_owned(),
            (None, Some(count)) => format!("  {count} matches"),
        };
        let summary_style = if warning.is_some() || matches == Some(0) {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let query_style = if matches == Some(0) {
            Style::default().fg(Color::Red)
        } else {
            input_style()
        };
        let cursor = entered.chars().count() + query.chars().count();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(entered, Style::default().fg(Color::DarkGray)),
                Span::styled(query.to_owned(), query_style),
                Span::styled(summary, summary_style),
            ])),
            area,
        );
        // A real cursor is the cue that keys are going into the prompt rather than
        // the list.
        if let Ok(cursor) = u16::try_from(cursor)
            && cursor < area.width
        {
            frame.set_cursor_position((area.x + cursor, area.y));
        }
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
                "/: filter  y: copy  p: paste  [: previous  ]: next  n: new browser  x: close  S: connections  q/Esc: quit"
            }
            super::InputMode::Filter => {
                "Tab: complete  /: open directory  ↑/↓ or Ctrl-N/P: select  Ctrl-D/U: scroll  Enter: fill in, then keep  Esc: cancel"
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
    use std::time::Duration;

    use ratatui::{Terminal, backend::TestBackend};

    use super::App;
    use crate::tui::browser_context::ListingStatus;

    fn rendered(app: &mut App, width: u16, height: u16) -> String {
        rendered_lines(app, width, height).concat()
    }

    fn rendered_lines(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|row| {
                (0..width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect()
            })
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

    /// A browser whose listing of three files has been fully applied.
    async fn listed(directory: &std::path::Path) -> App {
        for name in ["alpha.txt", "beta.txt", "gamma.txt"] {
            tokio::fs::write(directory.join(name), b"").await.unwrap();
        }
        let mut app = App::new();
        app.start(Some(directory.to_str().unwrap())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                app.pump_events();
                if app
                    .active_context()
                    .is_some_and(|context| context.status() == &ListingStatus::Complete)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        app
    }

    /// The line index of the file list's first and last row, read off its border.
    fn list_bounds(lines: &[String]) -> (usize, usize) {
        let top = lines
            .iter()
            .position(|line| line.contains('┌') && line.contains("Files"))
            .expect("the file list is drawn with a border");
        let bottom = lines
            .iter()
            .rposition(|line| line.contains('└'))
            .expect("the file list border is closed");
        (top + 1, bottom - 1)
    }

    /// The query is read downwards into the matches: it takes the line directly
    /// above the list, and the matches start at the list's top edge.
    #[tokio::test]
    async fn the_filter_prompt_sits_directly_above_the_matches() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = listed(directory.path()).await;

        let closed = rendered_lines(&mut app, 60, 16);
        let (closed_first, _) = list_bounds(&closed);
        assert!(
            closed[closed_first].contains("alpha.txt"),
            "an unfiltered list starts at the top edge"
        );

        app.open_filter();
        for character in "beta".chars() {
            app.extend_filter(character);
        }
        let open = rendered_lines(&mut app, 60, 16);
        let (first, _) = list_bounds(&open);
        assert_eq!(
            first,
            closed_first + 1,
            "the prompt takes a line of its own, and none while it is closed"
        );
        assert!(
            open[first - 2].contains("/beta"),
            "the query is echoed on the line above the list"
        );
        assert!(open[first - 2].contains("1 match"));
        assert!(
            open[first].contains("beta.txt"),
            "the match sits on the first row of the pane, under the query"
        );

        // A `/` that refused to open anything answers on the same line.
        app.descend_filter();
        assert!(rendered(&mut app, 60, 16).contains("no exact match to open"));

        app.extend_filter('z');
        assert!(rendered(&mut app, 60, 16).contains("no matches"));

        app.abandon_filter();
        let closed = rendered_lines(&mut app, 60, 16);
        let (first, _) = list_bounds(&closed);
        assert!(closed[first].contains("alpha.txt"));
        assert!(closed.concat().contains("/: filter"));

        app.shutdown().await.unwrap();
    }
}
