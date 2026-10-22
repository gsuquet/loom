//! Pure ratatui rendering of queues, object pages and modals (ADR-0014).

#![allow(clippy::option_if_let_else)]

use loom_api::{ContextName, Importance, SearchResult, Status, TaskDetail};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use super::keymap::{Layer, ModalKind, QueueTab, ViewKind, bindings_for_layer};
use super::state::{AppState, ObjectPageContent, ObjectPageState, ObjectTarget, PREVIEW_MIN_WIDTH};

const fn importance_label(importance: Importance) -> &'static str {
    match importance {
        Importance::P0 => "p0",
        Importance::P1 => "p1",
        Importance::P2 => "p2",
        Importance::P3 => "p3",
    }
}

/// Draws the entire TUI frame for `state`.
pub fn render(frame: &mut Frame<'_>, state: &AppState) {
    let area = frame.area();
    if area.height == 0 || area.width == 0 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Top bar
            Constraint::Min(1),    // Main area
            Constraint::Length(1), // Bottom bar
        ])
        .split(area);

    render_top_bar(frame, chunks[0], state);

    if let Some(page) = state.page_stack.last() {
        render_object_page(frame, chunks[1], state, page);
    } else {
        render_queue_view(frame, chunks[1], state, area.width);
    }

    render_footer(frame, chunks[2], state);

    if let Some(modal) = state.modal {
        render_modal(frame, area, state, modal);
    }
}

