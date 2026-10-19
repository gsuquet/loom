//! Pure ratatui rendering of queues, object pages and modals (ADR-0014).

use loom_api::{
    Action, Importance, RepoSummary, SearchResult, StatusName, TaskDetail, TaskSummary,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use crate::tui::color::{Theme, status_glyph};
use crate::tui::keymap::{Layer, QueueTab, ViewKind, bindings_for_layer, help_sections};
use crate::tui::state::{AppState, ModalState, ObjectPageContent, ObjectPageState, ObjectTarget};

const fn importance_label(importance: Importance) -> &'static str {
    match importance {
        Importance::P0 => "p0",
        Importance::P1 => "p1",
        Importance::P2 => "p2",
        Importance::P3 => "p3",
    }
}

const fn action_label(action: Action) -> &'static str {
    match action {
        Action::SetStatus(StatusName::Inbox) => "inbox",
        Action::SetStatus(StatusName::Ready) => "ready",
        Action::SetStatus(StatusName::Active) => "active",
        Action::SetStatus(StatusName::Waiting) => "waiting",
        Action::SetStatus(StatusName::Done) => "done",
        Action::SetStatus(StatusName::Cancelled) => "cancelled",
        Action::Pin => "pin",
        Action::Unpin => "unpin",
        Action::LogNote => "log_note",
        Action::Edit => "edit",
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
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_top_bar(frame, chunks[0], state);

    if let Some(page) = state.object_stack.last() {
        render_object_page(frame, chunks[1], state, page);
    } else {
        render_queue_view(frame, chunks[1], state, area.width);
    }

    render_footer(frame, chunks[2], state);

    if let Some(modal) = &state.modal {
        render_modal(frame, area, state, modal);
    }
}

fn render_top_bar(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw(" "));

    for (idx, tab) in QueueTab::ALL.into_iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" │ ", state.theme.muted()));
        }
        let count = state.tab_count(tab);
        let text = format!("{} {} {}", tab.number(), tab.title(), count);
        let style = if state.object_stack.is_empty() && state.tab == tab {
            state.theme.active_tab()
        } else {
            state.theme.muted()
        };
        spans.push(Span::styled(text, style));
    }

    spans.push(Span::raw("   "));
    match &state.context_filter {
        Some(ctx) => {
            spans.push(Span::styled(
                format!("● {ctx}"),
                state.theme.context_style(ctx.as_str()),
            ));
        }
        None => {
            if state.contexts.is_empty() {
                spans.push(Span::styled("all contexts", state.theme.muted()));
            } else {
                for (i, ctx) in state.contexts.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::raw(" "));
                    }
                    spans.push(Span::styled(
                        format!("● {ctx}"),
                        state.theme.context_style(ctx.as_str()),
                    ));
                }
            }
        }
    }

    spans.push(Span::raw("  "));
    spans.push(Span::styled(state.sse_state.label(), state.theme.muted()));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_queue_view(frame: &mut Frame<'_>, area: Rect, state: &AppState, terminal_width: u16) {
    if state.show_preview(terminal_width) {
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
    let context_label = state
        .context_filter
        .as_ref()
        .map_or_else(|| "all contexts".to_owned(), ToString::to_string);
    let title = format!(" {} · {} ", state.tab.title(), context_label);
    let block = Block::default().borders(Borders::ALL).title(title);

    let selected = state.active_selected_index();
    let items: Vec<ListItem<'static>> = match state.tab {
        QueueTab::Today => build_task_rows(&state.today, selected, &state.theme),
        QueueTab::Inbox => build_task_rows(&state.inbox, selected, &state.theme),
        QueueTab::Waiting => build_task_rows(&state.waiting, selected, &state.theme),
        QueueTab::Repos => build_repo_rows(&state.repos, selected, &state.theme),
    };

    if items.is_empty() {
        let empty = Paragraph::new(Line::styled(
            "  No items in this queue.",
            state.theme.muted(),
        ))
        .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn build_task_rows(
    tasks: &[TaskSummary],
    selected: usize,
    theme: &Theme,
) -> Vec<ListItem<'static>> {
    tasks
        .iter()
        .enumerate()
        .map(|(idx, summary)| {
            let is_selected = idx == selected;
            let cursor = if is_selected { "▸" } else { " " };
            let status_name = summary.task.as_ref().and_then(|t| t.status.name());
            let glyph = status_glyph(status_name);
            let importance = summary.task.as_ref().map(|t| t.importance);
            let imp_text = importance.map_or("--", importance_label);
            let imp_style = importance.map_or_else(Default::default, |i| theme.importance_style(i));
            let kind_text = summary
                .task
                .as_ref()
                .map_or_else(|| "invalid".to_owned(), |t| t.kind.name().to_owned());
            let title = summary
                .task
                .as_ref()
                .map_or_else(|| "(invalid task.md)".to_owned(), |t| t.title.clone());
            let ctx_name = summary.context.as_str().to_owned();
            let ctx_style = theme.context_style(&ctx_name);

            let line = Line::from(vec![
                Span::raw(format!("{cursor}{glyph} ")),
                Span::styled(format!("{imp_text} "), imp_style),
                Span::styled(format!("{ctx_name:<10} "), ctx_style),
                Span::styled(format!("{} ", summary.prefix), theme.muted()),
                Span::styled(format!("{kind_text:<7} "), theme.muted()),
                Span::raw(title),
            ]);
            let item = ListItem::new(line);
            if is_selected {
                item.style(theme.selected_row())
            } else {
                item
            }
        })
        .collect()
}

fn build_repo_rows(
    repos: &[RepoSummary],
    selected: usize,
    theme: &Theme,
) -> Vec<ListItem<'static>> {
    repos
        .iter()
        .enumerate()
        .map(|(idx, repo)| {
            let is_selected = idx == selected;
            let cursor = if is_selected { "▸ " } else { "  " };
            let ctx_key = repo.identity.context.context_name().to_owned();
            let ctx_style = theme.context_style(&ctx_key);
            let path = repo.identity.path.display().to_string();

            let line = Line::from(vec![
                Span::raw(cursor),
                Span::styled(format!("{ctx_key:<10} "), ctx_style),
                Span::raw(format!("{:<24} ", repo.identity.slug)),
                Span::styled(path, theme.muted()),
            ]);
            let item = ListItem::new(line);
            if is_selected {
                item.style(theme.selected_row())
            } else {
                item
            }
        })
        .collect()
}

