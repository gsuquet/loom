//! Pure application state and transitions of the TUI (ADR-0014).
//!
//! No I/O happens in this module: key events and SSE [`ChangeEvent`]s mutate [`AppState`] and
//! return an [`AppEffect`] that the runtime loop in the binary executes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use loom_api::{
    Action, ChangeEvent, ContextName, Date, Importance, Kind, SearchResult, Status, StatusName,
    TaskDetail, TaskId, TaskSummary,
};

use crate::task::{parse_date, read_only_text};

use super::color::Theme;
use super::keymap::{KeyCommand, ModalKind, QueueTab, ViewKind, resolve_key};
use super::palette::{PaletteAction, PaletteItem, build_palette_items, filter_palette};

/// Minimum terminal width (in columns) required to display the right-hand preview pane.
pub const PREVIEW_MIN_WIDTH: u16 = 120;

/// Focused field in the Accept modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptField {
    Kind,
    Importance,
    Due,
}

impl AcceptField {
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Kind => Self::Importance,
            Self::Importance => Self::Due,
            Self::Due => Self::Kind,
        }
    }

    #[must_use]
    pub const fn prev(self) -> Self {
        match self {
            Self::Kind => Self::Due,
            Self::Importance => Self::Kind,
            Self::Due => Self::Importance,
        }
    }
}

/// State of the open Accept modal (`a`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptModalState {
    pub task_id: TaskId,
    pub kind: Kind,
    pub importance: Importance,
    pub due_input: String,
    pub field: AcceptField,
    pub error: Option<String>,
}

/// Focused field in the Snooze modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnoozeField {
    Until,
    On,
}

impl SnoozeField {
    #[must_use]
    pub const fn toggle(self) -> Self {
        match self {
            Self::Until => Self::On,
            Self::On => Self::Until,
        }
    }
}

/// State of the open Snooze modal (`z`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnoozeModalState {
    pub task_id: TaskId,
    pub until_input: String,
    pub on_input: String,
    pub field: SnoozeField,
    pub error: Option<String>,
}

/// State of the open New Task modal (`+`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTaskModalState {
    pub title_input: String,
    pub context: ContextName,
    pub error: Option<String>,
}

/// Triage kind choices for cyclic selection in Accept modal.
pub const TRIAGE_KINDS: &[Kind] = &[
    Kind::Chore,
    Kind::Bug,
    Kind::Story,
    Kind::Content { format: None },
    Kind::Research,
    Kind::Investigation,
    Kind::Review,
    Kind::Support,
    Kind::Admin,
    Kind::Interview,
    Kind::Learning,
];

/// Triage importance choices for cyclic selection in Accept modal.
pub const TRIAGE_IMPORTANCES: &[Importance] = &[
    Importance::P0,
    Importance::P1,
    Importance::P2,
    Importance::P3,
];

fn cycle_next<T: PartialEq + Clone>(items: &[T], current: &T) -> T {
    let pos = items.iter().position(|i| i == current).unwrap_or(0);
    items[(pos + 1) % items.len()].clone()
}

fn cycle_prev<T: PartialEq + Clone>(items: &[T], current: &T) -> T {
    let pos = items.iter().position(|i| i == current).unwrap_or(0);
    if pos == 0 {
        items[items.len() - 1].clone()
    } else {
        items[pos - 1].clone()
    }
}

fn cycle_kind_next(items: &[Kind], current: &Kind) -> Kind {
    let pos = items
        .iter()
        .position(|i| i.name() == current.name())
        .unwrap_or(0);
    items[(pos + 1) % items.len()].clone()
}

fn cycle_kind_prev(items: &[Kind], current: &Kind) -> Kind {
    let pos = items
        .iter()
        .position(|i| i.name() == current.name())
        .unwrap_or(0);
    if pos == 0 {
        items[items.len() - 1].clone()
    } else {
        items[pos - 1].clone()
    }
}

/// Connection state of the `GET /v1/events` SSE stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseState {
    /// Connected to `loomd` and receiving events.
    Connected,
    /// Disconnected; waiting before reconnecting with `Last-Event-ID`.
    Reconnecting,
    /// Failed or inactive.
    Disconnected,
}

impl SseState {
    /// Short badge shown in the top bar.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connected => "● live",
            Self::Reconnecting => "◌ reconnecting",
            Self::Disconnected => "○ offline",
        }
    }
}

/// Identifier of an object that can be opened as a full-screen object page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectTarget {
    /// A task by ID.
    Task(TaskId),
}

/// Content of an open object page on the view stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectPageContent {
    /// Task detail page.
    Task(Box<TaskDetail>),
    /// Placeholder while `loom` fetches the object from `loomd`.
    Loading(ObjectTarget),
}

/// State of one pushed object page on the navigation stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPageState {
    /// Target object.
    pub target: ObjectTarget,
    /// Loaded content or loading placeholder.
    pub content: ObjectPageContent,
    /// Vertical scroll offset.
    pub scroll: usize,
}

/// Pure application effects requested by state transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEffect {
    /// No effect needed.
    None,
    /// Refresh the queues and current open/previewed object.
    Refresh,
    /// Fetch task detail for preview or open page.
    FetchTask(TaskId),
    /// Search query changed; debounce before requesting `/v1/search`.
    Search(String),
    /// Accept an inbox task: sets status to ready with optional fields.
    AcceptTask {
        id: TaskId,
        kind: Option<Kind>,
        importance: Option<Importance>,
        due: Option<Option<Date>>,
    },
    /// Snooze a task until date.
    SnoozeTask {
        id: TaskId,
        until: Date,
        on: Option<String>,
    },
    /// Cancel a task.
    CancelTask(TaskId),
    /// Mark a task done.
    DoneTask(TaskId),
    /// Focus task, set status active, and cd to repo on quit.
    GoTask(TaskId),
    /// Undo last task event.
    UndoTask(TaskId),
    /// Create new task in context.
    CreateTask { title: String, context: ContextName },
}

/// View display toggles for the Today queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TodayOptions {
    /// Whether Today queue displays priority reasons.
    pub why: bool,
    /// Whether Today queue groups tasks by parent project.
    pub group_by_parent: bool,
}