fn render_top_bar(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw(" "));

    for (idx, tab) in QueueTab::ALL.into_iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" │ ", state.theme.dim_style()));
        }
        let count = match tab {
            QueueTab::Today => state.today.len(),
            QueueTab::Inbox => state.inbox.len(),
            QueueTab::Waiting => state.waiting.len(),
        };
        let text = format!("{} {} {}", tab.shortcut(), tab.label(), count);
        let style = if state.page_stack.is_empty() && state.active_tab == tab {
            state.theme.header_style()
        } else {
            state.theme.dim_style()
        };
        spans.push(Span::styled(text, style));
    }

    spans.push(Span::raw("   "));
    match &state.active_context {
        Some(ctx) => {
            spans.push(Span::styled(
                format!("[{ctx}]"),
                state.theme.context_badge(ctx.as_str()),
            ));
        }
        None => {
            spans.push(Span::styled("[all]", state.theme.context_badge("all")));
        }
    }

    spans.push(Span::raw("   "));
    spans.push(Span::styled(
        state.sse_state.label(),
        state.theme.dim_style(),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_queue_view(frame: &mut Frame<'_>, area: Rect, state: &AppState, width: u16) {
    if state.preview_enabled && width >= PREVIEW_MIN_WIDTH {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area);
        render_queue_list(frame, panes[0], state);
        render_preview_pane(frame, panes[1], state);
    } else {
        render_queue_list(frame, area, state);
    }
}

fn render_queue_list(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let ctx_label = state
        .active_context
        .as_ref()
        .map_or("all", ContextName::as_str);
    let title = format!(" {} · [{}] ", state.active_tab.label(), ctx_label);
    let block = Block::default().borders(Borders::ALL).title(title);

    let selected = state.current_selected();
    let tasks = match state.active_tab {
        QueueTab::Today => &state.today,
        QueueTab::Inbox => &state.inbox,
        QueueTab::Waiting => &state.waiting,
    };

    if tasks.is_empty() {
        let empty = Paragraph::new(" No tasks in this queue.").block(block);
        frame.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem<'_>> = tasks
        .iter()
        .enumerate()
        .map(|(idx, summary)| {
            let is_sel = idx == selected;
            let mut spans: Vec<Span<'_>> = Vec::new();

            if is_sel {
                spans.push(Span::raw("▶ "));
            } else {
                spans.push(Span::raw("  "));
            }

            if let Some(task) = &summary.task {
                let imp = task.importance;
                spans.push(Span::styled(
                    format!("{:<3}", importance_label(imp)),
                    state.theme.importance_style(imp),
                ));

                let status_style = state.theme.status_style(task.status.name());
                let status_str = match &task.status {
                    Status::Inbox => "inbox",
                    Status::Ready => "ready",
                    Status::Active => "active",
                    Status::Waiting { .. } => "waiting",
                    Status::Done => "done",
                    Status::Cancelled => "cancelled",
                    Status::Unknown(name) => name.as_str(),
                };
                spans.push(Span::styled(format!("{status_str:<8}"), status_style));
            } else {
                spans.push(Span::styled("   ", state.theme.dim_style()));
                spans.push(Span::styled("        ", state.theme.dim_style()));
            }

            spans.push(Span::styled(
                format!("[{}] ", summary.context),
                state.theme.context_badge(summary.context.as_str()),
            ));

            if let Some(task) = &summary.task {
                spans.push(Span::styled(
                    format!("{:<5}", task.kind.name()),
                    state.theme.dim_style(),
                ));
                spans.push(Span::raw(task.title.clone()));
            } else {
                spans.push(Span::raw(summary.id.to_string()));
            }

            let style = if is_sel {
                state.theme.selected_style()
            } else {
                ratatui::style::Style::default()
            };
            ListItem::new(Line::from(spans)).style(style)
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn render_preview_pane(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let block = Block::default().borders(Borders::ALL).title(" Preview ");
    if let Some(detail) = &state.preview_detail {
        render_task_detail(frame, area, block, detail, 0);
    } else if let Some(summary) = state.selected_task() {
        let loading = Paragraph::new(format!(" Loading {}...", summary.id)).block(block);
        frame.render_widget(loading, area);
    } else {
        let empty = Paragraph::new(" No item selected.").block(block);
        frame.render_widget(empty, area);
    }
}

fn render_object_page(
    frame: &mut Frame<'_>,
    area: Rect,
    _state: &AppState,
    page: &ObjectPageState,
) {
    let title = match &page.target {
        ObjectTarget::Task(id) => format!(" Task {id} "),
    };
    let block = Block::default().borders(Borders::ALL).title(title);

    match &page.content {
        ObjectPageContent::Task(detail) => {
            render_task_detail(frame, area, block, detail, page.scroll);
        }
        ObjectPageContent::Loading(target) => match target {
            ObjectTarget::Task(id) => {
                let p = Paragraph::new(format!(" Loading task {id}...")).block(block);
                frame.render_widget(p, area);
            }
        },
    }
}

fn render_task_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    block: Block<'_>,
    detail: &TaskDetail,
    scroll: usize,
) {
    let mut lines = Vec::new();

    let id_line = Line::from(vec![
        Span::styled("ID: ", ratatui::style::Style::default().bold()),
        Span::raw(detail.summary.id.to_string()),
        Span::raw("   "),
        Span::styled("Context: ", ratatui::style::Style::default().bold()),
        Span::raw(format!("[{}]", detail.summary.context)),
    ]);
    lines.push(id_line);

    if let Some(task) = &detail.summary.task {
        let status_str = match &task.status {
            Status::Inbox => "inbox",
            Status::Ready => "ready",
            Status::Active => "active",
            Status::Waiting { .. } => "waiting",
            Status::Done => "done",
            Status::Cancelled => "cancelled",
            Status::Unknown(name) => name.as_str(),
        };
        let meta_line = Line::from(vec![
            Span::styled("Status: ", ratatui::style::Style::default().bold()),
            Span::raw(status_str),
            Span::raw("   "),
            Span::styled("Importance: ", ratatui::style::Style::default().bold()),
            Span::raw(importance_label(task.importance)),
            Span::raw("   "),
            Span::styled("Kind: ", ratatui::style::Style::default().bold()),
            Span::raw(task.kind.name()),
        ]);
        lines.push(meta_line);

        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("Title: ", ratatui::style::Style::default().bold()),
            Span::raw(task.title.clone()),
        ]));

        if let Some(due) = &task.due {
            lines.push(Line::from(vec![
                Span::styled("Due: ", ratatui::style::Style::default().bold()),
                Span::raw(due.to_string()),
            ]));
        }
    }

    if !detail.body.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "── Notes ──────────────────────────────────────────",
            ratatui::style::Style::default(),
        ));
        for note in detail.body.lines() {
            lines.push(Line::raw(note.to_owned()));
        }
    }

    let skipped = lines.into_iter().skip(scroll).collect::<Vec<_>>();
    let paragraph = Paragraph::new(skipped)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    if let Some(err) = &state.last_error {
        let err_span = Span::styled(format!(" [Error: {err}]"), state.theme.error_style());
        frame.render_widget(Paragraph::new(Line::from(vec![err_span])), area);
        return;
    }

    if let Some(msg) = &state.status_message {
        let msg_span = Span::styled(format!(" {msg}"), state.theme.header_style());
        frame.render_widget(Paragraph::new(Line::from(vec![msg_span])), area);
        return;
    }

    let hints: Vec<(&str, &str)> = if state.page_stack.is_empty() {
        vec![
            ("q", "quit"),
            ("?", "help"),
            (":", "palette"),
            ("/", "search"),
            ("@", "context"),
            ("r", "refresh"),
        ]
    } else {
        vec![
            ("Esc", "back"),
            ("j/k", "scroll"),
            ("r", "refresh"),
            (":", "palette"),
            ("/", "search"),
            ("?", "help"),
            ("q", "quit"),
        ]
    };

    let mut spans = vec![Span::raw(" ")];
    for (idx, (key, desc)) in hints.into_iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(key, state.theme.header_style()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(desc, state.theme.dim_style()));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_modal(frame: &mut Frame<'_>, area: Rect, state: &AppState, modal: ModalKind) {
    let popup = centered_rect(70, 70, area);
    frame.render_widget(Clear, popup);

    match modal {
        ModalKind::Help => render_help_modal(frame, popup, state, modal),
        ModalKind::Palette => render_palette_modal(frame, popup, state, modal),
        ModalKind::ContextPicker => render_context_picker_modal(frame, popup, state, modal),
        ModalKind::Search => render_search_modal(frame, popup, state, modal),
        ModalKind::Accept => render_accept_modal(frame, popup, state, modal),
        ModalKind::Snooze => render_snooze_modal(frame, popup, state, modal),
        ModalKind::NewTask => render_new_task_modal(frame, popup, state, modal),
    }
}