fn render_preview_pane(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    match state.tab {
        QueueTab::Today | QueueTab::Inbox | QueueTab::Waiting => {
            let Some(summary) = state.selected_task() else {
                let block = Block::default().borders(Borders::ALL).title(" Preview ");
                frame.render_widget(
                    Paragraph::new(Line::styled("  Nothing selected.", state.theme.muted()))
                        .block(block),
                    area,
                );
                return;
            };
            let title_text = summary
                .task
                .as_ref()
                .map_or("(invalid task.md)", |t| t.title.as_str());
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} {} ", summary.prefix, title_text));

            let lines = if let Some(detail) = &state.preview_task
                && detail.summary.id == summary.id
            {
                task_detail_lines(detail, &state.theme)
            } else {
                task_summary_lines(summary, &state.theme)
            };
            let paragraph = Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
        }
        QueueTab::Repos => {
            let Some(repo) = state.selected_repo() else {
                let block = Block::default().borders(Borders::ALL).title(" Preview ");
                frame.render_widget(
                    Paragraph::new(Line::styled("  Nothing selected.", state.theme.muted()))
                        .block(block),
                    area,
                );
                return;
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", repo.identity.slug));
            let lines = repo_detail_lines(repo, &state.theme);
            let paragraph = Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
        }
    }
}

const fn status_label(status: StatusName) -> &'static str {
    match status {
        StatusName::Inbox => "inbox",
        StatusName::Ready => "ready",
        StatusName::Active => "active",
        StatusName::Waiting => "waiting",
        StatusName::Done => "done",
        StatusName::Cancelled => "cancelled",
    }
}

fn task_summary_lines(summary: &TaskSummary, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let ctx = summary.context.as_str().to_owned();
    if let Some(task) = &summary.task {
        let status = task.status.name().map_or("unknown", status_label);
        let imp = importance_label(task.importance);
        let mut meta = vec![
            Span::raw(format!("{} · {status} · {imp} · ", task.kind.name())),
            Span::styled(ctx, theme.context_style(summary.context.as_str())),
        ];
        if let Some(due) = &task.due {
            meta.push(Span::raw(format!(" · due {due}")));
        }
        lines.push(Line::from(meta));
    } else {
        lines.push(Line::from(vec![
            Span::raw("invalid task.md · "),
            Span::styled(ctx, theme.context_style(summary.context.as_str())),
        ]));
    }
    lines.push(Line::styled(format!("id {}", summary.id), theme.muted()));
    lines
}

