//! Pure ratatui rendering of queues, object pages and modals (ADR-0014).

#![allow(clippy::option_if_let_else)]

use loom_api::{ContextName, Importance, SearchResult, Status, TaskDetail, TaskSummary};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap,
};

use super::color::Theme;
use super::events::{self, EventsLoad, EventsView};
use super::keymap::{Layer, ModalKind, PickField, QueueTab, ViewKind, bindings_for_layer};
use super::markdown;
use super::state::{
    AppState, ObjectPageContent, ObjectPageState, ObjectTarget, PREVIEW_MIN_WIDTH, PageTab,
    PickOption, ToastTone,
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

/// Widest kind column of a queue; a longer kind is cut (docs/design/ui-style.md, 4.2).
const KIND_MAX_WIDTH: usize = 10;

/// Widest status column; only an unknown status can be longer than `cancelled`.
const STATUS_MAX_WIDTH: usize = 10;

/// Label of the notes section of a task.
const NOTES_LABEL: &str = "Notes";

const fn status_label(status: &Status) -> &str {
    match status {
        Status::Inbox => "inbox",
        Status::Ready => "ready",
        Status::Active => "active",
        Status::Waiting { .. } => "waiting",
        Status::Done => "done",
        Status::Cancelled => "cancelled",
        Status::Unknown(name) => name.as_str(),
    }
}

/// `text` padded to `width` cells, or cut with `…` when it is longer.
fn fit(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len <= width {
        format!("{text:<width$}")
    } else if width == 0 {
        String::new()
    } else {
        let kept: String = text.chars().take(width - 1).collect();
        format!("{kept}…")
    }
}

/// A pane: border, ` title `, one cell of padding left and right.
fn pane(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .padding(Padding::horizontal(1))
}

/// Key hints: key in strong text, action muted, two spaces between hints.
fn hint_line<K: AsRef<str>, D: AsRef<str>>(hints: &[(K, D)], theme: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (idx, (key, desc)) in hints.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(key.as_ref().to_owned(), theme.strong_style()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(desc.as_ref().to_owned(), theme.dim_style()));
    }
    Line::from(spans)
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
    let block = pane(title);

    let selected = state.current_selected();
    let tasks = match state.active_tab {
        QueueTab::Today => &state.today,
        QueueTab::Inbox => &state.inbox,
        QueueTab::Waiting => &state.waiting,
    };

    if tasks.is_empty() {
        let empty = Paragraph::new(Line::styled(
            "No tasks in this queue.",
            state.theme.dim_style(),
        ))
        .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let columns = Columns::of(tasks);
    let items: Vec<ListItem<'_>> = tasks
        .iter()
        .enumerate()
        .map(|(idx, summary)| render_queue_item(summary, idx == selected, &columns, state))
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

/// Widths of the badge columns of a queue, from its rows (docs/design/ui-style.md, 4.2).
struct Columns {
    status: usize,
    context: usize,
    kind: usize,
}

impl Columns {
    fn of(tasks: &[TaskSummary]) -> Self {
        let mut columns = Self {
            status: 0,
            context: 0,
            kind: 0,
        };
        for summary in tasks {
            columns.context = columns
                .context
                .max(summary.context.as_str().chars().count() + 2);
            if let Some(task) = &summary.task {
                columns.status = columns
                    .status
                    .max(status_label(&task.status).chars().count());
                columns.kind = columns.kind.max(task.kind.name().chars().count());
            }
        }
        columns.status = columns.status.min(STATUS_MAX_WIDTH);
        columns.kind = columns.kind.min(KIND_MAX_WIDTH);
        columns
    }
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

fn render_queue_item<'a>(
    summary: &'a TaskSummary,
    is_sel: bool,
    columns: &Columns,
    state: &AppState,
) -> ListItem<'a> {
    let theme = &state.theme;
    let mut spans: Vec<Span<'_>> = Vec::new();

    let depth = if state.active_tab == QueueTab::Today {
        state.task_depth_in_today(summary)
    } else {
        0
    };

    // The selection is the marker and the title; badges keep their colors.
    let selected_style = theme.selected_style().patch(theme.strong_style());
    let marker_style = if is_sel {
        selected_style
    } else {
        Style::default()
    };
    let marker = prefix_string(is_sel, depth);
    match marker.split_once(' ') {
        Some((head, tail)) if is_sel => {
            spans.push(Span::styled(head.to_owned(), marker_style));
            spans.push(Span::raw(format!(" {tail}")));
        }
        _ => spans.push(Span::raw(marker)),
    }

    if let Some(task) = &summary.task {
        let imp = task.importance;
        spans.push(Span::styled(
            importance_label(imp),
            theme.importance_style(imp),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            fit(status_label(&task.status), columns.status),
            theme.status_style(task.status.name()),
        ));
    } else {
        spans.push(Span::raw(" ".repeat(2 + 1 + columns.status)));
    }
    spans.push(Span::raw(" "));

    spans.push(Span::styled(
        fit(&format!("[{}]", summary.context), columns.context),
        theme.context_badge(summary.context.as_str()),
    ));
    spans.push(Span::raw(" "));

    let title_style = if is_sel {
        selected_style
    } else {
        Style::default()
    };

    if let Some(task) = &summary.task {
        if columns.kind > 0 {
            spans.push(Span::styled(
                fit(task.kind.name(), columns.kind),
                theme.dim_style(),
            ));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(task.title.clone(), title_style));

        if state.active_tab == QueueTab::Waiting
            && let Status::Waiting { on, until } = &task.status
        {
            if let Some(until) = until {
                spans.push(Span::styled(format!(" (until {until})"), theme.dim_style()));
            } else {
                spans.push(Span::styled(" (undated)", theme.dim_style()));
            }
            if let Some(on) = on {
                spans.push(Span::styled(format!(" [on: {on}]"), theme.dim_style()));
            }
        }

        if state.active_tab == QueueTab::Today
            && state.group_by_parent()
            && let Some(parent) = &task.relations.parent
        {
            spans.push(Span::styled(
                format!(" [parent: {parent}]"),
                theme.dim_style(),
            ));
        }

        if state.active_tab == QueueTab::Today
            && state.why_enabled()
            && let Some(rank) = &summary.rank
        {
            spans.push(Span::styled(
                format!(" ({})", reason_text(rank.reason)),
                theme.dim_style(),
            ));
        }
    } else {
        if columns.kind > 0 {
            spans.push(Span::raw(" ".repeat(columns.kind + 1)));
        }
        spans.push(Span::styled(summary.id.to_string(), title_style));
    }

    if let Some(ro) = &summary.read_only {
        spans.push(Span::styled(
            format!(" [ro: {}]", read_only_text(ro)),
            theme.error_style(),
        ));
    }

    ListItem::new(Line::from(spans))
}

fn render_preview_pane(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let block = pane(" Preview ");
    if let Some(detail) = &state.preview_detail {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        render_task_overview(frame, inner, detail, 0, &state.theme);
    } else if let Some(summary) = state.selected_task() {
        let loading = Paragraph::new(Line::styled(
            format!("Loading {}...", summary.id),
            state.theme.dim_style(),
        ))
        .block(block);
        frame.render_widget(loading, area);
    } else {
        let empty =
            Paragraph::new(Line::styled("No item selected.", state.theme.dim_style())).block(block);
        frame.render_widget(empty, area);
    }
}

fn render_object_page(frame: &mut Frame<'_>, area: Rect, state: &AppState, page: &ObjectPageState) {
    let title = match &page.target {
        ObjectTarget::Task(id) => format!(" Task {id} "),
    };
    let block = pane(title);

    match &page.content {
        ObjectPageContent::Task(detail) => {
            render_task_page(frame, area, block, detail, page, &state.theme);
        }
        ObjectPageContent::Loading(target) => match target {
            ObjectTarget::Task(id) => {
                let p = Paragraph::new(Line::styled(
                    format!("Loading task {id}..."),
                    state.theme.dim_style(),
                ))
                .block(block);
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
    theme: &Theme,
) {
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 3 {
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);
    frame.render_widget(Paragraph::new(page_tab_bar(page.tab)), chunks[0]);
    match page.tab {
        PageTab::Overview => render_task_overview(frame, chunks[2], detail, page.scroll, theme),
        PageTab::Events => render_events(frame, chunks[2], &page.events, page.scroll, theme),
        PageTab::Relations => {
            let text = Paragraph::new(Line::styled(
                "The Relations tab has no content in this version.",
                theme.dim_style(),
            ));
            frame.render_widget(text, chunks[2]);
        }
    }
}

/// `text` cut to `width` characters, with `…` where it was cut.
fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// The lines of the Events tab: one per event, oldest first.
fn events_lines(view: &EventsView, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let log = match &view.load {
        EventsLoad::NotLoaded | EventsLoad::Loading => {
            return vec![Line::styled("Loading events...", theme.dim_style())];
        }
        EventsLoad::Failed(error) => {
            return vec![Line::styled(
                format!("Cannot load the events: {error}"),
                theme.error_style(),
            )];
        }
        EventsLoad::Loaded(log) => log,
    };
    let rows = events::rows(
        log,
        view.type_filter.as_deref(),
        view.actor_filter.as_deref(),
    );
    if rows.is_empty() {
        let text = if log.events.is_empty() {
            "No events."
        } else {
            "No event matches the filters."
        };
        return vec![Line::styled(text, theme.dim_style())];
    }
    let actor_width = rows
        .iter()
        .map(|r| r.actor.chars().count())
        .max()
        .unwrap_or(0);
    let kind_width = rows.iter().map(|r| r.kind.len()).max().unwrap_or(0);
    // time, two gaps of two columns, then the summary gets what is left.
    let summary_width = width.saturating_sub(16 + actor_width + kind_width + 6);
    rows.iter()
        .map(|row| {
            Line::from(vec![
                Span::styled(row.time.clone(), theme.dim_style()),
                Span::raw("  "),
                Span::raw(format!("{:<actor_width$}", row.actor)),
                Span::raw("  "),
                Span::styled(format!("{:<kind_width$}", row.kind), theme.strong_style()),
                Span::raw("  "),
                Span::raw(clip(&row.summary, summary_width)),
            ])
        })
        .collect()
}

fn render_events(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &EventsView,
    scroll: usize,
    theme: &Theme,
) {
    let lines = events_lines(view, usize::from(area.width), theme);
    let shown = lines.into_iter().skip(scroll).collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(shown), area);
}

fn page_tab_bar(selected: PageTab) -> Line<'static> {
    let mut spans = Vec::new();
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

/// A labeled rule that opens a section: `── Notes ─────` (docs/design/ui-style.md, 4.5).
fn section_header(label: &str, width: usize, theme: &Theme) -> Line<'static> {
    let used = 3 + label.chars().count() + 1;
    Line::from(vec![
        Span::styled("── ", theme.dim_style()),
        Span::styled(label.to_owned(), theme.strong_style()),
        Span::styled(
            format!(" {}", "─".repeat(width.saturating_sub(used))),
            theme.dim_style(),
        ),
    ])
}

/// The field grid of a task: banner, title, fields, notes (docs/design/ui-style.md, 4.4).
fn task_overview_lines(detail: &TaskDetail, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if let Some(ro) = &detail.summary.read_only {
        lines.push(Line::styled(
            format!("⚠ READ-ONLY: {}", read_only_text(ro)),
            theme.error_style(),
        ));
        lines.push(Line::raw(""));
    }

    let mut fields: Vec<(&str, Vec<Span<'static>>)> = Vec::new();
    if let Some(task) = &detail.summary.task {
        lines.push(Line::styled(task.title.clone(), theme.strong_style()));
        lines.push(Line::raw(""));

        fields.push((
            "Status",
            vec![Span::styled(
                status_label(&task.status).to_owned(),
                theme.status_style(task.status.name()),
            )],
        ));
        fields.push((
            "Importance",
            vec![Span::styled(
                importance_label(task.importance),
                theme.importance_style(task.importance),
            )],
        ));
        fields.push(("Kind", vec![Span::raw(task.kind.name().to_owned())]));
    }
    fields.push((
        "Context",
        vec![Span::styled(
            format!("[{}]", detail.summary.context),
            theme.context_badge(detail.summary.context.as_str()),
        )],
    ));
    if let Some(task) = &detail.summary.task {
        if let Some(due) = &task.due {
            fields.push(("Due", vec![Span::raw(due.to_string())]));
        }
        if let Status::Waiting { on, until } = &task.status {
            let mut value = until
                .as_ref()
                .map_or_else(|| "undated".to_owned(), |until| format!("until {until}"));
            if let Some(on) = on {
                value.push_str(" · on ");
                value.push_str(on);
            }
            fields.push(("Waiting", vec![Span::raw(value)]));
        }
        if let Some(rank) = &detail.summary.rank {
            fields.push((
                "Rank",
                vec![Span::raw(format!(
                    "#{} ({})",
                    rank.position,
                    reason_text(rank.reason)
                ))],
            ));
        }
        if let Some(parent) = &task.relations.parent {
            fields.push(("Parent", vec![Span::raw(parent.to_string())]));
        }
    }
    fields.push(("ID", vec![Span::raw(detail.summary.id.to_string())]));

    let label_width = fields
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    for (label, value) in fields {
        let mut spans = vec![Span::styled(
            format!("{label:>label_width$} "),
            theme.dim_style(),
        )];
        spans.extend(value);
        lines.push(Line::from(spans));
    }

    lines.push(Line::raw(""));
    lines.push(section_header(NOTES_LABEL, width, theme));
    let body = markdown::without_leading_heading(&detail.body, NOTES_LABEL);
    if body.trim().is_empty() {
        lines.push(Line::styled("No notes.", theme.dim_style()));
    } else {
        lines.extend(markdown::render(body, width));
    }
    lines
}

fn render_task_overview(
    frame: &mut Frame<'_>,
    area: Rect,
    detail: &TaskDetail,
    scroll: usize,
    theme: &Theme,
) {
    let lines = task_overview_lines(detail, usize::from(area.width), theme);
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
        let mut spans = vec![Span::raw(" ")];
        if state.status_tone == ToastTone::Success {
            spans.push(Span::styled("✓ ", state.theme.success_style()));
        }
        spans.push(Span::raw(msg.clone()));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
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
            ("t/i/d/s", "edit"),
            ("u", "undo"),
            ("j/k", "scroll"),
            (":", "palette"),
            ("?", "help"),
            ("q", "quit"),
        ]
    };

    let mut spans = vec![Span::raw(" ")];
    match state.page_stack.last() {
        Some(page) if page.tab == PageTab::Events => {
            spans.extend(hint_line(&events_hints(&page.events), &state.theme).spans);
        }
        Some(_) | None => spans.extend(hint_line(&hints, &state.theme).spans),
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The footer of the Events tab: the two filters with their values.
fn events_hints(view: &EventsView) -> Vec<(String, String)> {
    let value = |filter: Option<&String>| filter.map_or("all", String::as_str).to_owned();
    vec![
        ("Esc".to_owned(), "back".to_owned()),
        ("[/]".to_owned(), "tab".to_owned()),
        (
            "f".to_owned(),
            format!("type: {}", value(view.type_filter.as_ref())),
        ),
        (
            "a".to_owned(),
            format!("actor: {}", value(view.actor_filter.as_ref())),
        ),
        ("j/k".to_owned(), "scroll".to_owned()),
        ("?".to_owned(), "help".to_owned()),
        ("q".to_owned(), "quit".to_owned()),
    ]
}

fn render_modal(frame: &mut Frame<'_>, area: Rect, state: &AppState, modal: ModalKind) {
    // Forms take the height they need; lists take most of the screen.
    let popup = match modal {
        ModalKind::NewTask => centered_form(area, 8),
        ModalKind::Snooze => centered_form(area, 10),
        ModalKind::Accept => centered_form(area, 13),
        ModalKind::Pick => {
            let rows = state.pick_modal.as_ref().map_or(3, |m| {
                if m.field == PickField::Due {
                    3
                } else {
                    u16::try_from(m.options.len()).unwrap_or(u16::MAX)
                }
            });
            centered_form(area, rows.saturating_add(4))
        }
        ModalKind::Help | ModalKind::Palette | ModalKind::ContextPicker | ModalKind::Search => {
            centered_rect(70, 70, area)
        }
    };
    frame.render_widget(Clear, popup);

    // Every modal has a border and a title (docs/design/ui-style.md, 4.6).
    let title = match &state.pick_modal {
        Some(m) if modal == ModalKind::Pick => format!(" Change {} ", m.field.name()),
        Some(_) | None => format!(" {} ", modal.title()),
    };
    let block = pane(Line::styled(title, state.theme.strong_style()));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    match modal {
        ModalKind::Help => render_help_modal(frame, inner, state),
        ModalKind::Palette => render_palette_modal(frame, inner, state),
        ModalKind::ContextPicker => render_context_picker_modal(frame, inner, state),
        ModalKind::Search => render_search_modal(frame, inner, state),
        ModalKind::Accept => render_accept_modal(frame, inner, state),
        ModalKind::Snooze => render_snooze_modal(frame, inner, state),
        ModalKind::NewTask => render_new_task_modal(frame, inner, state),
        ModalKind::Pick => render_pick_modal(frame, inner, state),
    }
}

/// The box of a form field: thick and strong when focused, plain and muted otherwise.
fn field_block(label: &str, focused: bool, theme: &Theme) -> Block<'static> {
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1));
    if focused {
        block
            .border_type(BorderType::Thick)
            .title(Line::styled(format!(" {label} "), theme.strong_style()))
    } else {
        block
            .border_style(theme.dim_style())
            .title(Line::styled(format!(" {label} "), theme.dim_style()))
    }
}