/// Pure state of the TUI.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Color and style theme.
    pub theme: Theme,
    /// Active context filter (`None` = all contexts).
    pub active_context: Option<ContextName>,
    /// All configured context names in configuration order.
    pub contexts: Vec<ContextName>,
    /// Active queue tab.
    pub active_tab: QueueTab,
    /// Tasks in the Today queue.
    pub today: Vec<TaskSummary>,
    /// Selected index in the Today queue.
    pub today_selected: usize,
    /// Tasks in the Inbox queue.
    pub inbox: Vec<TaskSummary>,
    /// Selected index in the Inbox queue.
    pub inbox_selected: usize,
    /// Tasks in the Waiting queue.
    pub waiting: Vec<TaskSummary>,
    /// Selected index in the Waiting queue.
    pub waiting_selected: usize,
    /// Whether the preview pane is enabled by user preference.
    pub preview_enabled: bool,
    /// Detail of the task currently shown in the preview pane.
    pub preview_detail: Option<TaskDetail>,
    /// Navigation stack of pushed object pages.
    pub page_stack: Vec<ObjectPageState>,
    /// Active modal overlay, if any.
    pub modal: Option<ModalKind>,
    /// Scroll offset in `?` help modal.
    pub help_scroll: usize,
    /// All available palette items.
    pub palette_items: Vec<PaletteItem>,
    /// Filtered palette items.
    pub filtered_palette: Vec<(usize, PaletteItem)>,
    /// Current search/palette query string.
    pub palette_query: String,
    /// Selected index in command palette.
    pub palette_selected: usize,
    /// Live search query string.
    pub search_query: String,
    /// Live search results.
    pub search_results: Vec<SearchResult>,
    /// Selected index in live search results.
    pub search_selected: usize,
    /// Selected index in context picker modal.
    pub context_picker_selected: usize,
    /// View options for the Today queue.
    pub today_options: TodayOptions,
    /// State of the active Accept modal, when open.
    pub accept_modal: Option<AcceptModalState>,
    /// State of the active Snooze modal, when open.
    pub snooze_modal: Option<SnoozeModalState>,
    /// State of the active New Task modal, when open.
    pub new_task_modal: Option<NewTaskModalState>,
    /// Fallback default context if none is active or selected.
    pub default_context: Option<ContextName>,
    /// Task ID of the last triage action executed, for undo (`u`).
    pub last_action: Option<TaskId>,
    /// Injected local date for pure date calculations.
    pub local_date: Date,
    /// Transitory status message shown on bottom bar.
    pub status_message: Option<String>,
    /// Visible error state from a failed network request or daemon error.
    pub last_error: Option<String>,
    /// SSE connection state.
    pub sse_state: SseState,
    /// Flag requesting the event loop to exit cleanly.
    pub should_quit: bool,
}

impl AppState {
    /// Creates a new initial [`AppState`].
    #[must_use]
    pub fn new(
        theme: Theme,
        active_context: Option<ContextName>,
        contexts: Vec<ContextName>,
        local_date: Date,
    ) -> Self {
        let palette_items = build_palette_items(&contexts);
        let filtered_palette = filter_palette(&palette_items, "");

        Self {
            theme,
            active_context,
            contexts,
            active_tab: QueueTab::Today,
            today: Vec::new(),
            today_selected: 0,
            inbox: Vec::new(),
            inbox_selected: 0,
            waiting: Vec::new(),
            waiting_selected: 0,
            preview_enabled: true,
            preview_detail: None,
            page_stack: Vec::new(),
            modal: None,
            help_scroll: 0,
            palette_items,
            filtered_palette,
            palette_query: String::new(),
            palette_selected: 0,
            search_query: String::new(),
            search_results: Vec::new(),
            search_selected: 0,
            context_picker_selected: 0,
            today_options: TodayOptions::default(),
            accept_modal: None,
            snooze_modal: None,
            new_task_modal: None,
            default_context: None,
            last_action: None,
            local_date,
            status_message: None,
            last_error: None,
            sse_state: SseState::Disconnected,
            should_quit: false,
        }
    }

    /// Updates the current local date for pure date calculations.
    pub const fn set_local_date(&mut self, local_date: Date) {
        self.local_date = local_date;
    }

    /// Records the successful completion of an action on a task for undo.
    pub fn record_action_success(&mut self, task_id: TaskId) {
        self.last_action = Some(task_id);
    }

    /// Sets or clears the task ID tracked for undo.
    pub fn set_last_action(&mut self, task_id: Option<TaskId>) {
        self.last_action = task_id;
    }

    /// Sets the fallback default context.
    #[must_use]
    pub fn with_default_context(mut self, default_context: Option<ContextName>) -> Self {
        self.default_context = default_context;
        self
    }

    /// Whether priority reasons are displayed in the Today queue.
    #[must_use]
    pub const fn why_enabled(&self) -> bool {
        self.today_options.why
    }

    /// Whether tasks are grouped by parent in the Today queue.
    #[must_use]
    pub const fn group_by_parent(&self) -> bool {
        self.today_options.group_by_parent
    }

    /// Sorts tasks in the Today queue by priority rank (default order).
    pub fn sort_today_by_rank(&mut self) {
        if self.today.is_empty() {
            return;
        }
        let selected_id = self.today.get(self.today_selected).map(|t| t.id.clone());
        self.today
            .sort_by_key(|t| t.rank.map_or(usize::MAX, |r| r.position));
        if let Some(id) = selected_id {
            self.today_selected = self.today.iter().position(|t| t.id == id).unwrap_or(0);
        }
    }

    /// Groups tasks in the Today queue by parent project.
    pub fn sort_today_by_parent(&mut self) {
        if self.today.is_empty() {
            return;
        }
        let selected_id = self.today.get(self.today_selected).map(|t| t.id.clone());
        self.sort_today_by_rank();

        let ids_in_today: std::collections::HashSet<&TaskId> =
            self.today.iter().map(|t| &t.id).collect();
        let mut grouped: Vec<TaskSummary> = Vec::with_capacity(self.today.len());
        let mut placed = vec![false; self.today.len()];

        // 1. Root tasks: tasks that have no parent, OR whose parent is not in Today.
        for i in 0..self.today.len() {
            let parent_in_today = self.today[i]
                .task
                .as_ref()
                .and_then(|t| t.relations.parent.as_ref())
                .is_some_and(|p| ids_in_today.contains(p));

            if !parent_in_today && !placed[i] {
                placed[i] = true;
                let parent_id = self.today[i].id.clone();
                grouped.push(self.today[i].clone());
                place_subtree(&parent_id, &self.today, &mut placed, &mut grouped);
            }
        }

        // 2. Any remaining tasks (e.g. cycles):
        for i in 0..self.today.len() {
            if !placed[i] {
                placed[i] = true;
                let parent_id = self.today[i].id.clone();
                grouped.push(self.today[i].clone());
                place_subtree(&parent_id, &self.today, &mut placed, &mut grouped);
            }
        }

        self.today = grouped;
        if let Some(id) = selected_id {
            self.today_selected = self.today.iter().position(|t| t.id == id).unwrap_or(0);
        }
    }

    /// Active view under any modal.
    #[must_use]
    pub const fn active_view(&self) -> ViewKind {
        if self.page_stack.is_empty() {
            ViewKind::Queue
        } else {
            ViewKind::ObjectPage
        }
    }

