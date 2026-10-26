//! Pure ratatui rendering of queues, object pages and modals (ADR-0014).

#![allow(clippy::option_if_let_else)]

use loom_api::{ContextName, Importance, SearchResult, Status, TaskDetail, TaskSummary};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use super::keymap::{Layer, ModalKind, QueueTab, ViewKind, bindings_for_layer};
use super::markdown;
use super::state::{
    AppState, ObjectPageContent, ObjectPageState, ObjectTarget, PREVIEW_MIN_WIDTH, PageTab,
};
use crate::task::{read_only_text, reason_text};

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
        .map(|(idx, summary)| render_queue_item(summary, idx == selected, state))
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn prefix_string(is_sel: bool, depth: usize) -> String {
    let sel = if is_sel { "▶ " } else { "  " };
    if depth == 0 {
        sel.to_string()
    } else {
        let indent = "  ".repeat(depth - 1);
        format!("{sel}{indent}└─ ")
    }
}

fn render_queue_item<'a>(summary: &'a TaskSummary, is_sel: bool, state: &AppState) -> ListItem<'a> {
    let mut spans: Vec<Span<'_>> = Vec::new();

    let depth = if state.active_tab == QueueTab::Today {
        state.task_depth_in_today(summary)
    } else {
        0
    };

    spans.push(Span::raw(prefix_string(is_sel, depth)));

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

        if state.active_tab == QueueTab::Waiting
            && let Status::Waiting { on, until } = &task.status
        {
            if let Some(until) = until {
                spans.push(Span::styled(
                    format!(" (until {until})"),
                    state.theme.dim_style(),
                ));
            } else {
                spans.push(Span::styled(" (undated)", state.theme.dim_style()));
            }
            if let Some(on) = on {
                spans.push(Span::styled(
                    format!(" [on: {on}]"),
                    state.theme.dim_style(),
                ));
            }
        }

        if state.active_tab == QueueTab::Today
            && state.group_by_parent()
            && let Some(parent) = &task.relations.parent
        {
            spans.push(Span::styled(
                format!(" [parent: {parent}]"),
                state.theme.dim_style(),
            ));
        }

        if state.active_tab == QueueTab::Today
            && state.why_enabled()
            && let Some(rank) = &summary.rank
        {
            spans.push(Span::styled(
                format!(" ({})", reason_text(rank.reason)),
                state.theme.dim_style(),
            ));
        }
    } else {
        spans.push(Span::raw(summary.id.to_string()));
    }

    if let Some(ro) = &summary.read_only {
        spans.push(Span::styled(
            format!(" [ro: {}]", read_only_text(ro)),
            state.theme.error_style(),
        ));
    }

    let style = if is_sel {
        state.theme.selected_style()
    } else {
        ratatui::style::Style::default()
    };
    ListItem::new(Line::from(spans)).style(style)
}

fn render_preview_pane(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let block = Block::default().borders(Borders::ALL).title(" Preview ");
    if let Some(detail) = &state.preview_detail {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        render_task_overview(frame, inner, detail, 0);
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
            render_task_page(frame, area, block, detail, page);
        }
        ObjectPageContent::Loading(target) => match target {
            ObjectTarget::Task(id) => {
                let p = Paragraph::new(format!(" Loading task {id}...")).block(block);
                frame.render_widget(p, area);
            }
        },
    }
}

/// The tab bar and the selected tab of a task page.
fn render_task_page(
    frame: &mut Frame<'_>,
    area: Rect,
    block: Block<'_>,
    detail: &TaskDetail,
    page: &ObjectPageState,
) {
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 2 {
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);
    frame.render_widget(Paragraph::new(page_tab_bar(page.tab)), chunks[0]);
    match page.tab {
        PageTab::Overview => render_task_overview(frame, chunks[1], detail, page.scroll),
        PageTab::Relations | PageTab::Events => {
            let text = Paragraph::new(format!(
                " The {} tab has no content in this version.",
                page.tab.name()
            ));
            frame.render_widget(text, chunks[1]);
        }
    }
}

fn page_tab_bar(selected: PageTab) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (idx, tab) in PageTab::ALL.into_iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" │ "));
        }
        let style = if tab == selected {
            ratatui::style::Style::default()
                .add_modifier(ratatui::style::Modifier::REVERSED | ratatui::style::Modifier::BOLD)
        } else {
            ratatui::style::Style::default()
        };
        spans.push(Span::styled(format!(" {} ", tab.name()), style));
    }
    Line::from(spans)
}