/// A text field: its value, a cursor when focused, a muted placeholder when empty.
fn text_field(
    label: &str,
    value: &str,
    placeholder: &str,
    focused: bool,
    theme: &Theme,
) -> Paragraph<'static> {
    let mut spans = Vec::new();
    if value.is_empty() && !placeholder.is_empty() {
        if focused {
            spans.push(Span::styled("▏", theme.strong_style()));
        }
        // Italic too, so that a placeholder differs from a value without color.
        spans.push(Span::styled(
            placeholder.to_owned(),
            theme
                .dim_style()
                .add_modifier(ratatui::style::Modifier::ITALIC),
        ));
    } else {
        spans.push(Span::raw(value.to_owned()));
        if focused {
            spans.push(Span::styled("▏", theme.strong_style()));
        }
    }
    Paragraph::new(Line::from(spans)).block(field_block(label, focused, theme))
}

/// An option field: `‹ value ›` when focused, the value alone otherwise.
fn option_field(
    label: &str,
    value: Span<'static>,
    focused: bool,
    theme: &Theme,
) -> Paragraph<'static> {
    let line = if focused {
        Line::from(vec![
            Span::styled("‹ ", theme.dim_style()),
            value,
            Span::styled(" ›", theme.dim_style()),
        ])
    } else {
        Line::from(value)
    };
    Paragraph::new(line).block(field_block(label, focused, theme))
}