    /// Handles a key event and produces an [`AppEffect`].
    pub fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        // Modal text input takes priority for character typing.
        if let Some(modal) = self.modal {
            match modal {
                ModalKind::Palette => {
                    if let KeyCode::Char(c) = key.code
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        self.palette_query.push(c);
                        self.filtered_palette =
                            filter_palette(&self.palette_items, &self.palette_query);
                        self.palette_selected = 0;
                        return AppEffect::None;
                    } else if key.code == KeyCode::Backspace {
                        self.palette_query.pop();
                        self.filtered_palette =
                            filter_palette(&self.palette_items, &self.palette_query);
                        self.palette_selected = 0;
                        return AppEffect::None;
                    }
                }
                ModalKind::Search => {
                    if let KeyCode::Char(c) = key.code
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        self.search_query.push(c);
                        return AppEffect::Search(self.search_query.clone());
                    } else if key.code == KeyCode::Backspace {
                        self.search_query.pop();
                        return AppEffect::Search(self.search_query.clone());
                    }
                }
                ModalKind::Accept => {
                    if let Some(m) = &mut self.accept_modal
                        && m.field == AcceptField::Due
                    {
                        if let KeyCode::Char(c) = key.code
                            && !key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            m.due_input.push(c);
                            return AppEffect::None;
                        } else if key.code == KeyCode::Backspace {
                            m.due_input.pop();
                            return AppEffect::None;
                        }
                    }
                }
                ModalKind::Snooze => {
                    if let Some(m) = &mut self.snooze_modal {
                        match m.field {
                            SnoozeField::Until => {
                                if let KeyCode::Char(c) = key.code
                                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                                {
                                    m.until_input.push(c);
                                    return AppEffect::None;
                                } else if key.code == KeyCode::Backspace {
                                    m.until_input.pop();
                                    return AppEffect::None;
                                }
                            }
                            SnoozeField::On => {
                                if let KeyCode::Char(c) = key.code
                                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                                {
                                    m.on_input.push(c);
                                    return AppEffect::None;
                                } else if key.code == KeyCode::Backspace {
                                    m.on_input.pop();
                                    return AppEffect::None;
                                }
                            }
                        }
                    }
                }
                ModalKind::NewTask => {
                    if let Some(m) = &mut self.new_task_modal {
                        if let KeyCode::Char(c) = key.code
                            && !key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            m.title_input.push(c);
                            return AppEffect::None;
                        } else if key.code == KeyCode::Backspace {
                            m.title_input.pop();
                            return AppEffect::None;
                        }
                    }
                }
                ModalKind::Help | ModalKind::ContextPicker => {}
            }
        }

        let cmd = resolve_key(self.modal, self.active_view(), key);
        cmd.map_or(AppEffect::None, |c| self.apply_command(c))
    }

    fn apply_command(&mut self, cmd: KeyCommand) -> AppEffect {
        match cmd {
            KeyCommand::Quit => {
                self.should_quit = true;
                AppEffect::None
            }
            KeyCommand::SelectTab(_)
            | KeyCommand::NextTab
            | KeyCommand::PrevTab
            | KeyCommand::MoveDown
            | KeyCommand::MoveUp
            | KeyCommand::MoveFirst
            | KeyCommand::MoveLast
            | KeyCommand::PageDown
            | KeyCommand::PageUp
            | KeyCommand::OpenSelected
            | KeyCommand::TogglePreview
            | KeyCommand::Refresh
            | KeyCommand::Back => self.apply_navigation_command(cmd),

            KeyCommand::OpenPalette
            | KeyCommand::OpenSearch
            | KeyCommand::OpenHelp
            | KeyCommand::OpenContextPicker => self.apply_open_modal_command(cmd),

            KeyCommand::TriageAccept
            | KeyCommand::TriageSnooze
            | KeyCommand::TriageCancel
            | KeyCommand::TriageDone
            | KeyCommand::TriageGo
            | KeyCommand::Undo
            | KeyCommand::NewTask
            | KeyCommand::ToggleWhy
            | KeyCommand::ToggleGroupByParent => self.apply_triage_command(cmd),

            KeyCommand::NextField
            | KeyCommand::PrevField
            | KeyCommand::PrevOption
            | KeyCommand::NextOption
            | KeyCommand::CloseModal
            | KeyCommand::ConfirmModal => self.apply_modal_command(cmd),
        }
    }

    fn apply_navigation_command(&mut self, cmd: KeyCommand) -> AppEffect {
        match cmd {
            KeyCommand::SelectTab(tab) => {
                self.active_tab = tab;
                self.update_preview_effect()
            }
            KeyCommand::NextTab => {
                self.active_tab = self.active_tab.next();
                self.update_preview_effect()
            }
            KeyCommand::PrevTab => {
                self.active_tab = self.active_tab.prev();
                self.update_preview_effect()
            }
            KeyCommand::MoveDown => {
                self.move_down();
                self.update_preview_effect()
            }
            KeyCommand::MoveUp => {
                self.move_up();
                self.update_preview_effect()
            }
            KeyCommand::MoveFirst => {
                self.set_selected(0);
                self.update_preview_effect()
            }
            KeyCommand::MoveLast => {
                let len = self.current_queue_len();
                if len > 0 {
                    self.set_selected(len - 1);
                }
                self.update_preview_effect()
            }
            KeyCommand::PageDown => {
                let curr = self.current_selected();
                let len = self.current_queue_len();
                if len > 0 {
                    self.set_selected((curr + 10).min(len - 1));
                }
                self.update_preview_effect()
            }
            KeyCommand::PageUp => {
                let curr = self.current_selected();
                self.set_selected(curr.saturating_sub(10));
                self.update_preview_effect()
            }
            KeyCommand::OpenSelected => self.open_selected(),
            KeyCommand::TogglePreview => {
                self.preview_enabled = !self.preview_enabled;
                self.update_preview_effect()
            }
            KeyCommand::Refresh => AppEffect::Refresh,
            KeyCommand::Back => {
                self.page_stack.pop();
                self.update_preview_effect()
            }
            _ => AppEffect::None,
        }
    }

    fn apply_open_modal_command(&mut self, cmd: KeyCommand) -> AppEffect {
        match cmd {
            KeyCommand::OpenPalette => {
                self.modal = Some(ModalKind::Palette);
                self.palette_query.clear();
                self.filtered_palette = filter_palette(&self.palette_items, "");
                self.palette_selected = 0;
            }
            KeyCommand::OpenSearch => {
                self.modal = Some(ModalKind::Search);
                self.search_query.clear();
                self.search_results.clear();
                self.search_selected = 0;
            }
            KeyCommand::OpenHelp => {
                self.modal = Some(ModalKind::Help);
                self.help_scroll = 0;
            }
            KeyCommand::OpenContextPicker => {
                self.modal = Some(ModalKind::ContextPicker);
                self.context_picker_selected = self.active_context.as_ref().map_or(0, |current| {
                    self.contexts
                        .iter()
                        .position(|c| c == current)
                        .map_or(0, |idx| idx + 1)
                });
            }
            _ => {}
        }
        AppEffect::None
    }

    fn actions_for_selected(&self) -> (Option<&TaskSummary>, Vec<Action>) {
        let Some(summary) = self.selected_task() else {
            return (None, Vec::new());
        };
        let actions = if let Some(detail) = &self.preview_detail
            && detail.summary.id == summary.id
        {
            detail.actions.clone()
        } else if let Some(task) = &summary.task {
            loom_core::available_actions(task)
        } else {
            Vec::new()
        };
        (Some(summary), actions)
    }

    fn editable_action_task(&mut self, required_status: StatusName) -> Option<TaskId> {
        let (summary, actions) = self.actions_for_selected();
        let summary = summary?;
        if let Some(ro) = &summary.read_only {
            self.status_message = Some(format!("task is read-only: {}", read_only_text(ro)));
            return None;
        }
        if actions.contains(&Action::SetStatus(required_status)) {
            Some(summary.id.clone())
        } else {
            self.status_message = Some(format!("cannot move task to {required_status}"));
            None
        }
    }

    fn go_action_task(&mut self) -> Option<TaskId> {
        let (summary, actions) = self.actions_for_selected();
        let summary = summary?;
        if let Some(ro) = &summary.read_only {
            self.status_message = Some(format!("task is read-only: {}", read_only_text(ro)));
            return None;
        }
        let is_active = summary.task.as_ref().map(|t| &t.status) == Some(&Status::Active);
        if is_active || actions.contains(&Action::SetStatus(StatusName::Active)) {
            Some(summary.id.clone())
        } else {
            self.status_message = Some("cannot activate task".into());
            None
        }
    }

    fn open_accept_modal(&mut self) -> AppEffect {
        let (summary, actions) = self.actions_for_selected();
        let Some(task) = summary else {
            return AppEffect::None;
        };
        if let Some(ro) = &task.read_only {
            self.status_message = Some(format!("task is read-only: {}", read_only_text(ro)));
            return AppEffect::None;
        }
        if !actions.contains(&Action::Accept) {
            self.status_message = Some("cannot accept: task is not in inbox".into());
            return AppEffect::None;
        }
        let default_kind = task.task.as_ref().map_or(Kind::Chore, |t| match &t.kind {
            Kind::Unknown { .. } => Kind::Chore,
            k => k.clone(),
        });
        let default_importance = task
            .task
            .as_ref()
            .map_or_else(|| Importance::default_for(&default_kind), |t| t.importance);
        let default_due = task
            .task
            .as_ref()
            .and_then(|t| t.due)
            .map_or(String::new(), |d| d.to_string());

        self.accept_modal = Some(AcceptModalState {
            task_id: task.id.clone(),
            kind: default_kind,
            importance: default_importance,
            due_input: default_due,
            field: AcceptField::Kind,
            error: None,
        });
        self.modal = Some(ModalKind::Accept);
        AppEffect::None
    }

    fn open_snooze_modal(&mut self) -> AppEffect {
        let (summary, actions) = self.actions_for_selected();
        let Some(task) = summary else {
            return AppEffect::None;
        };
        if let Some(ro) = &task.read_only {
            self.status_message = Some(format!("task is read-only: {}", read_only_text(ro)));
            return AppEffect::None;
        }
        if !actions.contains(&Action::SetStatus(StatusName::Waiting)) {
            self.status_message = Some("cannot snooze task".into());
            return AppEffect::None;
        }
        self.snooze_modal = Some(SnoozeModalState {
            task_id: task.id.clone(),
            until_input: "tomorrow".into(),
            on_input: String::new(),
            field: SnoozeField::Until,
            error: None,
        });
        self.modal = Some(ModalKind::Snooze);
        AppEffect::None
    }

    fn open_new_task_modal(&mut self) -> AppEffect {
        let context = if let Some(ctx) = &self.active_context {
            ctx.clone()
        } else if let Some(task) = self.selected_task() {
            task.context.clone()
        } else if let Some(ctx) = &self.default_context {
            ctx.clone()
        } else {
            self.status_message =
                Some("no context: select a task or configure default_context".into());
            return AppEffect::None;
        };
        self.new_task_modal = Some(NewTaskModalState {
            title_input: String::new(),
            context,
            error: None,
        });
        self.modal = Some(ModalKind::NewTask);
        AppEffect::None
    }

    fn apply_triage_command(&mut self, cmd: KeyCommand) -> AppEffect {
        match cmd {
            KeyCommand::TriageAccept => self.open_accept_modal(),
            KeyCommand::TriageSnooze => self.open_snooze_modal(),
            KeyCommand::TriageCancel => self
                .editable_action_task(StatusName::Cancelled)
                .map_or(AppEffect::None, AppEffect::CancelTask),
            KeyCommand::TriageDone => self
                .editable_action_task(StatusName::Done)
                .map_or(AppEffect::None, AppEffect::DoneTask),
            KeyCommand::TriageGo => self
                .go_action_task()
                .map_or(AppEffect::None, AppEffect::GoTask),
            KeyCommand::Undo => self.last_action.take().map_or_else(
                || {
                    self.set_status("nothing to undo");
                    AppEffect::None
                },
                AppEffect::UndoTask,
            ),
            KeyCommand::NewTask => self.open_new_task_modal(),
            KeyCommand::ToggleWhy => {
                self.today_options.why = !self.today_options.why;
                AppEffect::None
            }
            KeyCommand::ToggleGroupByParent => {
                self.today_options.group_by_parent = !self.today_options.group_by_parent;
                if self.today_options.group_by_parent {
                    self.sort_today_by_parent();
                } else {
                    self.sort_today_by_rank();
                }
                AppEffect::None
            }
            _ => AppEffect::None,
        }
    }

    fn apply_modal_command(&mut self, cmd: KeyCommand) -> AppEffect {
        match cmd {
            KeyCommand::NextField => {
                if self.modal == Some(ModalKind::Accept)
                    && let Some(m) = &mut self.accept_modal
                {
                    m.field = m.field.next();
                } else if self.modal == Some(ModalKind::Snooze)
                    && let Some(m) = &mut self.snooze_modal
                {
                    m.field = m.field.toggle();
                }
                AppEffect::None
            }
            KeyCommand::PrevField => {
                if self.modal == Some(ModalKind::Accept)
                    && let Some(m) = &mut self.accept_modal
                {
                    m.field = m.field.prev();
                } else if self.modal == Some(ModalKind::Snooze)
                    && let Some(m) = &mut self.snooze_modal
                {
                    m.field = m.field.toggle();
                }
                AppEffect::None
            }
            KeyCommand::PrevOption => {
                if self.modal == Some(ModalKind::Accept)
                    && let Some(m) = &mut self.accept_modal
                {
                    match m.field {
                        AcceptField::Kind => {
                            m.kind = cycle_kind_prev(TRIAGE_KINDS, &m.kind);
                            m.importance = Importance::default_for(&m.kind);
                        }
                        AcceptField::Importance => {
                            m.importance = cycle_prev(TRIAGE_IMPORTANCES, &m.importance);
                        }
                        AcceptField::Due => {}
                    }
                }
                AppEffect::None
            }
            KeyCommand::NextOption => {
                if self.modal == Some(ModalKind::Accept)
                    && let Some(m) = &mut self.accept_modal
                {
                    match m.field {
                        AcceptField::Kind => {
                            m.kind = cycle_kind_next(TRIAGE_KINDS, &m.kind);
                            m.importance = Importance::default_for(&m.kind);
                        }
                        AcceptField::Importance => {
                            m.importance = cycle_next(TRIAGE_IMPORTANCES, &m.importance);
                        }
                        AcceptField::Due => {}
                    }
                }
                AppEffect::None
            }
            KeyCommand::CloseModal => {
                self.modal = None;
                self.accept_modal = None;
                self.snooze_modal = None;
                self.new_task_modal = None;
                AppEffect::None
            }
            KeyCommand::ConfirmModal => self.confirm_modal(),
            _ => AppEffect::None,
        }
    }

    fn confirm_modal(&mut self) -> AppEffect {
        let Some(modal) = self.modal else {
            return AppEffect::None;
        };

        match modal {
            ModalKind::Accept => self.confirm_accept_modal(),
            ModalKind::Snooze => self.confirm_snooze_modal(),
            ModalKind::NewTask => self.confirm_new_task_modal(),
            ModalKind::Palette => self.confirm_palette_modal(),
            ModalKind::ContextPicker => self.confirm_context_picker_modal(),
            ModalKind::Search => self.confirm_search_modal(),
            ModalKind::Help => {
                self.modal = None;
                AppEffect::None
            }
        }
    }

    fn confirm_accept_modal(&mut self) -> AppEffect {
        let Some(m) = &mut self.accept_modal else {
            return AppEffect::None;
        };
        let due = if m.due_input.trim().is_empty() {
            Some(None)
        } else {
            let Ok(d) = parse_date(m.due_input.trim(), self.local_date) else {
                m.error = Some(format!("invalid date {:?}", m.due_input.trim()));
                return AppEffect::None;
            };
            Some(Some(d))
        };
        let effect = AppEffect::AcceptTask {
            id: m.task_id.clone(),
            kind: Some(m.kind.clone()),
            importance: Some(m.importance),
            due,
        };
        self.modal = None;
        self.accept_modal = None;
        effect
    }

    fn confirm_snooze_modal(&mut self) -> AppEffect {
        let Some(m) = &mut self.snooze_modal else {
            return AppEffect::None;
        };
        let Ok(until) = parse_date(m.until_input.trim(), self.local_date) else {
            m.error = Some(format!("invalid date {:?}", m.until_input.trim()));
            return AppEffect::None;
        };
        let on = if m.on_input.trim().is_empty() {
            None
        } else {
            Some(m.on_input.trim().to_string())
        };
        let effect = AppEffect::SnoozeTask {
            id: m.task_id.clone(),
            until,
            on,
        };
        self.modal = None;
        self.snooze_modal = None;
        effect
    }

    fn confirm_new_task_modal(&mut self) -> AppEffect {
        let Some(m) = &mut self.new_task_modal else {
            return AppEffect::None;
        };
        let title = m.title_input.trim().to_string();
        if title.is_empty() {
            m.error = Some("task title cannot be empty".into());
            return AppEffect::None;
        }
        let effect = AppEffect::CreateTask {
            title,
            context: m.context.clone(),
        };
        self.modal = None;
        self.new_task_modal = None;
        effect
    }

    fn confirm_palette_modal(&mut self) -> AppEffect {
        self.modal = None;
        let Some((_, item)) = self.filtered_palette.get(self.palette_selected) else {
            return AppEffect::None;
        };
        match &item.action {
            PaletteAction::SelectTab(tab) => {
                self.active_tab = *tab;
                self.update_preview_effect()
            }
            PaletteAction::OpenSearch => {
                self.modal = Some(ModalKind::Search);
                self.search_query.clear();
                self.search_results.clear();
                self.search_selected = 0;
                AppEffect::None
            }
            PaletteAction::OpenContextPicker => {
                self.modal = Some(ModalKind::ContextPicker);
                self.context_picker_selected = 0;
                AppEffect::None
            }
            PaletteAction::SetContext(ctx) => {
                self.active_context = ctx.clone();
                AppEffect::Refresh
            }
            PaletteAction::TogglePreview => {
                self.preview_enabled = !self.preview_enabled;
                self.update_preview_effect()
            }
            PaletteAction::Refresh => AppEffect::Refresh,
            PaletteAction::OpenHelp => {
                self.modal = Some(ModalKind::Help);
                self.help_scroll = 0;
                AppEffect::None
            }
            PaletteAction::Quit => {
                self.should_quit = true;
                AppEffect::None
            }
        }
    }

    fn confirm_context_picker_modal(&mut self) -> AppEffect {
        self.modal = None;
        if self.context_picker_selected == 0 {
            self.active_context = None;
        } else if let Some(ctx) = self.contexts.get(self.context_picker_selected - 1) {
            self.active_context = Some(ctx.clone());
        }
        AppEffect::Refresh
    }

    fn confirm_search_modal(&mut self) -> AppEffect {
        self.modal = None;
        let Some(res) = self.search_results.get(self.search_selected) else {
            return AppEffect::None;
        };
        match res {
            SearchResult::Task(task) => {
                self.page_stack.push(ObjectPageState {
                    target: ObjectTarget::Task(task.id.clone()),
                    content: ObjectPageContent::Loading(ObjectTarget::Task(task.id.clone())),
                    scroll: 0,
                });
                AppEffect::FetchTask(task.id.clone())
            }
            SearchResult::Repo(_) => AppEffect::None,
        }
    }

    /// Opens the currently selected item in an object page.
    pub fn open_selected(&mut self) -> AppEffect {
        if let Some(task) = self.selected_task() {
            let id = task.id.clone();
            self.page_stack.push(ObjectPageState {
                target: ObjectTarget::Task(id.clone()),
                content: ObjectPageContent::Loading(ObjectTarget::Task(id.clone())),
                scroll: 0,
            });
            AppEffect::FetchTask(id)
        } else {
            AppEffect::None
        }
    }

    /// Task currently selected in the active queue.
    #[must_use]
    pub fn selected_task(&self) -> Option<&TaskSummary> {
        match self.active_tab {
            QueueTab::Today => self.today.get(self.today_selected),
            QueueTab::Inbox => self.inbox.get(self.inbox_selected),
            QueueTab::Waiting => self.waiting.get(self.waiting_selected),
        }
    }

    /// Number of items in the active queue.
    #[must_use]
    pub const fn current_queue_len(&self) -> usize {
        match self.active_tab {
            QueueTab::Today => self.today.len(),
            QueueTab::Inbox => self.inbox.len(),
            QueueTab::Waiting => self.waiting.len(),
        }
    }

    /// Selected index in the active queue.
    #[must_use]
    pub const fn current_selected(&self) -> usize {
        match self.active_tab {
            QueueTab::Today => self.today_selected,
            QueueTab::Inbox => self.inbox_selected,
            QueueTab::Waiting => self.waiting_selected,
        }
    }

    /// Sets the selected index in the active queue.
    pub const fn set_selected(&mut self, idx: usize) {
        match self.active_tab {
            QueueTab::Today => self.today_selected = idx,
            QueueTab::Inbox => self.inbox_selected = idx,
            QueueTab::Waiting => self.waiting_selected = idx,
        }
    }

    fn move_down(&mut self) {
        if let Some(modal) = self.modal {
            match modal {
                ModalKind::Help => self.help_scroll = self.help_scroll.saturating_add(1),
                ModalKind::Palette => {
                    let len = self.filtered_palette.len();
                    if len > 0 {
                        self.palette_selected = (self.palette_selected + 1).min(len - 1);
                    }
                }
                ModalKind::Search => {
                    let len = self.search_results.len();
                    if len > 0 {
                        self.search_selected = (self.search_selected + 1).min(len - 1);
                    }
                }
                ModalKind::ContextPicker => {
                    let len = self.contexts.len() + 1; // 1 for 'all'
                    self.context_picker_selected = (self.context_picker_selected + 1).min(len - 1);
                }
                ModalKind::Accept => {
                    if let Some(m) = &mut self.accept_modal {
                        m.field = m.field.next();
                    }
                }
                ModalKind::Snooze => {
                    if let Some(m) = &mut self.snooze_modal {
                        m.field = m.field.toggle();
                    }
                }
                ModalKind::NewTask => {}
            }
            return;
        }

        if let Some(page) = self.page_stack.last_mut() {
            page.scroll = page.scroll.saturating_add(1);
            return;
        }

        let curr = self.current_selected();
        let len = self.current_queue_len();
        if len > 0 {
            self.set_selected((curr + 1).min(len - 1));
        }
    }

    fn move_up(&mut self) {
        if let Some(modal) = self.modal {
            match modal {
                ModalKind::Help => self.help_scroll = self.help_scroll.saturating_sub(1),
                ModalKind::Palette => {
                    self.palette_selected = self.palette_selected.saturating_sub(1);
                }
                ModalKind::Search => {
                    self.search_selected = self.search_selected.saturating_sub(1);
                }
                ModalKind::ContextPicker => {
                    self.context_picker_selected = self.context_picker_selected.saturating_sub(1);
                }
                ModalKind::Accept => {
                    if let Some(m) = &mut self.accept_modal {
                        m.field = m.field.prev();
                    }
                }
                ModalKind::Snooze => {
                    if let Some(m) = &mut self.snooze_modal {
                        m.field = m.field.toggle();
                    }
                }
                ModalKind::NewTask => {}
            }
            return;
        }

        if let Some(page) = self.page_stack.last_mut() {
            page.scroll = page.scroll.saturating_sub(1);
            return;
        }

        let curr = self.current_selected();
        self.set_selected(curr.saturating_sub(1));
    }

    fn update_preview_effect(&self) -> AppEffect {
        if !self.preview_enabled || !self.page_stack.is_empty() {
            return AppEffect::None;
        }
        self.selected_task()
            .map_or(AppEffect::None, |t| AppEffect::FetchTask(t.id.clone()))
    }

    /// Handles an incoming SSE change event from loomd.
    pub fn apply_change_event(&mut self, event: &ChangeEvent) -> AppEffect {
        match event {
            ChangeEvent::TaskChanged { context, .. }
            | ChangeEvent::TaskArchived { context, .. } => {
                if self.active_context.as_ref().is_some_and(|c| c != context) {
                    return AppEffect::None;
                }
                AppEffect::Refresh
            }
            ChangeEvent::CountsChanged { context, .. } => {
                if self.active_context.as_ref().is_some_and(|c| c != context) {
                    return AppEffect::None;
                }
                AppEffect::Refresh
            }
            // There is no Repos queue; repo updates come often and trigger no TUI refresh.
            ChangeEvent::RepoChanged { .. } => AppEffect::None,
            ChangeEvent::Resync => AppEffect::Refresh,
        }
    }

    /// Sets a temporary status message on the status bar.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
    }

    /// Sets an error message to display prominently.
    pub fn set_error(&mut self, err: impl Into<String>) {
        self.last_error = Some(err.into());
    }

    /// Clears the active error.
    pub fn clear_error(&mut self) {
        self.last_error = None;
    }
}

