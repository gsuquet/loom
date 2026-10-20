//! Pure ratatui rendering of queues, object pages and modals (ADR-0014).

#![allow(clippy::option_if_let_else)]

use loom_api::{ContextName, Importance, SearchResult, TaskDetail};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use super::color::Theme;
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
        let empty = Paragraph::new(Line::styled(
            "  No tasks in this queue.",
            state.theme.dim_style(),
        ))
        .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem<'static>> = tasks
        .iter()
        .enumerate()
        .map(|(idx, task)| {
            let mut spans = Vec::new();
            let is_sel = idx == selected;
            let marker = if is_sel { "▶ " } else { "  " };
            spans.push(Span::raw(marker));

            let (importance_str, importance_style, status_str, status_style, kind_str, title_str) =
                if let Some(t) = &task.task {
                    (
                        importance_label(t.importance),
                        state.theme.importance_style(t.importance),
                        t.status
                            .name()
                            .map_or_else(|| "unknown".to_owned(), |s| s.to_string()),
                        state.theme.status_style(t.status.name()),
                        t.kind.name().to_string(),
                        t.title.clone(),
                    )
                } else {
                    (
                        "p3",
                        state.theme.dim_style(),
                        "invalid".to_string(),
                        state.theme.dim_style(),
                        "unknown".to_string(),
                        "(invalid task file)".to_string(),
                    )
                };

            spans.push(Span::styled(
                format!("{importance_str:<3} "),
                importance_style,
            ));

            spans.push(Span::styled(format!("{status_str:<7} "), status_style));

            spans.push(Span::styled(
                format!("[{}] ", task.context),
                state.theme.context_badge(task.context.as_str()),
            ));

            spans.push(Span::raw(format!("{kind_str:<8} ")));
            spans.push(Span::raw(title_str));

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
        let lines = task_detail_lines(detail, &state.theme);
        let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
        frame.render_widget(paragraph, area);
    } else {
        let empty = Paragraph::new(Line::styled(
            "  Select a task to preview.",
            state.theme.dim_style(),
        ))
        .block(block);
        frame.render_widget(empty, area);
    }
}