/// The last line of a form: its error, or its key hints.
fn form_footer(
    error: Option<&String>,
    hints: &[(&str, &str)],
    theme: &Theme,
) -> Paragraph<'static> {
    let line = match error {
        Some(err) => Line::styled(format!("Error: {err}"), theme.error_style()),
        None => hint_line(hints, theme),
    };
    Paragraph::new(line)
}

/// A list modal: a field on top of a list.
fn query_and_list(area: Rect) -> (Rect, Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    (chunks[0], chunks[1])
}

fn render_help_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState) {
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
    frame.render_widget(Paragraph::new(scrolled), popup);
}

fn render_palette_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState) {
    let (query_area, list_area) = query_and_list(popup);
    let query = text_field("Command", &state.palette_query, "", true, &state.theme);
    frame.render_widget(query, query_area);

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
    frame.render_widget(List::new(items), list_area);
}

fn render_context_picker_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState) {
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

    frame.render_widget(List::new(items), popup);
}

fn render_search_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState) {
    let (query_area, list_area) = query_and_list(popup);
    let query = text_field("Query", &state.search_query, "", true, &state.theme);
    frame.render_widget(query, query_area);

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
    frame.render_widget(List::new(items), list_area);
}

/// Splits a form into `fields` boxes of 3 lines, a blank line and the footer line.
fn form_rows(area: Rect, fields: usize) -> Vec<Rect> {
    let mut constraints = vec![Constraint::Length(3); fields];
    constraints.push(Constraint::Length(1));
    constraints.push(Constraint::Length(1));
    constraints.push(Constraint::Min(0));
    Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area)
        .to_vec()
}