fn render_help_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState, modal: ModalKind) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", modal.title()));

    let mut lines = Vec::new();
    lines.push(Line::styled(
        "Global shortcuts:",
        state.theme.header_style(),
    ));
    for b in bindings_for_layer(Layer::Global) {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<12}", b.label), state.theme.header_style()),
            Span::raw(b.description),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled("Queue view:", state.theme.header_style()));
    for b in bindings_for_layer(Layer::View(ViewKind::Queue)) {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<12}", b.label), state.theme.header_style()),
            Span::raw(b.description),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled("Object page:", state.theme.header_style()));
    for b in bindings_for_layer(Layer::View(ViewKind::ObjectPage)) {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<12}", b.label), state.theme.header_style()),
            Span::raw(b.description),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled("Modals:", state.theme.header_style()));
    for b in bindings_for_layer(Layer::Modal(ModalKind::Help)) {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<12}", b.label), state.theme.header_style()),
            Span::raw(b.description),
        ]));
    }

    let scrolled = lines
        .into_iter()
        .skip(state.help_scroll)
        .collect::<Vec<_>>();
    let p = Paragraph::new(scrolled).block(block);
    frame.render_widget(p, popup);
}

fn render_palette_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState, modal: ModalKind) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", modal.title()));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(popup);

    let input_block = Block::default().borders(Borders::ALL).title(" Command ");
    let query = Paragraph::new(format!(":{}", state.palette_query)).block(input_block);
    frame.render_widget(query, chunks[0]);

    let items: Vec<ListItem<'_>> = state
        .filtered_palette
        .iter()
        .enumerate()
        .map(|(idx, (_, item))| {
            let is_sel = idx == state.palette_selected;
            let prefix = if is_sel { "▶ " } else { "  " };
            let line = Line::from(vec![
                Span::raw(prefix),
                Span::styled(
                    format!("{:<9}", format!("{}:", item.category)),
                    state.theme.dim_style(),
                ),
                Span::raw(item.name.clone()),
            ]);
            let style = if is_sel {
                state.theme.selected_style()
            } else {
                ratatui::style::Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();
    let list = List::new(items).block(block);
    frame.render_widget(list, chunks[1]);
}

fn render_context_picker_modal(
    frame: &mut Frame<'_>,
    popup: Rect,
    state: &AppState,
    modal: ModalKind,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", modal.title()));

    let mut all_contexts = vec!["all".to_owned()];
    for c in &state.contexts {
        all_contexts.push(c.to_string());
    }

    let items: Vec<ListItem<'_>> = all_contexts
        .iter()
        .enumerate()
        .map(|(idx, name)| {
            let is_sel = idx == state.context_picker_selected;
            let prefix = if is_sel { "▶ " } else { "  " };
            let style = if is_sel {
                state.theme.selected_style()
            } else {
                ratatui::style::Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::raw(prefix),
                Span::styled(
                    format!("[{name}]"),
                    state.theme.context_badge(name.as_str()),
                ),
            ]))
            .style(style)
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, popup);
}

fn render_search_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState, modal: ModalKind) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", modal.title()));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(popup);

    let input_block = Block::default().borders(Borders::ALL).title(" Query ");
    let query = Paragraph::new(format!("/{}", state.search_query)).block(input_block);
    frame.render_widget(query, chunks[0]);

    let items: Vec<ListItem<'_>> = state
        .search_results
        .iter()
        .enumerate()
        .map(|(idx, res)| {
            let is_sel = idx == state.search_selected;
            let prefix = if is_sel { "▶ " } else { "  " };
            let line = match res {
                SearchResult::Task(task) => Line::from(vec![
                    Span::raw(prefix),
                    Span::styled("task ", state.theme.dim_style()),
                    Span::styled(
                        format!("[{}] ", task.context),
                        state.theme.context_badge(task.context.as_str()),
                    ),
                    Span::raw(task.title.clone()),
                ]),
                SearchResult::Repo(repo) => Line::from(vec![
                    Span::raw(prefix),
                    Span::styled("repo ", state.theme.dim_style()),
                    Span::styled(
                        format!("[{}] ", repo.context),
                        state.theme.context_badge(repo.context.as_str()),
                    ),
                    Span::raw(repo.name.clone()),
                ]),
            };
            let style = if is_sel {
                state.theme.selected_style()
            } else {
                ratatui::style::Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();
    let list = List::new(items).block(block);
    frame.render_widget(list, chunks[1]);
}

fn render_accept_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState, modal: ModalKind) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", modal.title()));

    let Some(m) = &state.accept_modal else {
        frame.render_widget(block, popup);
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(popup);

    let kind_style = if m.field == super::state::AcceptField::Kind {
        state.theme.selected_style()
    } else {
        ratatui::style::Style::default()
    };
    let kind_block = Block::default().borders(Borders::ALL).title(" Kind (←/→) ");
    let kind_p = Paragraph::new(format!("  {}", m.kind.name()))
        .block(kind_block)
        .style(kind_style);
    frame.render_widget(kind_p, chunks[0]);

    let imp_style = if m.field == super::state::AcceptField::Importance {
        state.theme.selected_style()
    } else {
        ratatui::style::Style::default()
    };
    let imp_block = Block::default()
        .borders(Borders::ALL)
        .title(" Importance (←/→) ");
    let imp_p = Paragraph::new(format!("  {}", importance_label(m.importance)))
        .block(imp_block)
        .style(imp_style);
    frame.render_widget(imp_p, chunks[1]);

    let due_style = if m.field == super::state::AcceptField::Due {
        state.theme.selected_style()
    } else {
        ratatui::style::Style::default()
    };
    let due_block = Block::default()
        .borders(Borders::ALL)
        .title(" Due date (YYYY-MM-DD, today, tomorrow, +1d, +1w) ");
    let due_p = Paragraph::new(format!("  {}", m.due_input))
        .block(due_block)
        .style(due_style);
    frame.render_widget(due_p, chunks[2]);

    let hint = if let Some(err) = &m.error {
        Line::styled(
            format!("Error: {err}"),
            ratatui::style::Style::default().fg(ratatui::style::Color::Red),
        )
    } else {
        Line::styled(
            "Tab/Shift+Tab to switch fields · Enter to confirm · Esc to cancel",
            state.theme.dim_style(),
        )
    };
    let hint_p = Paragraph::new(hint).block(Block::default().borders(Borders::TOP));
    frame.render_widget(hint_p, chunks[3]);
}

fn render_snooze_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState, modal: ModalKind) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", modal.title()));

    let Some(m) = &state.snooze_modal else {
        frame.render_widget(block, popup);
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(popup);

    let until_style = if m.field == super::state::SnoozeField::Until {
        state.theme.selected_style()
    } else {
        ratatui::style::Style::default()
    };
    let until_block = Block::default()
        .borders(Borders::ALL)
        .title(" Until (YYYY-MM-DD, today, tomorrow, +1d, +1w) ");
    let until_p = Paragraph::new(format!("  {}", m.until_input))
        .block(until_block)
        .style(until_style);
    frame.render_widget(until_p, chunks[0]);

    let on_style = if m.field == super::state::SnoozeField::On {
        state.theme.selected_style()
    } else {
        ratatui::style::Style::default()
    };
    let on_block = Block::default()
        .borders(Borders::ALL)
        .title(" Waiting on (optional) ");
    let on_p = Paragraph::new(format!("  {}", m.on_input))
        .block(on_block)
        .style(on_style);
    frame.render_widget(on_p, chunks[1]);

    let hint = if let Some(err) = &m.error {
        Line::styled(
            format!("Error: {err}"),
            ratatui::style::Style::default().fg(ratatui::style::Color::Red),
        )
    } else {
        Line::styled(
            "Tab to switch field · Enter to confirm · Esc to cancel",
            state.theme.dim_style(),
        )
    };
    let hint_p = Paragraph::new(hint).block(Block::default().borders(Borders::TOP));
    frame.render_widget(hint_p, chunks[2]);
}