fn task_detail_lines(detail: &TaskDetail, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = task_summary_lines(&detail.summary, theme);
    if !detail.actions.is_empty() {
        let acts: Vec<&str> = detail.actions.iter().copied().map(action_label).collect();
        lines.push(Line::styled(
            format!("actions: {}", acts.join(", ")),
            theme.muted(),
        ));
    }
    lines.push(Line::styled(
        "────────────────────────────────────────",
        theme.muted(),
    ));
    if detail.body.trim().is_empty() {
        lines.push(Line::styled("(no notes)", theme.muted()));
    } else {
        for body_line in detail.body.lines() {
            lines.push(Line::raw(body_line.to_owned()));
        }
    }
    lines
}

fn repo_detail_lines(repo: &RepoSummary, theme: &Theme) -> Vec<Line<'static>> {
    let ctx = repo.identity.context.context_name().to_owned();
    let mut lines = vec![
        Line::from(vec![
            Span::raw("context: "),
            Span::styled(ctx.clone(), theme.context_style(&ctx)),
        ]),
        Line::raw(format!("slug:    {}", repo.identity.slug)),
        Line::raw(format!("path:    {}", repo.identity.path.display())),
    ];
    for remote in &repo.identity.remotes {
        lines.push(Line::styled(
            format!("remote {}: {}", remote.name, remote.url),
            theme.muted(),
        ));
    }
    lines
}

fn render_object_page(frame: &mut Frame<'_>, area: Rect, state: &AppState, page: &ObjectPageState) {
    let (title, lines) = match &page.content {
        ObjectPageContent::Task(detail) => {
            let title_text = detail
                .summary
                .task
                .as_ref()
                .map_or("(invalid task.md)", |t| t.title.as_str());
            (
                format!(" Task {} · {} ", detail.summary.prefix, title_text),
                task_detail_lines(detail, &state.theme),
            )
        }
        ObjectPageContent::Repo(repo) => (
            format!(" Repo {} ", repo.identity.slug),
            repo_detail_lines(repo, &state.theme),
        ),
        ObjectPageContent::Loading(target) => {
            let label = match target {
                ObjectTarget::Task(id) => format!("Task {id}"),
                ObjectTarget::Repo(slug) => format!("Repo {slug}"),
            };
            (
                format!(" {label} "),
                vec![Line::styled("Loading...", state.theme.muted())],
            )
        }
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((page.scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    if let Some(msg) = &state.status_message {
        frame.render_widget(Paragraph::new(Line::raw(format!(" {msg}"))), area);
        return;
    }

    let view = state.active_view();
    let hints: Vec<(&str, &str)> = match view {
        ViewKind::Queue => vec![
            ("Enter", "open"),
            ("|", "preview"),
            ("@", "context"),
            ("r", "refresh"),
            (":", "palette"),
            ("/", "search"),
            ("?", "help"),
            ("q", "quit"),
        ],
        ViewKind::ObjectPage => vec![
            ("Esc", "back"),
            ("j/k", "scroll"),
            ("r", "refresh"),
            (":", "palette"),
            ("/", "search"),
            ("?", "help"),
            ("q", "quit"),
        ],
    };

    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    for (idx, (key, desc)) in hints.into_iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(key.to_owned(), state.theme.active_tab()));
        spans.push(Span::styled(format!(" {desc}"), state.theme.muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[allow(clippy::too_many_lines)]
fn render_modal(frame: &mut Frame<'_>, area: Rect, state: &AppState, modal: &ModalState) {
    let popup = centered_rect(70, 75, area);
    frame.render_widget(Clear, popup);

    match modal {
        ModalState::Help { scroll } => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Key bindings (?) ");
            let lines = render_help_lines(state.active_view(), &state.theme);
            let paragraph = Paragraph::new(lines).block(block).scroll((*scroll, 0));
            frame.render_widget(paragraph, popup);
        }
        ModalState::Palette { query, selected } => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Command palette (:) ");
            let candidates = state.palette_candidates();
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(": ", state.theme.active_tab()),
                    Span::raw(query.clone()),
                ]),
                Line::styled(
                    "────────────────────────────────────────",
                    state.theme.muted(),
                ),
            ];
            for (idx, item) in candidates.iter().enumerate() {
                let is_sel = idx == *selected;
                let prefix = if is_sel { "▸ " } else { "  " };
                let row = Line::from(vec![
                    Span::raw(prefix),
                    Span::styled(format!("{:<8} ", item.category), state.theme.muted()),
                    Span::raw(format!("{:<34} ", item.label)),
                    Span::styled(item.shortcut, state.theme.muted()),
                ]);
                lines.push(if is_sel {
                    row.style(state.theme.selected_row())
                } else {
                    row
                });
            }
            frame.render_widget(Paragraph::new(lines).block(block), popup);
        }
        ModalState::Search {
            query,
            results,
            selected,
        } => {
            let block = Block::default().borders(Borders::ALL).title(" Search (/) ");
            let mut lines = vec![
                Line::from(vec![
                    Span::styled("/ ", state.theme.active_tab()),
                    Span::raw(query.clone()),
                ]),
                Line::styled(
                    "────────────────────────────────────────",
                    state.theme.muted(),
                ),
            ];
            if results.is_empty() {
                lines.push(Line::styled(
                    if query.trim().is_empty() {
                        "  Type to search tasks and repositories..."
                    } else {
                        "  No matches."
                    },
                    state.theme.muted(),
                ));
            } else {
                for (idx, result) in results.iter().enumerate() {
                    let is_sel = idx == *selected;
                    let prefix = if is_sel { "▸ " } else { "  " };
                    let row = match result {
                        SearchResult::Task(task) => Line::from(vec![
                            Span::raw(prefix),
                            Span::styled(
                                format!("{:<10} ", task.context),
                                state.theme.context_style(task.context.as_str()),
                            ),
                            Span::styled(format!("{} ", task.id), state.theme.muted()),
                            Span::raw(task.title.clone()),
                        ]),
                        SearchResult::Repo(repo) => Line::from(vec![
                            Span::raw(prefix),
                            Span::styled(
                                format!("{:<10} ", repo.context),
                                state.theme.context_style(&repo.context),
                            ),
                            Span::raw(format!("repo {} ", repo.slug)),
                            Span::styled(repo.path.display().to_string(), state.theme.muted()),
                        ]),
                    };
                    lines.push(if is_sel {
                        row.style(state.theme.selected_row())
                    } else {
                        row
                    });
                }
            }
            frame.render_widget(Paragraph::new(lines).block(block), popup);
        }
        ModalState::ContextPicker { selected } => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Filter by context (@) ");
            let mut lines = Vec::new();
            let all_selected = *selected == 0;
            let all_line = Line::from(vec![
                Span::raw(if all_selected { "▸ " } else { "  " }),
                Span::raw("All contexts"),
            ]);
            lines.push(if all_selected {
                all_line.style(state.theme.selected_row())
            } else {
                all_line
            });
            for (idx, ctx) in state.contexts.iter().enumerate() {
                let is_sel = *selected == idx + 1;
                let line = Line::from(vec![
                    Span::raw(if is_sel { "▸ " } else { "  " }),
                    Span::styled(format!("● {ctx}"), state.theme.context_style(ctx.as_str())),
                ]);
                lines.push(if is_sel {
                    line.style(state.theme.selected_row())
                } else {
                    line
                });
            }
            frame.render_widget(Paragraph::new(lines).block(block), popup);
        }
    }
}