fn render_accept_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState) {
    let Some(m) = &state.accept_modal else {
        return;
    };
    let theme = &state.theme;
    let rows = form_rows(popup, 3);
    let field = m.field;

    frame.render_widget(
        option_field(
            "Kind",
            Span::raw(m.kind.name().to_owned()),
            field == super::state::AcceptField::Kind,
            theme,
        ),
        rows[0],
    );
    frame.render_widget(
        option_field(
            "Importance",
            Span::styled(
                importance_label(m.importance),
                theme.importance_style(m.importance),
            ),
            field == super::state::AcceptField::Importance,
            theme,
        ),
        rows[1],
    );
    frame.render_widget(
        text_field(
            "Due",
            &m.due_input,
            "YYYY-MM-DD, today, tomorrow, +1d, +1w",
            field == super::state::AcceptField::Due,
            theme,
        ),
        rows[2],
    );
    frame.render_widget(
        form_footer(
            m.error.as_ref(),
            &[
                ("Tab", "next field"),
                ("←/→", "change"),
                ("Enter", "accept"),
                ("Esc", "cancel"),
            ],
            theme,
        ),
        rows[4],
    );
}

fn render_snooze_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState) {
    let Some(m) = &state.snooze_modal else {
        return;
    };
    let theme = &state.theme;
    let rows = form_rows(popup, 2);

    frame.render_widget(
        text_field(
            "Until",
            &m.until_input,
            "YYYY-MM-DD, today, tomorrow, +1d, +1w",
            m.field == super::state::SnoozeField::Until,
            theme,
        ),
        rows[0],
    );
    frame.render_widget(
        text_field(
            "Waiting on",
            &m.on_input,
            "optional",
            m.field == super::state::SnoozeField::On,
            theme,
        ),
        rows[1],
    );
    frame.render_widget(
        form_footer(
            m.error.as_ref(),
            &[
                ("Tab", "next field"),
                ("Enter", "snooze"),
                ("Esc", "cancel"),
            ],
            theme,
        ),
        rows[3],
    );
}