fn place_subtree(
    parent_id: &TaskId,
    today: &[TaskSummary],
    placed: &mut [bool],
    result: &mut Vec<TaskSummary>,
) {
    for j in 0..today.len() {
        if !placed[j]
            && today[j]
                .task
                .as_ref()
                .and_then(|t| t.relations.parent.as_ref())
                == Some(parent_id)
        {
            placed[j] = true;
            result.push(today[j].clone());
            place_subtree(&today[j].id, today, placed, result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_api::{RankReason, TaskReadOnly};
    use std::collections::BTreeMap;
    use std::str::FromStr;

    fn test_theme() -> Theme {
        Theme::from_config(&BTreeMap::new(), false)
    }

    fn test_date() -> Date {
        jiff::civil::date(2026, 10, 22)
    }

    fn make_task(id: &str, status: Status, read_only: Option<TaskReadOnly>) -> TaskSummary {
        let task_id: TaskId = id.parse().unwrap();
        let created = loom_core::Timestamp::EPOCH;
        TaskSummary {
            id: task_id.clone(),
            prefix: id.to_owned(),
            context: ContextName::from_str("work").unwrap(),
            task: Some(loom_core::Task {
                id: task_id,
                title: format!("Task {id}"),
                kind: Kind::Bug,
                context: ContextName::from_str("work").unwrap(),
                status,
                importance: Importance::P1,
                due: Some(jiff::civil::date(2026, 10, 25)),
                pinned: false,
                links: Vec::new(),
                relations: loom_core::Relations::default(),
                created,
                updated: created,
            }),
            read_only,
            rank: None,
        }
    }

    #[test]
    fn initial_state_has_three_tabs_and_empty_queues() {
        let state = AppState::new(test_theme(), None, Vec::new(), test_date());
        assert_eq!(state.active_tab, QueueTab::Today);
        assert_eq!(state.current_queue_len(), 0);
        assert!(!state.should_quit);
    }

    #[test]
    fn quit_key_sets_should_quit() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let effect = state.handle_key(q);
        assert_eq!(effect, AppEffect::None);
        assert!(state.should_quit);
    }

    #[test]
    fn tab_switching_updates_active_tab() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        let effect = state.handle_key(tab);
        assert_eq!(state.active_tab, QueueTab::Inbox);
        assert_eq!(effect, AppEffect::None);

        let effect2 = state.handle_key(tab);
        assert_eq!(state.active_tab, QueueTab::Waiting);
        assert_eq!(effect2, AppEffect::None);

        let effect3 = state.handle_key(tab);
        assert_eq!(state.active_tab, QueueTab::Today);
        assert_eq!(effect3, AppEffect::None);
    }

    #[test]
    fn apply_change_event_ignores_repo_changed() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        let event = ChangeEvent::RepoChanged {
            slug: "work/my-repo".to_owned(),
        };
        assert_eq!(state.apply_change_event(&event), AppEffect::None);
    }

    #[test]
    fn apply_change_event_returns_refresh_for_task_events() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        let task_id: TaskId = "k3q7zm2a".parse().unwrap();
        state.page_stack.push(ObjectPageState {
            target: ObjectTarget::Task(task_id.clone()),
            content: ObjectPageContent::Loading(ObjectTarget::Task(task_id.clone())),
            scroll: 0,
        });

        let event = ChangeEvent::TaskChanged {
            context: "work".parse().unwrap(),
            id: task_id,
        };
        assert_eq!(state.apply_change_event(&event), AppEffect::Refresh);

        let other_id: TaskId = "p9x8w111".parse().unwrap();
        let other_event = ChangeEvent::TaskChanged {
            context: "work".parse().unwrap(),
            id: other_id,
        };
        assert_eq!(state.apply_change_event(&other_event), AppEffect::Refresh);
    }

    #[test]
    fn search_modal_captures_typing_and_emits_search_effect() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        state.modal = Some(ModalKind::Search);

        let char_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let effect = state.handle_key(char_a);
        assert_eq!(effect, AppEffect::Search("a".to_owned()));
        assert_eq!(state.search_query, "a");

        let char_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE);
        let effect = state.handle_key(char_b);
        assert_eq!(effect, AppEffect::Search("ab".to_owned()));
        assert_eq!(state.search_query, "ab");

        let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        let effect = state.handle_key(backspace);
        assert_eq!(effect, AppEffect::Search("a".to_owned()));
        assert_eq!(state.search_query, "a");
    }

    #[test]
    fn accept_inbox_task_modal_flow_and_confirm() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        state.active_tab = QueueTab::Inbox;
        state.inbox = vec![make_task("k3q7zm2a", Status::Inbox, None)];
        state.inbox_selected = 0;

        // Press 'a' to open accept modal
        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let effect = state.handle_key(a);
        assert_eq!(effect, AppEffect::None);
        assert_eq!(state.modal, Some(ModalKind::Accept));
        let modal = state.accept_modal.as_ref().unwrap();
        assert_eq!(modal.field, AcceptField::Kind);
        assert_eq!(modal.kind, Kind::Bug);
        assert_eq!(modal.importance, Importance::P1);
        assert_eq!(modal.due_input, "2026-10-25");

        // Next option on Kind cycles kind and updates default importance to P2
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        state.handle_key(right);
        assert_eq!(state.accept_modal.as_ref().unwrap().kind, Kind::Story);
        assert_eq!(
            state.accept_modal.as_ref().unwrap().importance,
            Importance::P2
        );

        // Next field moves to Importance
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        state.handle_key(tab);
        assert_eq!(
            state.accept_modal.as_ref().unwrap().field,
            AcceptField::Importance
        );

        // Next option on Importance cycles importance from P2 to P3
        state.handle_key(right);
        assert_eq!(
            state.accept_modal.as_ref().unwrap().importance,
            Importance::P3
        );

        // Next field moves to Due
        state.handle_key(tab);
        assert_eq!(state.accept_modal.as_ref().unwrap().field, AcceptField::Due);

        // Backspace pops chars from due
        let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        for _ in 0..10 {
            state.handle_key(backspace);
        }
        assert_eq!(state.accept_modal.as_ref().unwrap().due_input, "");

        // Type "tomorrow"
        for c in "tomorrow".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(state.accept_modal.as_ref().unwrap().due_input, "tomorrow");

        // Enter confirms modal
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let effect = state.handle_key(enter);
        assert_eq!(state.modal, None);
        assert_eq!(state.accept_modal, None);
        assert_eq!(
            effect,
            AppEffect::AcceptTask {
                id: "k3q7zm2a".parse().unwrap(),
                kind: Some(Kind::Story),
                importance: Some(Importance::P3),
                due: Some(Some(jiff::civil::date(2026, 10, 23))),
            }
        );
    }

    #[test]
    fn accept_modal_invalid_date_keeps_modal_open_with_error() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        state.active_tab = QueueTab::Inbox;
        state.inbox = vec![make_task("k3q7zm2a", Status::Inbox, None)];
        state.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));

        let m = state.accept_modal.as_mut().unwrap();
        m.due_input = "invalid-date".to_owned();

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let effect = state.handle_key(enter);
        assert_eq!(effect, AppEffect::None);
        assert_eq!(state.modal, Some(ModalKind::Accept));
        assert!(state.accept_modal.as_ref().unwrap().error.is_some());
    }

    #[test]
    fn accept_refused_on_non_inbox_task() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        state.active_tab = QueueTab::Today;
        state.today = vec![make_task("k3q7zm2a", Status::Ready, None)];

        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let effect = state.handle_key(a);
        assert_eq!(effect, AppEffect::None);
        assert_eq!(state.modal, None);
        assert_eq!(
            state.status_message.as_deref(),
            Some("cannot accept: task is not in inbox")
        );
    }

    #[test]
    fn read_only_task_refuses_all_triage_actions_and_shows_why() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        state.active_tab = QueueTab::Inbox;
        state.inbox = vec![make_task(
            "k3q7zm2a",
            Status::Inbox,
            Some(TaskReadOnly::InvalidFile("line 1: bad syntax".to_owned())),
        )];

        let keys_to_test = ['a', 'z', 'x', 'c', 'g'];
        for ch in keys_to_test {
            let key = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
            let effect = state.handle_key(key);
            assert_eq!(effect, AppEffect::None, "key '{ch}' should be refused");
            assert_eq!(state.modal, None);
            assert_eq!(
                state.status_message.as_deref(),
                Some("task is read-only: invalid task.md: line 1: bad syntax")
            );
        }
    }

    #[test]
    fn snooze_flow_and_confirm() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        state.active_tab = QueueTab::Today;
        state.today = vec![make_task("k3q7zm2a", Status::Ready, None)];

        let z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
        let effect = state.handle_key(z);
        assert_eq!(effect, AppEffect::None);
        assert_eq!(state.modal, Some(ModalKind::Snooze));
        assert_eq!(
            state.snooze_modal.as_ref().unwrap().field,
            SnoozeField::Until
        );
        assert_eq!(state.snooze_modal.as_ref().unwrap().until_input, "tomorrow");

        // Switch to 'on' field
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        state.handle_key(tab);
        assert_eq!(state.snooze_modal.as_ref().unwrap().field, SnoozeField::On);

        // Type waiting on
        for c in "Alice".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(state.snooze_modal.as_ref().unwrap().on_input, "Alice");

        // Confirm
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let effect = state.handle_key(enter);
        assert_eq!(state.modal, None);
        assert_eq!(
            effect,
            AppEffect::SnoozeTask {
                id: "k3q7zm2a".parse().unwrap(),
                until: jiff::civil::date(2026, 10, 23),
                on: Some("Alice".to_owned()),
            }
        );
    }

    #[test]
    fn new_task_flow_and_confirm() {
        let mut state = AppState::new(
            test_theme(),
            Some(ContextName::from_str("work").unwrap()),
            vec![ContextName::from_str("work").unwrap()],
            test_date(),
        );

        let plus = KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE);
        let effect = state.handle_key(plus);
        assert_eq!(effect, AppEffect::None);
        assert_eq!(state.modal, Some(ModalKind::NewTask));
        assert_eq!(
            state.new_task_modal.as_ref().unwrap().context.as_str(),
            "work"
        );

        // Type title
        for c in "Fix bug".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(
            state.new_task_modal.as_ref().unwrap().title_input,
            "Fix bug"
        );

        // Confirm
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let effect = state.handle_key(enter);
        assert_eq!(state.modal, None);
        assert_eq!(
            effect,
            AppEffect::CreateTask {
                title: "Fix bug".to_owned(),
                context: ContextName::from_str("work").unwrap(),
            }
        );
    }

    #[test]
    fn direct_triage_actions_emit_effects() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        state.active_tab = QueueTab::Today;
        state.today = vec![make_task("k3q7zm2a", Status::Ready, None)];

        // 'g' emits GoTask
        let g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(
            state.handle_key(g),
            AppEffect::GoTask("k3q7zm2a".parse().unwrap())
        );

        // 'c' emits DoneTask
        let c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(
            state.handle_key(c),
            AppEffect::DoneTask("k3q7zm2a".parse().unwrap())
        );

        // 'x' emits CancelTask
        let x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(
            state.handle_key(x),
            AppEffect::CancelTask("k3q7zm2a".parse().unwrap())
        );

        // 'u' with recorded action emits UndoTask
        state.record_action_success("k3q7zm2a".parse().unwrap());
        let u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        assert_eq!(
            state.handle_key(u),
            AppEffect::UndoTask("k3q7zm2a".parse().unwrap())
        );
    }

    #[test]
    fn toggle_why_and_group_by_parent() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        assert!(!state.why_enabled());
        assert!(!state.group_by_parent());

        let w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE);
        state.handle_key(w);
        assert!(state.why_enabled());

        let p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        state.handle_key(p);
        assert!(state.group_by_parent());
    }

    #[test]
    fn undo_after_accept_restores_accepted_task_even_when_gone_from_queue() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        let task_id: TaskId = "k3q7zm2a".parse().unwrap();
        state.active_tab = QueueTab::Inbox;
        state.inbox = vec![
            make_task("k3q7zm2a", Status::Inbox, None),
            make_task("m9x2rt4b", Status::Inbox, None),
        ];
        state.inbox_selected = 0;

        // Open accept modal
        state.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        // Confirm accept modal
        let effect = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(effect, AppEffect::AcceptTask { ref id, .. } if id == &task_id));
        // Action has not succeeded yet: last_action is not set eagerly
        assert_eq!(state.last_action, None);

        // Simulated success callback from runtime
        state.record_action_success(task_id.clone());

        // Simulated refresh: the accepted task is removed from inbox, next task is now at index 0
        state.inbox = vec![make_task("m9x2rt4b", Status::Inbox, None)];
        state.inbox_selected = 0;

        // Press 'u': must undo the accepted task (k3q7zm2a), not the currently selected one (m9x2rt4b)
        let undo_effect = state.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(undo_effect, AppEffect::UndoTask(task_id));

        // Pressing 'u' a second time shows "nothing to undo" instead of acting on the selected task
        let second_undo = state.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(second_undo, AppEffect::None);
        assert_eq!(state.status_message.as_deref(), Some("nothing to undo"));
    }

    #[test]
    fn failed_action_does_not_set_last_action_and_shows_nothing_to_undo() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        state.active_tab = QueueTab::Inbox;
        state.inbox = vec![make_task("k3q7zm2a", Status::Inbox, None)];
        state.inbox_selected = 0;

        // Open and confirm accept modal
        state.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        let effect = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(effect, AppEffect::AcceptTask { .. }));

        // Action failed: record_action_success is NOT called
        assert_eq!(state.last_action, None);

        // Pressing 'u' refuses cleanly with "nothing to undo"
        let undo_effect = state.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(undo_effect, AppEffect::None);
        assert_eq!(state.status_message.as_deref(), Some("nothing to undo"));
    }

    #[test]
    fn cycling_kind_from_content_with_format() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        state.active_tab = QueueTab::Inbox;
        let mut task = make_task("k3q7zm2a", Status::Inbox, None);
        if let Some(t) = &mut task.task {
            t.kind = Kind::Content {
                format: Some(loom_api::ContentFormat::Talk),
            };
        }
        state.inbox = vec![task];
        state.inbox_selected = 0;

        // Open accept modal
        state.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(
            state.accept_modal.as_ref().unwrap().kind,
            Kind::Content {
                format: Some(loom_api::ContentFormat::Talk)
            }
        );

        // Next option: advances to Research (not back to start of list)
        state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(state.accept_modal.as_ref().unwrap().kind, Kind::Research);

        // Prev option: cycles back to Content { format: None }
        state.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(
            state.accept_modal.as_ref().unwrap().kind,
            Kind::Content { format: None }
        );

        // Prev option again: cycles back to Story
        state.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(state.accept_modal.as_ref().unwrap().kind, Kind::Story);
    }

    #[test]
    fn parent_grouping_groups_grandchildren_and_preserves_selection() {
        let mut state = AppState::new(test_theme(), None, Vec::new(), test_date());
        state.active_tab = QueueTab::Today;

        let root_id: TaskId = "task0001".parse().unwrap();
        let child_id: TaskId = "task0003".parse().unwrap();

        let mut root = make_task("task0001", Status::Ready, None);
        root.rank = Some(loom_api::TaskRank {
            position: 3,
            reason: RankReason::DueDate,
        });

        let mut other = make_task("task0002", Status::Ready, None);
        other.rank = Some(loom_api::TaskRank {
            position: 1,
            reason: RankReason::Urgent,
        });

        let mut child = make_task("task0003", Status::Ready, None);
        if let Some(t) = &mut child.task {
            t.relations.parent = Some(root_id);
        }
        child.rank = Some(loom_api::TaskRank {
            position: 2,
            reason: RankReason::Urgent,
        });

        let mut grand = make_task("task0004", Status::Ready, None);
        if let Some(t) = &mut grand.task {
            t.relations.parent = Some(child_id);
        }
        grand.rank = Some(loom_api::TaskRank {
            position: 4,
            reason: RankReason::First,
        });

        // Insert in arbitrary order: other, child, root, grand
        state.today = vec![other, child, root, grand];
        // Select child (index 1)
        state.today_selected = 1;

        // Group by parent
        state.sort_today_by_parent();

        // Expected order:
        // 1. Root task 'other' (rank 1, no children)
        // 2. Root task 'root' (rank 3), followed by child (task0003), followed by grandchild (task0004)
        assert_eq!(state.today[0].id.as_str(), "task0002");
        assert_eq!(state.today[1].id.as_str(), "task0001");
        assert_eq!(state.today[2].id.as_str(), "task0003");
        assert_eq!(state.today[3].id.as_str(), "task0004");

        // Selection must follow task0003 to its new index (2)
        assert_eq!(state.today_selected, 2);
        assert_eq!(state.today[state.today_selected].id.as_str(), "task0003");
    }

    #[test]
    fn new_task_uses_selected_task_context_or_default_context() {
        let mut state = AppState::new(
            test_theme(),
            None,
            vec!["alpha".parse().unwrap(), "work".parse().unwrap()],
            test_date(),
        );
        state.active_tab = QueueTab::Today;
        let mut task = make_task("k3q7zm2a", Status::Ready, None);
        task.context = "work".parse().unwrap();
        state.today = vec![task];
        state.today_selected = 0;

        // With [all] active, '+' uses the selected task's context ('work'), NOT contexts.first() ('alpha')
        state.handle_key(KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE));
        assert_eq!(
            state.new_task_modal.as_ref().unwrap().context,
            "work".parse::<ContextName>().unwrap()
        );
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        // When queue is empty and default_context is set, '+' uses default_context
        state.today.clear();
        state.default_context = Some("personal".parse().unwrap());
        state.handle_key(KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE));
        assert_eq!(
            state.new_task_modal.as_ref().unwrap().context,
            "personal".parse::<ContextName>().unwrap()
        );
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        // When queue is empty and no default_context, '+' refuses cleanly
        state.default_context = None;
        let effect = state.handle_key(KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE));
        assert_eq!(effect, AppEffect::None);
        assert_eq!(state.modal, None);
        assert!(state.status_message.is_some());
    }
}