fn render_new_task_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState, modal: ModalKind) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", modal.title()));

    let Some(m) = &state.new_task_modal else {
        frame.render_widget(block, popup);
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(popup);

    let ctx_block = Block::default().borders(Borders::ALL).title(" Context ");
    let ctx_p = Paragraph::new(format!("  [{}]", m.context))
        .block(ctx_block)
        .style(state.theme.context_badge(m.context.as_str()));
    frame.render_widget(ctx_p, chunks[0]);

    let title_block = Block::default().borders(Borders::ALL).title(" Title ");
    let title_p = Paragraph::new(format!("  {}", m.title_input))
        .block(title_block)
        .style(state.theme.selected_style());
    frame.render_widget(title_p, chunks[1]);

    let hint = if let Some(err) = &m.error {
        Line::styled(
            format!("Error: {err}"),
            ratatui::style::Style::default().fg(ratatui::style::Color::Red),
        )
    } else {
        Line::styled(
            "Enter to create task · Esc to cancel",
            state.theme.dim_style(),
        )
    };
    let hint_p = Paragraph::new(hint).block(Block::default().borders(Borders::TOP));
    frame.render_widget(hint_p, chunks[2]);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::super::color::Theme;
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::collections::BTreeMap;

    fn test_theme() -> Theme {
        Theme::from_config(&BTreeMap::new(), false)
    }

    fn buffer_text_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn renders_empty_queue_view_snapshot() {
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = AppState::new(
            test_theme(),
            None,
            Vec::new(),
            jiff::civil::date(2026, 10, 22),
        );

        terminal.draw(|f| render(f, &state)).unwrap();

        let expected = [
            " 1 Today 0 │ 2 Inbox 0 │ 3 Waiting 0   [all]   ○ offline                        ",
            "┌ Today · [all] ───────────────────────────────────────────────────────────────┐",
            "│ No tasks in this queue.                                                      │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "└──────────────────────────────────────────────────────────────────────────────┘",
            " q quit  ? help  : palette  / search  @ context  r refresh                      ",
        ];
        assert_eq!(buffer_text_lines(&terminal), expected);
    }

    #[test]
    fn renders_error_banner_when_last_error_is_set() {
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new(
            test_theme(),
            None,
            Vec::new(),
            jiff::civil::date(2026, 10, 22),
        );
        state.set_error("cannot connect to loomd");

        terminal.draw(|f| render(f, &state)).unwrap();

        let expected = [
            " 1 Today 0 │ 2 Inbox 0 │ 3 Waiting 0   [all]   ○ offline                        ",
            "┌ Today · [all] ───────────────────────────────────────────────────────────────┐",
            "│ No tasks in this queue.                                                      │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "└──────────────────────────────────────────────────────────────────────────────┘",
            " [Error: cannot connect to loomd]                                               ",
        ];
        assert_eq!(buffer_text_lines(&terminal), expected);
    }
}