fn render_pick_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState) {
    let Some(m) = &state.pick_modal else {
        return;
    };
    let theme = &state.theme;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(popup);

    let footer = if m.field == PickField::Due {
        form_footer(
            m.error.as_ref(),
            &[("Enter", "apply"), ("Esc", "cancel")],
            theme,
        )
    } else {
        form_footer(
            m.error.as_ref(),
            &[("j/k", "choose"), ("Enter", "apply"), ("Esc", "cancel")],
            theme,
        )
    };
    frame.render_widget(footer, chunks[2]);

    if m.field == PickField::Due {
        let field = text_field(
            "Due",
            &m.due_input,
            "YYYY-MM-DD, today, tomorrow, +1d, +1w; empty removes",
            true,
            theme,
        );
        let area = Rect {
            height: chunks[0].height.min(3),
            ..chunks[0]
        };
        frame.render_widget(field, area);
        return;
    }
    let items: Vec<ListItem<'_>> = m
        .options
        .iter()
        .enumerate()
        .map(|(idx, option)| {
            let label = match option {
                PickOption::Kind(kind) => Span::raw(kind.name().to_owned()),
                PickOption::Importance(importance) => Span::styled(
                    importance_label(*importance),
                    theme.importance_style(*importance),
                ),
                PickOption::Status(status) => {
                    Span::styled(status.to_string(), theme.status_style(Some(*status)))
                }
            };
            let is_sel = idx == m.selected;
            let marker = if is_sel {
                Span::styled("▶", theme.selected_style().patch(theme.strong_style()))
            } else {
                Span::raw(" ")
            };
            let mut spans = vec![marker, Span::raw(" "), label];
            if m.current.as_ref() == Some(option) {
                spans.push(Span::styled(" (current)", theme.dim_style()));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    frame.render_widget(List::new(items), chunks[0]);
}

fn render_new_task_modal(frame: &mut Frame<'_>, popup: Rect, state: &AppState) {
    let Some(m) = &state.new_task_modal else {
        return;
    };
    let theme = &state.theme;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(popup);

    // The context is fixed when the modal opens: a badge, not an input.
    let context = Line::from(vec![
        Span::styled("Context ", theme.dim_style()),
        Span::styled(
            format!("[{}]", m.context),
            theme.context_badge(m.context.as_str()),
        ),
    ]);
    frame.render_widget(Paragraph::new(context), chunks[0]);
    frame.render_widget(
        text_field("Title", &m.title_input, "What needs doing?", true, theme),
        chunks[1],
    );
    frame.render_widget(
        form_footer(
            m.error.as_ref(),
            &[("Enter", "create"), ("Esc", "cancel")],
            theme,
        ),
        chunks[3],
    );
}

/// A centered form `height` lines high, at most 72 columns wide.
fn centered_form(area: Rect, height: u16) -> Rect {
    let width = area.width.saturating_sub(4).min(72);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
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
            events: EventsView::default(),
        });
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
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
        assert!(lines[4].contains("Login timeout"), "{text}");
        assert!(text.contains("Symptoms"), "{text}");
        assert!(text.contains("• Mobile users time out"), "{text}");
        assert!(text.contains("• retry 3 times"), "{text}");
        assert!(!text.contains("**"), "{text}");
        assert!(lines[19].contains("[/]"), "{text}");
    }

    fn events_page_lines(width: u16, view: EventsView) -> Vec<String> {
        let mut state = AppState::new(
            test_theme(),
            None,
            Vec::new(),
            jiff::civil::date(2026, 10, 22),
        );
        let id: loom_core::TaskId = "k3q7zm2a".parse().unwrap();
        state.page_stack.push(ObjectPageState {
            target: ObjectTarget::Task(id),
            content: ObjectPageContent::Task(Box::new(detail("", None))),
            scroll: 0,
            tab: PageTab::Events,
            events: view,
        });
        let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        buffer_text_lines(&terminal)
    }

    fn loaded_events() -> EventsView {
        use loom_api::{Actor, Change, Event, EventRecord, MachineId, Timestamp};
        let record = |actor, event, secs| {
            let origin = loom_core::Origin {
                actor,
                machine_id: MachineId("desk".to_owned()),
            };
            EventRecord::new(
                "aaaaaaaa".parse().unwrap(),
                event,
                Timestamp::from_unix_secs(secs).unwrap(),
                &origin,
            )
        };
        let log = loom_api::TaskEvents {
            id: "k3q7zm2a".parse().unwrap(),
            events: vec![
                record(
                    Actor::User,
                    Event::NoteLogged {
                        text: "called the client about the login timeout and the retry policy"
                            .to_owned(),
                    },
                    1_791_792_000,
                ),
                record(
                    Actor::Daemon,
                    Event::StatusChanged(Change {
                        from: Status::Inbox,
                        to: Status::Ready,
                    }),
                    1_791_792_060,
                ),
            ],
            skipped: Vec::new(),
        };
        EventsView {
            load: crate::tui::events::EventsLoad::Loaded(Box::new(log)),
            ..EventsView::default()
        }
    }

    #[test]
    fn events_tab_lists_the_log_at_80_columns() {
        let lines = events_page_lines(80, loaded_events());
        let text = lines.join("\n");
        assert!(lines[4].contains("user"), "{text}");
        assert!(lines[4].contains("note_logged"), "{text}");
        assert!(lines[4].contains("called the client"), "{text}");
        assert!(lines[5].contains("loomd"), "{text}");
        assert!(lines[5].contains("status: inbox -> ready"), "{text}");
        // The long note is cut at the edge of the pane, never wrapped.
        assert!(lines[4].contains('…'), "{text}");
        assert!(lines.iter().all(|l| l.chars().count() <= 80), "{text}");
        assert!(lines[11].contains("f type: all"), "{text}");
        assert!(lines[11].contains("a actor: all"), "{text}");
    }

    #[test]
    fn events_tab_shows_the_whole_note_at_160_columns() {
        let lines = events_page_lines(160, loaded_events());
        let text = lines.join("\n");
        assert!(lines[4].contains("retry policy"), "{text}");
        assert!(!lines[4].contains('…'), "{text}");
    }

    #[test]
    fn events_tab_shows_the_filters_and_what_they_leave() {
        let mut view = loaded_events();
        view.type_filter = Some("status_changed".to_owned());
        let lines = events_page_lines(80, view.clone());
        let text = lines.join("\n");
        assert!(lines[4].contains("loomd"), "{text}");
        assert!(!text.contains("note_logged"), "{text}");
        assert!(lines[11].contains("f type: status_changed"), "{text}");

        view.actor_filter = Some("user".to_owned());
        let text = events_page_lines(80, view).join("\n");
        assert!(text.contains("No event matches the filters."), "{text}");
    }

    #[test]
    fn events_tab_tells_loading_and_failure() {
        let loading = events_page_lines(
            80,
            EventsView {
                load: crate::tui::events::EventsLoad::Loading,
                ..EventsView::default()
            },
        )
        .join("\n");
        assert!(loading.contains("Loading events..."), "{loading}");
        let failed = events_page_lines(
            80,
            EventsView {
                load: crate::tui::events::EventsLoad::Failed("boom".to_owned()),
                ..EventsView::default()
            },
        )
        .join("\n");
        assert!(failed.contains("Cannot load the events: boom"), "{failed}");
    }

    #[test]
    fn task_page_overview_frame_at_80_columns() {
        let expected = [
            " 1 Today 0 │ 2 Inbox 0 │ 3 Waiting 0   [all]   ○ offline                        ",
            "┌ Task k3q7zm2a ───────────────────────────────────────────────────────────────┐",
            "│  Overview  │  Relations  │  Events                                           │",
            "│                                                                              │",
            "│ Login timeout                                                                │",
            "│                                                                              │",
            "│     Status ready                                                             │",
            "│ Importance p1                                                                │",
            "│       Kind bug                                                               │",
            "│    Context [work]                                                            │",
            "│         ID k3q7zm2a                                                          │",
            "│                                                                              │",
            "│ ── Notes ─────────────────────────────────────────────────────────────────── │",
            "│ Symptoms                                                                     │",
            "│ • Mobile users time out                                                      │",
            "│ • retry 3 times                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "└──────────────────────────────────────────────────────────────────────────────┘",
            " Esc back  [/] tab  t/i/d/s edit  u undo  j/k scroll  : palette  ? help  q quit ",
        ];
        assert_eq!(task_page_lines(80, PageTab::Overview), expected);
    }

    #[test]
    fn task_page_overview_frame_at_160_columns() {
        let expected = [
            " 1 Today 0 │ 2 Inbox 0 │ 3 Waiting 0   [all]   ○ offline                                                                                                        ",
            "┌ Task k3q7zm2a ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐",
            "│  Overview  │  Relations  │  Events                                                                                                                           │",
            "│                                                                                                                                                              │",
            "│ Login timeout                                                                                                                                                │",
            "│                                                                                                                                                              │",
            "│     Status ready                                                                                                                                             │",
            "│ Importance p1                                                                                                                                                │",
            "│       Kind bug                                                                                                                                               │",
            "│    Context [work]                                                                                                                                            │",
            "│         ID k3q7zm2a                                                                                                                                          │",
            "│                                                                                                                                                              │",
            "│ ── Notes ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────── │",
            "│ Symptoms                                                                                                                                                     │",
            "│ • Mobile users time out                                                                                                                                      │",
            "│ • retry 3 times                                                                                                                                              │",
            "│                                                                                                                                                              │",
            "│                                                                                                                                                              │",
            "└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘",
            " Esc back  [/] tab  t/i/d/s edit  u undo  j/k scroll  : palette  ? help  q quit                                                                                 ",
        ];
        assert_eq!(task_page_lines(160, PageTab::Overview), expected);
    }

    #[test]
    fn a_read_only_task_page_shows_why() {
        let expected = [
            " 1 Today 0 │ 2 Inbox 0 │ 3 Waiting 0   [all]   ○ offline                        ",
            "┌ Task k3q7zm2a ───────────────────────────────────────────────────────────────┐",
            "│  Overview  │  Relations  │  Events                                           │",
            "│                                                                              │",
            "│ ⚠ READ-ONLY: archived in archive/2026                                        │",
            "│                                                                              │",
            "│ Login timeout                                                                │",
            "│                                                                              │",
            "│     Status ready                                                             │",
            "│ Importance p1                                                                │",
            "│       Kind bug                                                               │",
            "│    Context [work]                                                            │",
            "│         ID k3q7zm2a                                                          │",
            "│                                                                              │",
            "│ ── Notes ─────────────────────────────────────────────────────────────────── │",
            "│ Symptoms                                                                     │",
            "│ • Mobile users time out                                                      │",
            "│ • retry 3 times                                                              │",
            "└──────────────────────────────────────────────────────────────────────────────┘",
            " Esc back  [/] tab  t/i/d/s edit  u undo  j/k scroll  : palette  ? help  q quit ",
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
        assert!(!text.contains("Login timeout"), "{text}");
    }
    fn summary(id: &str, title: &str, kind: loom_core::Kind, context: &str) -> TaskSummary {
        let mut d = detail("", None);
        let id: loom_core::TaskId = id.parse().unwrap();
        let context: ContextName = context.parse().unwrap();
        d.summary.id = id.clone();
        d.summary.context = context.clone();
        if let Some(task) = &mut d.summary.task {
            task.id = id;
            task.title = title.to_owned();
            task.kind = kind;
            task.context = context;
            task.status = Status::Inbox;
        }
        d.summary
    }

    fn inbox_state() -> AppState {
        let mut state = AppState::new(
            test_theme(),
            None,
            Vec::new(),
            jiff::civil::date(2026, 10, 22),
        );
        state.active_tab = QueueTab::Inbox;
        state.inbox = vec![
            summary(
                "df0rpyx9",
                "KCNA Certification",
                loom_core::Kind::Chore,
                "personal",
            ),
            summary(
                "k3q7zm2a",
                "Login timeout",
                loom_core::Kind::Investigation,
                "work",
            ),
        ];
        state
    }

    fn draw(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, state)).unwrap();
        buffer_text_lines(&terminal)
    }

    #[test]
    fn queue_rows_have_aligned_badge_columns() {
        let lines = draw(&inbox_state(), 80, 6);
        assert_eq!(
            lines[2],
            "│ ▶ p1 inbox [personal] chore      KCNA Certification                          │"
        );
        assert_eq!(
            lines[3],
            "│   p1 inbox [work]     investiga… Login timeout                               │"
        );
    }

    #[test]
    fn the_selection_does_not_color_the_badges() {
        let state = inbox_state();
        let mut terminal = Terminal::new(TestBackend::new(80, 5)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        let buffer = terminal.backend().buffer();
        let reversed = |x: u16| {
            buffer[(x, 2)]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        };
        // `▶` and the title are reversed; `p1`, `inbox`, `[personal]`, `chore` are not.
        assert!(reversed(2));
        assert!(!(4..34).any(reversed));
        assert!(reversed(35));
    }

    #[test]
    fn the_new_task_modal_has_a_frame_and_a_title_field() {
        let mut state = inbox_state();
        state.new_task_modal = Some(super::super::state::NewTaskModalState {
            title_input: String::new(),
            context: "personal".parse().unwrap(),
            error: None,
        });
        state.modal = Some(ModalKind::NewTask);
        let lines = draw(&state, 80, 20);
        let text = lines.join("\n");
        let top = lines
            .iter()
            .position(|l| l.contains("┌ New task"))
            .unwrap_or_else(|| panic!("{text}"));
        assert!(lines[top + 1].contains("│ Context [personal]"), "{text}");
        assert!(lines[top + 2].contains("┏ Title ━"), "{text}");
        assert!(lines[top + 3].contains("┃ ▏What needs doing?"), "{text}");
        assert!(
            lines[top + 6].contains("Enter create  Esc cancel"),
            "{text}"
        );
        assert!(lines[top + 7].contains("└──"), "{text}");
    }

    #[test]
    fn a_notes_heading_is_not_repeated_under_the_notes_header() {
        let d = detail("## Notes\n", None);
        let lines: Vec<String> = task_overview_lines(&d, 40, &test_theme())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let header = lines
            .iter()
            .position(|l| l.starts_with("── Notes ─"))
            .unwrap();
        assert_eq!(lines[header].chars().count(), 40);
        assert_eq!(lines[header + 1], "No notes.");
        assert_eq!(lines.len(), header + 2);
    }

    #[test]
    fn modals_do_not_panic_on_a_small_terminal() {
        let mut state = inbox_state();
        state.new_task_modal = Some(super::super::state::NewTaskModalState {
            title_input: "x".to_owned(),
            context: "personal".parse().unwrap(),
            error: None,
        });
        for modal in [ModalKind::NewTask, ModalKind::Help, ModalKind::Palette] {
            state.modal = Some(modal);
            for (width, height) in [(1, 1), (8, 3), (20, 6)] {
                draw(&state, width, height);
            }
        }
    }

    #[test]
    fn a_success_toast_has_a_check_mark() {
        let mut state = inbox_state();
        state.set_success("task created in inbox");
        let lines = draw(&state, 80, 5);
        assert!(
            lines[4].starts_with(" ✓ task created in inbox"),
            "{lines:?}"
        );
    }

    fn pick_modal_text(key: char) -> String {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new(
            test_theme(),
            None,
            Vec::new(),
            jiff::civil::date(2026, 10, 22),
        );
        let id: loom_core::TaskId = "k3q7zm2a".parse().unwrap();
        state.page_stack.push(ObjectPageState {
            target: ObjectTarget::Task(id),
            content: ObjectPageContent::Task(Box::new(TaskDetail {
                actions: vec![loom_api::Action::Edit],
                ..detail("", None)
            })),
            scroll: 0,
            tab: PageTab::Overview,
            events: EventsView::default(),
        });
        state.handle_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, &state)).unwrap();
        buffer_text_lines(&terminal).join("\n")
    }

    #[test]
    fn the_importance_picker_lists_the_choices_and_its_keys() {
        let text = pick_modal_text('i');
        assert!(text.contains("Change importance"), "{text}");
        assert!(text.contains("p0"), "{text}");
        assert!(text.contains("p3"), "{text}");
        assert!(text.contains("Enter apply"), "{text}");
    }

    #[test]
    fn the_due_picker_shows_the_current_date_and_the_accepted_forms() {
        let text = pick_modal_text('d');
        assert!(text.contains("Change due"), "{text}");
        assert!(text.contains("YYYY-MM-DD"), "{text}");
    }
}