fn render_task_overview(frame: &mut Frame<'_>, area: Rect, detail: &TaskDetail, scroll: usize) {
    let mut lines = Vec::new();

    if let Some(ro) = &detail.summary.read_only {
        lines.push(Line::from(vec![Span::styled(
            format!("⚠ READ-ONLY: {}", read_only_text(ro)),
            ratatui::style::Style::default()
                .fg(ratatui::style::Color::Red)
                .bold(),
        )]));
        lines.push(Line::raw(""));
    }

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

        if let Status::Waiting { on, until } = &task.status {
            let mut wait_spans = Vec::new();
            wait_spans.push(Span::styled(
                "Until: ",
                ratatui::style::Style::default().bold(),
            ));
            if let Some(until) = until {
                wait_spans.push(Span::raw(until.to_string()));
                wait_spans.push(Span::raw("   "));
            } else {
                wait_spans.push(Span::raw("undated   "));
            }
            if let Some(on) = on {
                wait_spans.push(Span::styled(
                    "Waiting on: ",
                    ratatui::style::Style::default().bold(),
                ));
                wait_spans.push(Span::raw(on.clone()));
            }
            lines.push(Line::from(wait_spans));
        }

        if let Some(rank) = &detail.summary.rank {
            lines.push(Line::from(vec![
                Span::styled("Rank: ", ratatui::style::Style::default().bold()),
                Span::raw(format!("#{} ({})", rank.position, reason_text(rank.reason))),
            ]));
        }

        if let Some(parent) = &task.relations.parent {
            lines.push(Line::from(vec![
                Span::styled("Parent: ", ratatui::style::Style::default().bold()),
                Span::raw(parent.to_string()),
            ]));
        }

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
        lines.extend(markdown::render(&detail.body, usize::from(area.width)));
    }

    let skipped = lines.into_iter().skip(scroll).collect::<Vec<_>>();
    let paragraph = Paragraph::new(skipped).wrap(Wrap { trim: false });
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
        match state.active_tab {
            QueueTab::Today => vec![
                ("q", "quit"),
                ("?", "help"),
                ("g", "go"),
                ("c", "done"),
                ("z", "snooze"),
                ("+", "new"),
                (":", "palette"),
            ],
            QueueTab::Inbox => vec![
                ("q", "quit"),
                ("?", "help"),
                ("a", "accept"),
                ("z", "snooze"),
                ("x", "cancel"),
                ("c", "done"),
                ("+", "new"),
            ],
            QueueTab::Waiting => vec![
                ("q", "quit"),
                ("?", "help"),
                ("g", "go"),
                ("c", "done"),
                ("x", "cancel"),
                ("+", "new"),
                (":", "palette"),
            ],
        }
    } else {
        vec![
            ("Esc", "back"),
            ("[/]", "tab"),
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
            " q quit  ? help  g go  c done  z snooze  + new  : palette                       ",
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

    fn detail(body: &str, read_only: Option<loom_api::TaskReadOnly>) -> TaskDetail {
        let created = loom_core::Timestamp::EPOCH;
        let id: loom_core::TaskId = "k3q7zm2a".parse().unwrap();
        let context: ContextName = "work".parse().unwrap();
        TaskDetail {
            summary: TaskSummary {
                id: id.clone(),
                prefix: "k3q7".to_owned(),
                context: context.clone(),
                task: Some(loom_core::Task {
                    id,
                    title: "Login timeout".to_owned(),
                    kind: loom_core::Kind::Bug,
                    context,
                    status: Status::Ready,
                    importance: Importance::P1,
                    due: None,
                    pinned: false,
                    links: Vec::new(),
                    relations: loom_core::Relations::default(),
                    created,
                    updated: created,
                }),
                read_only,
                rank: None,
            },
            body: body.to_owned(),
            actions: Vec::new(),
        }
    }

    fn task_page_lines(width: u16, tab: PageTab) -> Vec<String> {
        task_page_lines_with(width, tab, None)
    }

    fn task_page_lines_with(
        width: u16,
        tab: PageTab,
        read_only: Option<loom_api::TaskReadOnly>,
    ) -> Vec<String> {
        let mut state = AppState::new(
            test_theme(),
            None,
            Vec::new(),
            jiff::civil::date(2026, 10, 22),
        );
        let id: loom_core::TaskId = "k3q7zm2a".parse().unwrap();
        state.page_stack.push(ObjectPageState {
            target: ObjectTarget::Task(id),
            content: ObjectPageContent::Task(Box::new(detail(
                "## Symptoms\n- **Mobile** users time out\n- retry `3` times",
                read_only,
            ))),
            scroll: 0,
            tab,
        });
        let mut terminal = Terminal::new(TestBackend::new(width, 16)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        buffer_text_lines(&terminal)
    }

    #[test]
    fn task_page_shows_tabs_fields_and_rendered_body_at_80_columns() {
        let lines = task_page_lines(80, PageTab::Overview);
        let text = lines.join("\n");
        assert!(lines[1].starts_with("┌ Task k3q7zm2a"), "{text}");
        assert!(
            lines[2].contains(" Overview  │  Relations  │  Events "),
            "{text}"
        );
        assert!(text.contains("Title: Login timeout"), "{text}");
        assert!(text.contains("Symptoms"), "{text}");
        assert!(text.contains("• Mobile users time out"), "{text}");
        assert!(text.contains("• retry 3 times"), "{text}");
        assert!(!text.contains("**"), "{text}");
        assert!(lines[15].contains("[/]"), "{text}");
    }

    #[test]
    fn task_page_overview_frame_at_80_columns() {
        let expected = [
            " 1 Today 0 │ 2 Inbox 0 │ 3 Waiting 0   [all]   ○ offline                        ",
            "┌ Task k3q7zm2a ───────────────────────────────────────────────────────────────┐",
            "│  Overview  │  Relations  │  Events                                           │",
            "│ID: k3q7zm2a   Context: [work]                                                │",
            "│Status: ready   Importance: p1   Kind: bug                                    │",
            "│                                                                              │",
            "│Title: Login timeout                                                          │",
            "│                                                                              │",
            "│── Notes ──────────────────────────────────────────                           │",
            "│Symptoms                                                                      │",
            "│• Mobile users time out                                                       │",
            "│• retry 3 times                                                               │",
            "│                                                                              │",
            "│                                                                              │",
            "└──────────────────────────────────────────────────────────────────────────────┘",
            " Esc back  [/] tab  j/k scroll  r refresh  : palette  / search  ? help  q quit  ",
        ];
        assert_eq!(task_page_lines(80, PageTab::Overview), expected);
    }

    #[test]
    fn task_page_overview_frame_at_160_columns() {
        let expected = [
            " 1 Today 0 │ 2 Inbox 0 │ 3 Waiting 0   [all]   ○ offline                                                                                                        ",
            "┌ Task k3q7zm2a ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐",
            "│  Overview  │  Relations  │  Events                                                                                                                           │",
            "│ID: k3q7zm2a   Context: [work]                                                                                                                                │",
            "│Status: ready   Importance: p1   Kind: bug                                                                                                                    │",
            "│                                                                                                                                                              │",
            "│Title: Login timeout                                                                                                                                          │",
            "│                                                                                                                                                              │",
            "│── Notes ──────────────────────────────────────────                                                                                                           │",
            "│Symptoms                                                                                                                                                      │",
            "│• Mobile users time out                                                                                                                                       │",
            "│• retry 3 times                                                                                                                                               │",
            "│                                                                                                                                                              │",
            "│                                                                                                                                                              │",
            "└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘",
            " Esc back  [/] tab  j/k scroll  r refresh  : palette  / search  ? help  q quit                                                                                  ",
        ];
        assert_eq!(task_page_lines(160, PageTab::Overview), expected);
    }

    #[test]
    fn a_read_only_task_page_shows_why() {
        let expected = [
            " 1 Today 0 │ 2 Inbox 0 │ 3 Waiting 0   [all]   ○ offline                        ",
            "┌ Task k3q7zm2a ───────────────────────────────────────────────────────────────┐",
            "│  Overview  │  Relations  │  Events                                           │",
            "│⚠ READ-ONLY: archived in archive/2026                                         │",
            "│                                                                              │",
            "│ID: k3q7zm2a   Context: [work]                                                │",
            "│Status: ready   Importance: p1   Kind: bug                                    │",
            "│                                                                              │",
            "│Title: Login timeout                                                          │",
            "│                                                                              │",
            "│── Notes ──────────────────────────────────────────                           │",
            "│Symptoms                                                                      │",
            "│• Mobile users time out                                                       │",
            "│• retry 3 times                                                               │",
            "└──────────────────────────────────────────────────────────────────────────────┘",
            " Esc back  [/] tab  j/k scroll  r refresh  : palette  / search  ? help  q quit  ",
        ];
        let lines = task_page_lines_with(
            80,
            PageTab::Overview,
            Some(loom_api::TaskReadOnly::Archived(2026)),
        );
        assert_eq!(lines, expected);
    }

    #[test]
    fn other_tabs_say_that_they_have_no_content() {
        let text = task_page_lines(80, PageTab::Relations).join("\n");
        assert!(
            text.contains("The Relations tab has no content in this version."),
            "{text}"
        );
        assert!(!text.contains("Title: Login timeout"), "{text}");
    }
}