/// Builds the lines of the `?` help modal from the registered bindings of `view` and all other
/// layers.
#[must_use]
pub fn render_help_lines(view: ViewKind, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (idx, (layer, bindings)) in help_sections(view).into_iter().enumerate() {
        if idx > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            format!("{} layer", layer.title()),
            theme.active_tab(),
        ));
        for binding in bindings {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{:<16}", binding.key_label), theme.active_tab()),
                Span::raw(binding.description),
            ]));
        }
    }
    // Also include the inactive view layer so every binding in the keymap is inspectable from `?`.
    let other_view = match view {
        ViewKind::Queue => ViewKind::ObjectPage,
        ViewKind::ObjectPage => ViewKind::Queue,
    };
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!("{} layer", Layer::View(other_view).title()),
        theme.active_tab(),
    ));
    for binding in bindings_for_layer(Layer::View(other_view)) {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{:<16}", binding.key_label), theme.active_tab()),
            Span::raw(binding.description),
        ]));
    }
    lines
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
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
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::keymap::ModalKind;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use loom_api::{
        ContextName, Importance, Kind, Relations, Status, Task, TaskId, TaskMatch, Timestamp,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_to_string(state: &AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut lines = Vec::with_capacity(height as usize);
        for y in 0..height {
            let mut row = String::new();
            for x in 0..width {
                row.push_str(buffer[(x, y)].symbol());
            }
            lines.push(row.trim_end().to_owned());
        }
        lines.join("\n")
    }

    fn sample_state() -> AppState {
        let mut state = AppState::default();
        let id: TaskId = "k3q7zm2a".parse().unwrap();
        let ctx: ContextName = "company-a".parse().unwrap();
        let ts = Timestamp::from_unix_secs(1_791_792_000).unwrap();
        let summary = TaskSummary {
            id: id.clone(),
            prefix: "k3q7".to_owned(),
            context: ctx.clone(),
            task: Some(Task {
                id,
                context: ctx,
                title: "Login timeout on mobile checkout".to_owned(),
                kind: Kind::Bug,
                importance: Importance::P1,
                status: Status::Active,
                due: None,
                pinned: false,
                links: Vec::new(),
                relations: Relations::default(),
                created: ts,
                updated: ts,
            }),
            read_only: None,
            rank: None,
        };
        let detail = TaskDetail {
            summary: summary.clone(),
            body: "## Symptoms\nMobile users time out after 30 s.".to_owned(),
            actions: Vec::new(),
        };
        state.set_queues(vec![summary], Vec::new(), Vec::new(), Vec::new());
        state.set_task_detail(detail);
        state
    }

    #[test]
    fn every_registered_key_binding_appears_in_help() {
        let all_layers = [
            Layer::Global,
            Layer::View(ViewKind::Queue),
            Layer::View(ViewKind::ObjectPage),
            Layer::Modal(ModalKind::Help),
            Layer::Modal(ModalKind::Palette),
            Layer::Modal(ModalKind::Search),
            Layer::Modal(ModalKind::ContextPicker),
        ];

        for active_view in ViewKind::ALL {
            let lines = render_help_lines(active_view, &Theme::default());
            let help_text: String = lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");

            for layer in all_layers {
                for binding in bindings_for_layer(layer) {
                    assert!(
                        help_text.contains(binding.key_label),
                        "missing key_label {:?} of {:?} in help:\n{help_text}",
                        binding.key_label,
                        layer
                    );
                    assert!(
                        help_text.contains(binding.description),
                        "missing description {:?} of {:?} in help:\n{help_text}",
                        binding.description,
                        layer
                    );
                }
            }
        }
    }

    #[test]
    fn queue_renders_preview_pane_when_wide_and_hides_it_when_narrow_or_toggled() {
        let mut state = sample_state();

        let wide = render_to_string(&state, 140, 20);
        assert!(wide.contains("1 Today 1"), "{wide}");
        assert!(wide.contains("company-a"), "{wide}");
        assert!(wide.contains("Login timeout on mobile checkout"), "{wide}");
        assert!(wide.contains("## Symptoms"), "{wide}");

        // Under 120 columns, the preview pane is hidden automatically.
        let narrow = render_to_string(&state, 100, 20);
        assert!(
            narrow.contains("Login timeout on mobile checkout"),
            "{narrow}"
        );
        assert!(!narrow.contains("## Symptoms"), "{narrow}");

        // Pressing `|` hides the preview pane even on a 140-column terminal.
        state.handle_key(KeyEvent::new(KeyCode::Char('|'), KeyModifiers::NONE));
        let toggled = render_to_string(&state, 140, 20);
        assert!(!toggled.contains("## Symptoms"), "{toggled}");
    }

    #[test]
    fn object_page_and_modals_render_in_test_backend() {
        let mut state = sample_state();

        // Open object page with Enter.
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let page = render_to_string(&state, 120, 20);
        assert!(
            page.contains("Task k3q7 · Login timeout on mobile checkout"),
            "{page}"
        );
        assert!(page.contains("Mobile users time out after 30 s."), "{page}");

        // Open `?` help modal.
        state.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        let help = render_to_string(&state, 120, 35);
        assert!(help.contains("Key bindings (?)"), "{help}");
        assert!(help.contains("Open command palette"), "{help}");

        // Close help and open `:` command palette.
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        let palette = render_to_string(&state, 120, 25);
        assert!(palette.contains("Command palette (:)"), "{palette}");
        assert!(palette.contains("Today queue"), "{palette}");

        // Close palette and open `/` search modal with results.
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        state.set_search_results(vec![SearchResult::Task(TaskMatch {
            id: "k3q7zm2a".parse().unwrap(),
            context: "company-a".parse().unwrap(),
            title: "Login timeout on mobile checkout".to_owned(),
            status: Some("active".to_owned()),
            link_keys: Vec::new(),
            score: 1.0,
            matched_fields: Vec::new(),
        })]);
        let search = render_to_string(&state, 120, 20);
        assert!(search.contains("Search (/)"), "{search}");
        assert!(
            search.contains("Login timeout on mobile checkout"),
            "{search}"
        );
    }
}