fn task_detail_lines(detail: &TaskDetail, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let summary = &detail.summary;
    let (importance_str, importance_style, status_str, status_style, title_str) =
        if let Some(t) = &summary.task {
            (
                importance_label(t.importance),
                theme.importance_style(t.importance),
                t.status
                    .name()
                    .map_or_else(|| "unknown".to_owned(), |s| s.to_string()),
                theme.status_style(t.status.name()),
                t.title.clone(),
            )
        } else {
            (
                "p3",
                theme.dim_style(),
                "invalid".to_string(),
                theme.dim_style(),
                "(invalid task file)".to_string(),
            )
        };

    lines.push(Line::from(vec![
        Span::styled(summary.id.to_string(), theme.header_style()),
        Span::raw("  "),
        Span::styled(
            format!("[{}]", summary.context),
            theme.context_badge(summary.context.as_str()),
        ),
        Span::raw("  "),
        Span::styled(status_str, status_style),
        Span::raw("  "),
        Span::styled(importance_str, importance_style),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::styled(title_str, theme.header_style()));
    lines.push(Line::raw(""));
    if !detail.body.is_empty() {
        lines.push(Line::raw(detail.body.clone()));
    }
    lines
}

#[allow(clippy::cast_possible_truncation)]
fn render_object_page(frame: &mut Frame<'_>, area: Rect, state: &AppState, page: &ObjectPageState) {
    let block = Block::default().borders(Borders::ALL).title(" Task ");
    match &page.content {
        ObjectPageContent::Task(detail) => {
            let lines = task_detail_lines(detail, &state.theme);
            let paragraph = Paragraph::new(lines)
                .block(block)
                .scroll((page.scroll as u16, 0))
                .wrap(Wrap { trim: true });
            frame.render_widget(paragraph, area);
        }
        ObjectPageContent::Loading(ObjectTarget::Task(id)) => {
            let loading = Paragraph::new(Line::styled(
                format!("  Loading task {id}..."),
                state.theme.dim_style(),
            ))
            .block(block);
            frame.render_widget(loading, area);
        }
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    if let Some(err) = &state.last_error {
        frame.render_widget(
            Paragraph::new(Line::styled(
                format!(" [Error: {err}]"),
                state.theme.error_style(),
            )),
            area,
        );
        return;
    }

    if let Some(msg) = &state.status_message {
        frame.render_widget(Paragraph::new(Line::raw(format!(" {msg}"))), area);
        return;
    }

    let hints = [
        ("q", "quit"),
        ("?", "help"),
        (":", "palette"),
        ("/", "search"),
        ("@", "context"),
        ("r", "refresh"),
    ];
    let mut spans = vec![Span::raw(" ")];
    for (idx, (key, desc)) in hints.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(*key, state.theme.header_style()));
        spans.push(Span::styled(format!(" {desc}"), state.theme.dim_style()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
fn render_modal(frame: &mut Frame<'_>, area: Rect, state: &AppState, modal: ModalKind) {
    let popup = centered_rect(60, 60, area);
    frame.render_widget(Clear, popup);

    match modal {
        ModalKind::Help => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Key bindings (?) ");
            let mut lines = Vec::new();
            for layer in [
                Layer::Global,
                Layer::View(ViewKind::Queue),
                Layer::View(ViewKind::ObjectPage),
                Layer::Modal(ModalKind::Palette),
                Layer::Modal(ModalKind::Search),
                Layer::Modal(ModalKind::ContextPicker),
            ] {
                lines.push(Line::styled(
                    format!("─── {layer:?} ───"),
                    state.theme.header_style(),
                ));
                for binding in bindings_for_layer(layer) {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {:<12} ", binding.label),
                            state.theme.header_style(),
                        ),
                        Span::raw(binding.description),
                    ]));
                }
                lines.push(Line::raw(""));
            }
            let paragraph = Paragraph::new(lines)
                .block(block)
                .scroll((state.help_scroll as u16, 0));
            frame.render_widget(paragraph, popup);
        }
        ModalKind::Palette => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Command palette (:) ");
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(1)])
                .split(popup);

            let input = Paragraph::new(state.palette_query.clone())
                .block(Block::default().borders(Borders::ALL).title(" Search "));
            frame.render_widget(input, chunks[0]);

            let items: Vec<ListItem<'static>> = state
                .filtered_palette
                .iter()
                .enumerate()
                .map(|(idx, (_, item))| {
                    let is_sel = idx == state.palette_selected;
                    let marker = if is_sel { "▶ " } else { "  " };
                    let line = Line::from(vec![
                        Span::raw(marker),
                        Span::styled(format!("{:<8} ", item.category), state.theme.dim_style()),
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
        ModalKind::ContextPicker => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Pick context (@) ");
            let mut items = vec![ListItem::new(Line::raw("  all (show all)"))];
            for ctx in &state.contexts {
                items.push(ListItem::new(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("[{ctx}]"), state.theme.context_badge(ctx.as_str())),
                ])));
            }
            let items: Vec<ListItem<'static>> = items
                .into_iter()
                .enumerate()
                .map(|(idx, item)| {
                    if idx == state.context_picker_selected {
                        item.style(state.theme.selected_style())
                    } else {
                        item
                    }
                })
                .collect();
            let list = List::new(items).block(block);
            frame.render_widget(list, popup);
        }
        ModalKind::Search => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Live search (/) ");
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(1)])
                .split(popup);

            let input = Paragraph::new(state.search_query.clone())
                .block(Block::default().borders(Borders::ALL).title(" Query "));
            frame.render_widget(input, chunks[0]);

            let items: Vec<ListItem<'static>> = state
                .search_results
                .iter()
                .enumerate()
                .map(|(idx, res)| {
                    let is_sel = idx == state.search_selected;
                    let marker = if is_sel { "▶ " } else { "  " };
                    let line = match res {
                        SearchResult::Task(task) => Line::from(vec![
                            Span::raw(marker),
                            Span::styled(format!("{:<8} ", task.id), state.theme.header_style()),
                            Span::styled(
                                format!("[{}] ", task.context),
                                state.theme.context_badge(task.context.as_str()),
                            ),
                            Span::raw(task.title.clone()),
                        ]),
                        SearchResult::Repo(repo) => Line::from(vec![
                            Span::raw(marker),
                            Span::styled(format!("{:<8} ", "repo"), state.theme.dim_style()),
                            Span::styled(
                                format!("[{}] ", repo.context),
                                state.theme.context_badge(&repo.context),
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
    }
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
        let state = AppState::new(test_theme(), None, Vec::new());

        terminal.draw(|f| render(f, &state)).unwrap();

        let expected = [
            " 1 Today 0 │ 2 Inbox 0 │ 3 Waiting 0   [all]   ○ offline                        ",
            "┌ Today · [all] ───────────────────────────────────────────────────────────────┐",
            "│  No tasks in this queue.                                                     │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "└──────────────────────────────────────────────────────────────────────────────┘",
            " q quit  ? help  : palette  / search  @ context  r refresh                      ",
        ];
        // Note: TestBackend::assert_buffer_lines checks cell styles and fails on styled spans (e.g. BOLD).
        // buffer_text_lines verifies the full character grid across (width x height) for layout, alignment, and borders.
        assert_eq!(buffer_text_lines(&terminal), expected);
    }

    #[test]
    fn renders_error_banner_when_last_error_is_set() {
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new(test_theme(), None, Vec::new());
        state.set_error("cannot connect to loomd");

        terminal.draw(|f| render(f, &state)).unwrap();

        let expected = [
            " 1 Today 0 │ 2 Inbox 0 │ 3 Waiting 0   [all]   ○ offline                        ",
            "┌ Today · [all] ───────────────────────────────────────────────────────────────┐",
            "│  No tasks in this queue.                                                     │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "└──────────────────────────────────────────────────────────────────────────────┘",
            " [Error: cannot connect to loomd]                                               ",
        ];
        // Note: TestBackend::assert_buffer_lines checks cell styles and fails on styled spans (e.g. BOLD).
        // buffer_text_lines verifies the full character grid across (width x height) for layout, alignment, and borders.
        assert_eq!(buffer_text_lines(&terminal), expected);
    }
}
