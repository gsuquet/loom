//! Pure application state and transitions of the TUI (ADR-0014).
//!
//! No I/O happens in this module: key events and SSE [`ChangeEvent`]s mutate [`AppState`] and
//! return an [`AppEffect`] that the runtime loop in the binary executes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use loom_api::{
    ChangeEvent, ContextName, RepoSummary, SearchResult, TaskDetail, TaskId, TaskSummary,
};

use crate::tui::color::Theme;
use crate::tui::keymap::{KeyCommand, ModalKind, QueueTab, ViewKind, resolve_key};
use crate::tui::palette::{PaletteAction, PaletteItem, build_palette_items, filter_palette};

/// Minimum terminal width (in columns) required to display the right-hand preview pane.
pub const PREVIEW_MIN_WIDTH: u16 = 120;

/// Connection state of the `GET /v1/events` SSE stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseState {
    /// Connected to `loomd` and receiving events.
    Connected,
    /// Disconnected; waiting before reconnecting with `Last-Event-ID`.
    Reconnecting,
}

impl SseState {
    /// Short badge shown in the top bar.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connected => "● live",
            Self::Reconnecting => "◌ reconnecting",
        }
    }
}

/// Identifier of an object that can be opened as a full-screen object page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectTarget {
    /// A task by ID.
    Task(TaskId),
    /// A repository by `<context>/<name>` slug.
    Repo(String),
}

/// Content of an open object page on the view stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectPageContent {
    /// Task detail page.
    Task(Box<TaskDetail>),
    /// Repository detail page.
    Repo(Box<RepoSummary>),
    /// Placeholder while `loom` fetches the object from `loomd`.
    Loading(ObjectTarget),
}

/// State of one pushed object page on the navigation stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPageState {
    /// Target object of the page.
    pub target: ObjectTarget,
    /// Loaded content (or `Loading`).
    pub content: ObjectPageContent,
    /// Vertical scroll offset in lines.
    pub scroll: u16,
}

/// State of the active modal overlay.
#[derive(Debug, Clone, PartialEq)]
pub enum ModalState {
    /// `?` key bindings help modal.
    Help {
        /// Vertical scroll offset.
        scroll: u16,
    },
    /// `:` fuzzy command palette modal.
    Palette {
        /// Current filter text.
        query: String,
        /// Selected index in the filtered candidate list.
        selected: usize,
    },
    /// `/` live search modal over `GET /v1/search`.
    Search {
        /// Current search query text.
        query: String,
        /// Latest results from `loomd`.
        results: Vec<SearchResult>,
        /// Selected index in `results`.
        selected: usize,
    },
    /// `@` context filter picker modal.
    ContextPicker {
        /// Selected index (`0` = all contexts, `1..=N` = `contexts[i - 1]`).
        selected: usize,
    },
}

impl ModalState {
    /// The [`ModalKind`] of this modal state.
    #[must_use]
    pub const fn kind(&self) -> ModalKind {
        match self {
            Self::Help { .. } => ModalKind::Help,
            Self::Palette { .. } => ModalKind::Palette,
            Self::Search { .. } => ModalKind::Search,
            Self::ContextPicker { .. } => ModalKind::ContextPicker,
        }
    }
}

/// Side-effect requested by a pure state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEffect {
    /// No I/O needed; only re-render.
    None,
    /// Exit the TUI cleanly.
    Quit,
    /// Refetch queues (and the active object page / preview if any) from `loomd`.
    RefreshData,
    /// Fetch `/v1/tasks/{id}` for the right-hand queue preview pane.
    FetchPreviewTask(TaskId),
    /// Fetch `/v1/tasks/{id}` or `/v1/repos/{context}/{name}` for an object page.
    FetchObject(ObjectTarget),
    /// Run `GET /v1/search` for the `/` search modal.
    Search(String),
}

/// Complete state of the TUI.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Active queue tab.
    pub tab: QueueTab,
    /// Active context filter (`None` = all contexts).
    pub context_filter: Option<ContextName>,
    /// Known context names (from `config.toml` and discovered tasks/repos), sorted and unique.
    pub contexts: Vec<ContextName>,
    /// ANSI color theme.
    pub theme: Theme,
    /// Tasks in the Today (priority) queue.
    pub today: Vec<TaskSummary>,
    /// Tasks in the Inbox queue.
    pub inbox: Vec<TaskSummary>,
    /// Tasks in the Waiting queue.
    pub waiting: Vec<TaskSummary>,
    /// Repositories in the Repos queue.
    pub repos: Vec<RepoSummary>,
    /// Selected row index per [`QueueTab`].
    pub selected_by_tab: [usize; 4],
    /// Whether the user has the right-hand preview pane enabled (`|` toggles this).
    pub preview_enabled: bool,
    /// Cached [`TaskDetail`] for the currently selected task in the queue preview pane.
    pub preview_task: Option<TaskDetail>,
    /// Stack of open object pages (`Enter` pushes, `Esc`/`Backspace` pops).
    pub object_stack: Vec<ObjectPageState>,
    /// Active modal overlay (`None` when no modal is open).
    pub modal: Option<ModalState>,
    /// SSE connection indicator.
    pub sse_state: SseState,
    /// Optional status/error banner shown in the footer.
    pub status_message: Option<String>,
    /// Whether the user has requested to quit.
    pub should_quit: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new(Theme::default(), None, Vec::new())
    }
}

impl AppState {
    /// Creates an initial [`AppState`].
    #[must_use]
    pub const fn new(
        theme: Theme,
        context_filter: Option<ContextName>,
        contexts: Vec<ContextName>,
    ) -> Self {
        Self {
            tab: QueueTab::Today,
            context_filter,
            contexts,
            theme,
            today: Vec::new(),
            inbox: Vec::new(),
            waiting: Vec::new(),
            repos: Vec::new(),
            selected_by_tab: [0; 4],
            preview_enabled: true,
            preview_task: None,
            object_stack: Vec::new(),
            modal: None,
            sse_state: SseState::Connected,
            status_message: None,
            should_quit: false,
        }
    }

    /// Active [`ViewKind`] underneath any modal.
    #[must_use]
    pub const fn active_view(&self) -> ViewKind {
        if self.object_stack.is_empty() {
            ViewKind::Queue
        } else {
            ViewKind::ObjectPage
        }
    }

    /// Active [`ModalKind`] if a modal is open.
    #[must_use]
    pub const fn active_modal(&self) -> Option<ModalKind> {
        match &self.modal {
            Some(modal) => Some(modal.kind()),
            None => None,
        }
    }

    /// Whether the right-hand preview pane should be drawn for `terminal_width`.
    #[must_use]
    pub const fn show_preview(&self, terminal_width: u16) -> bool {
        self.preview_enabled && terminal_width >= PREVIEW_MIN_WIDTH
    }

    /// Item count of `tab`.
    #[must_use]
    pub const fn tab_count(&self, tab: QueueTab) -> usize {
        match tab {
            QueueTab::Today => self.today.len(),
            QueueTab::Inbox => self.inbox.len(),
            QueueTab::Waiting => self.waiting.len(),
            QueueTab::Repos => self.repos.len(),
        }
    }

    /// Length of the currently active queue tab.
    #[must_use]
    pub const fn active_queue_len(&self) -> usize {
        self.tab_count(self.tab)
    }

    /// Selected row index in `tab`.
    #[must_use]
    pub const fn selected_index(&self, tab: QueueTab) -> usize {
        self.selected_by_tab[Self::tab_slot(tab)]
    }

    /// Selected row index in the active queue tab.
    #[must_use]
    pub const fn active_selected_index(&self) -> usize {
        self.selected_index(self.tab)
    }

    const fn tab_slot(tab: QueueTab) -> usize {
        match tab {
            QueueTab::Today => 0,
            QueueTab::Inbox => 1,
            QueueTab::Waiting => 2,
            QueueTab::Repos => 3,
        }
    }

    /// Currently selected task in the active queue (when `tab` is a task queue).
    #[must_use]
    pub fn selected_task(&self) -> Option<&TaskSummary> {
        let idx = self.active_selected_index();
        match self.tab {
            QueueTab::Today => self.today.get(idx),
            QueueTab::Inbox => self.inbox.get(idx),
            QueueTab::Waiting => self.waiting.get(idx),
            QueueTab::Repos => None,
        }
    }

    /// Currently selected repository in the active queue (when `tab` is `QueueTab::Repos`).
    #[must_use]
    pub fn selected_repo(&self) -> Option<&RepoSummary> {
        let idx = self.active_selected_index();
        match self.tab {
            QueueTab::Repos => self.repos.get(idx),
            QueueTab::Today | QueueTab::Inbox | QueueTab::Waiting => None,
        }
    }

    /// Updates the loaded queue data, clamping selections and collecting any new context names.
    pub fn set_queues(
        &mut self,
        today: Vec<TaskSummary>,
        inbox: Vec<TaskSummary>,
        waiting: Vec<TaskSummary>,
        repos: Vec<RepoSummary>,
    ) {
        for task in today.iter().chain(inbox.iter()).chain(waiting.iter()) {
            self.register_context(&task.context);
        }
        for repo in &repos {
            if let Some(ctx) = repo.identity.context.context() {
                self.register_context(ctx);
            }
        }
        self.today = today;
        self.inbox = inbox;
        self.waiting = waiting;
        self.repos = repos;
        for tab in QueueTab::ALL {
            self.clamp_selection(tab);
        }
    }

    fn register_context(&mut self, context: &ContextName) {
        if !self.contexts.contains(context) {
            self.contexts.push(context.clone());
            self.contexts.sort();
        }
    }

    const fn clamp_selection(&mut self, tab: QueueTab) {
        let len = self.tab_count(tab);
        let slot = Self::tab_slot(tab);
        if len == 0 {
            self.selected_by_tab[slot] = 0;
        } else if self.selected_by_tab[slot] >= len {
            self.selected_by_tab[slot] = len - 1;
        }
    }

    /// Supplies a fetched [`TaskDetail`] for the preview pane and any matching open object page.
    pub fn set_task_detail(&mut self, detail: TaskDetail) {
        self.register_context(&detail.summary.context);
        for page in &mut self.object_stack {
            if page.target == ObjectTarget::Task(detail.summary.id.clone()) {
                page.content = ObjectPageContent::Task(Box::new(detail.clone()));
            }
        }
        if self
            .selected_task()
            .is_some_and(|selected| selected.id == detail.summary.id)
        {
            self.preview_task = Some(detail);
        }
    }

    /// Supplies a fetched [`RepoSummary`] for any matching open object page.
    pub fn set_repo_detail(&mut self, repo: &RepoSummary) {
        let target = ObjectTarget::Repo(repo.identity.slug.clone());
        for page in &mut self.object_stack {
            if page.target == target {
                page.content = ObjectPageContent::Repo(Box::new(repo.clone()));
            }
        }
    }

    /// Supplies search results for the open `/` search modal.
    pub fn set_search_results(&mut self, results: Vec<SearchResult>) {
        if let Some(ModalState::Search {
            results: modal_results,
            selected,
            ..
        }) = &mut self.modal
        {
            *modal_results = results;
            if modal_results.is_empty() {
                *selected = 0;
            } else if *selected >= modal_results.len() {
                *selected = modal_results.len() - 1;
            }
        }
    }

    /// Filtered command palette items when the palette modal is open.
    #[must_use]
    pub fn palette_candidates(&self) -> Vec<PaletteItem> {
        let all = build_palette_items(&self.contexts);
        if let Some(ModalState::Palette { query, .. }) = &self.modal {
            filter_palette(&all, query)
        } else {
            all
        }
    }

    /// Handles a key event and returns the resulting [`AppEffect`].
    pub fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        self.status_message = None;

        // First let open text-input modals consume printable character input before command lookup.
        if let Some(effect) = self.handle_modal_text_input(key) {
            return effect;
        }

        let Some(command) = resolve_key(self.active_view(), self.active_modal(), key) else {
            return AppEffect::None;
        };

        self.apply_command(command)
    }

    fn handle_modal_text_input(&mut self, key: KeyEvent) -> Option<AppEffect> {
        let ctrl_or_alt = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if ctrl_or_alt {
            return None;
        }
        match &mut self.modal {
            Some(ModalState::Palette { query, selected }) => {
                if let KeyCode::Char(ch) = key.code {
                    query.push(ch);
                    *selected = 0;
                    return Some(AppEffect::None);
                }
                None
            }
            Some(ModalState::Search {
                query,
                results,
                selected,
            }) => {
                if let KeyCode::Char(ch) = key.code {
                    query.push(ch);
                    *selected = 0;
                    let trimmed = query.trim().to_owned();
                    if trimmed.is_empty() {
                        results.clear();
                        return Some(AppEffect::None);
                    }
                    return Some(AppEffect::Search(trimmed));
                }
                None
            }
            Some(ModalState::Help { .. } | ModalState::ContextPicker { .. }) | None => None,
        }
    }

    fn apply_command(&mut self, command: KeyCommand) -> AppEffect {
        match command {
            KeyCommand::Quit => {
                self.should_quit = true;
                AppEffect::Quit
            }
            KeyCommand::OpenHelp => {
                self.modal = Some(ModalState::Help { scroll: 0 });
                AppEffect::None
            }
            KeyCommand::OpenPalette => {
                self.modal = Some(ModalState::Palette {
                    query: String::new(),
                    selected: 0,
                });
                AppEffect::None
            }
            KeyCommand::OpenSearch => {
                self.modal = Some(ModalState::Search {
                    query: String::new(),
                    results: Vec::new(),
                    selected: 0,
                });
                AppEffect::None
            }
            KeyCommand::OpenContextPicker => {
                let selected = self
                    .context_filter
                    .as_ref()
                    .and_then(|active| self.contexts.iter().position(|c| c == active))
                    .map_or(0, |idx| idx + 1);
                self.modal = Some(ModalState::ContextPicker { selected });
                AppEffect::None
            }
            KeyCommand::SelectTab(tab) => {
                self.object_stack.clear();
                self.tab = tab;
                self.sync_preview_effect()
            }
            KeyCommand::NextTab => {
                self.object_stack.clear();
                self.tab = self.tab.next();
                self.sync_preview_effect()
            }
            KeyCommand::PrevTab => {
                self.object_stack.clear();
                self.tab = self.tab.prev();
                self.sync_preview_effect()
            }
            KeyCommand::MoveDown => self.move_down(),
            KeyCommand::MoveUp => self.move_up(),
            KeyCommand::MoveFirst => self.move_first(),
            KeyCommand::MoveLast => self.move_last(),
            KeyCommand::OpenSelected => self.open_selected_from_queue(),
            KeyCommand::Back => {
                self.object_stack.pop();
                self.sync_preview_effect()
            }
            KeyCommand::TogglePreview => {
                self.preview_enabled = !self.preview_enabled;
                self.sync_preview_effect()
            }
            KeyCommand::Refresh => AppEffect::RefreshData,
            KeyCommand::CloseModal => {
                self.modal = None;
                AppEffect::None
            }
            KeyCommand::ConfirmModal => self.confirm_modal(),
            KeyCommand::DeleteChar => self.delete_modal_char(),
        }
    }

    fn move_down(&mut self) -> AppEffect {
        if let Some(modal) = &mut self.modal {
            match modal {
                ModalState::Help { scroll } => {
                    *scroll = scroll.saturating_add(1);
                }
                ModalState::Palette { .. } => {
                    let _ = modal;
                    let len = self.palette_candidates().len();
                    if let Some(ModalState::Palette { selected, .. }) = &mut self.modal
                        && len > 0
                    {
                        *selected = (*selected + 1) % len;
                    }
                }
                ModalState::Search {
                    results, selected, ..
                } => {
                    if !results.is_empty() {
                        *selected = (*selected + 1) % results.len();
                    }
                }
                ModalState::ContextPicker { selected } => {
                    let total = self.contexts.len() + 1;
                    *selected = (*selected + 1) % total;
                }
            }
            return AppEffect::None;
        }

        if let Some(page) = self.object_stack.last_mut() {
            page.scroll = page.scroll.saturating_add(1);
            return AppEffect::None;
        }

        let len = self.active_queue_len();
        if len > 0 {
            let slot = Self::tab_slot(self.tab);
            self.selected_by_tab[slot] = (self.selected_by_tab[slot] + 1).min(len - 1);
            return self.sync_preview_effect();
        }
        AppEffect::None
    }

    fn move_up(&mut self) -> AppEffect {
        if let Some(modal) = &mut self.modal {
            match modal {
                ModalState::Help { scroll } => {
                    *scroll = scroll.saturating_sub(1);
                }
                ModalState::Palette { .. } => {
                    let _ = modal;
                    let len = self.palette_candidates().len();
                    if let Some(ModalState::Palette { selected, .. }) = &mut self.modal
                        && len > 0
                    {
                        *selected = if *selected == 0 {
                            len - 1
                        } else {
                            *selected - 1
                        };
                    }
                }
                ModalState::Search {
                    results, selected, ..
                } => {
                    if !results.is_empty() {
                        *selected = if *selected == 0 {
                            results.len() - 1
                        } else {
                            *selected - 1
                        };
                    }
                }
                ModalState::ContextPicker { selected } => {
                    let total = self.contexts.len() + 1;
                    *selected = if *selected == 0 {
                        total - 1
                    } else {
                        *selected - 1
                    };
                }
            }
            return AppEffect::None;
        }

        if let Some(page) = self.object_stack.last_mut() {
            page.scroll = page.scroll.saturating_sub(1);
            return AppEffect::None;
        }

        let slot = Self::tab_slot(self.tab);
        if self.selected_by_tab[slot] > 0 {
            self.selected_by_tab[slot] -= 1;
            return self.sync_preview_effect();
        }
        AppEffect::None
    }

    fn move_first(&mut self) -> AppEffect {
        if let Some(page) = self.object_stack.last_mut() {
            page.scroll = 0;
            return AppEffect::None;
        }
        let slot = Self::tab_slot(self.tab);
        self.selected_by_tab[slot] = 0;
        self.sync_preview_effect()
    }

    fn move_last(&mut self) -> AppEffect {
        if let Some(page) = self.object_stack.last_mut() {
            page.scroll = 1000;
            return AppEffect::None;
        }
        let len = self.active_queue_len();
        if len > 0 {
            let slot = Self::tab_slot(self.tab);
            self.selected_by_tab[slot] = len - 1;
            return self.sync_preview_effect();
        }
        AppEffect::None
    }

    fn open_selected_from_queue(&mut self) -> AppEffect {
        if let Some(task) = self.selected_task().cloned() {
            let target = ObjectTarget::Task(task.id.clone());
            let content = self
                .preview_task
                .as_ref()
                .filter(|detail| detail.summary.id == task.id)
                .map_or_else(
                    || ObjectPageContent::Loading(target.clone()),
                    |detail| ObjectPageContent::Task(Box::new(detail.clone())),
                );
            self.object_stack.push(ObjectPageState {
                target: target.clone(),
                content,
                scroll: 0,
            });
            return AppEffect::FetchObject(target);
        }
        if let Some(repo) = self.selected_repo().cloned() {
            let target = ObjectTarget::Repo(repo.identity.slug.clone());
            self.object_stack.push(ObjectPageState {
                target: target.clone(),
                content: ObjectPageContent::Repo(Box::new(repo)),
                scroll: 0,
            });
            return AppEffect::FetchObject(target);
        }
        AppEffect::None
    }

    fn confirm_modal(&mut self) -> AppEffect {
        let Some(modal) = self.modal.take() else {
            return AppEffect::None;
        };
        match modal {
            ModalState::Help { .. } => AppEffect::None,
            ModalState::Palette { query, selected } => {
                let all = build_palette_items(&self.contexts);
                let candidates = filter_palette(&all, &query);
                let Some(item) = candidates.get(selected) else {
                    return AppEffect::None;
                };
                self.execute_palette_action(item.action.clone())
            }
            ModalState::Search {
                results, selected, ..
            } => {
                let Some(result) = results.get(selected) else {
                    return AppEffect::None;
                };
                match result {
                    SearchResult::Task(task_match) => {
                        let target = ObjectTarget::Task(task_match.id.clone());
                        self.object_stack.push(ObjectPageState {
                            target: target.clone(),
                            content: ObjectPageContent::Loading(target.clone()),
                            scroll: 0,
                        });
                        AppEffect::FetchObject(target)
                    }
                    SearchResult::Repo(repo_match) => {
                        let target = ObjectTarget::Repo(repo_match.slug.clone());
                        self.object_stack.push(ObjectPageState {
                            target: target.clone(),
                            content: ObjectPageContent::Loading(target.clone()),
                            scroll: 0,
                        });
                        AppEffect::FetchObject(target)
                    }
                }
            }
            ModalState::ContextPicker { selected } => {
                if selected == 0 {
                    self.context_filter = None;
                } else if let Some(ctx) = self.contexts.get(selected - 1) {
                    self.context_filter = Some(ctx.clone());
                }
                AppEffect::RefreshData
            }
        }
    }

    fn execute_palette_action(&mut self, action: PaletteAction) -> AppEffect {
        match action {
            PaletteAction::SwitchView(tab) => {
                self.object_stack.clear();
                self.tab = tab;
                self.sync_preview_effect()
            }
            PaletteAction::FilterContext(ctx) => {
                self.context_filter = ctx;
                AppEffect::RefreshData
            }
            PaletteAction::TogglePreview => {
                self.preview_enabled = !self.preview_enabled;
                self.sync_preview_effect()
            }
            PaletteAction::Refresh => AppEffect::RefreshData,
            PaletteAction::OpenHelp => {
                self.modal = Some(ModalState::Help { scroll: 0 });
                AppEffect::None
            }
            PaletteAction::OpenSearch => {
                self.modal = Some(ModalState::Search {
                    query: String::new(),
                    results: Vec::new(),
                    selected: 0,
                });
                AppEffect::None
            }
            PaletteAction::Quit => {
                self.should_quit = true;
                AppEffect::Quit
            }
        }
    }

    fn delete_modal_char(&mut self) -> AppEffect {
        match &mut self.modal {
            Some(ModalState::Palette { query, selected }) => {
                query.pop();
                *selected = 0;
                AppEffect::None
            }
            Some(ModalState::Search {
                query,
                results,
                selected,
            }) => {
                query.pop();
                *selected = 0;
                let trimmed = query.trim().to_owned();
                if trimmed.is_empty() {
                    results.clear();
                    AppEffect::None
                } else {
                    AppEffect::Search(trimmed)
                }
            }
            Some(ModalState::Help { .. } | ModalState::ContextPicker { .. }) | None => {
                AppEffect::None
            }
        }
    }

    /// Returns `AppEffect::FetchPreviewTask` if the currently selected queue item is a task whose
    /// detail is not yet cached in `preview_task`.
    pub fn sync_preview_effect(&mut self) -> AppEffect {
        let Some(selected) = self.selected_task() else {
            self.preview_task = None;
            return AppEffect::None;
        };
        if self
            .preview_task
            .as_ref()
            .is_some_and(|detail| detail.summary.id == selected.id)
        {
            return AppEffect::None;
        }
        let id = selected.id.clone();
        self.preview_task = None;
        AppEffect::FetchPreviewTask(id)
    }

    /// Handles a [`ChangeEvent`] from `GET /v1/events` and returns the required [`AppEffect`].
    pub fn apply_change_event(&mut self, event: &ChangeEvent) -> AppEffect {
        self.sse_state = SseState::Connected;
        if !event.concerns(self.context_filter.as_ref()) {
            return AppEffect::None;
        }
        match event {
            ChangeEvent::TaskChanged { .. }
            | ChangeEvent::TaskArchived { .. }
            | ChangeEvent::RepoChanged { .. }
            | ChangeEvent::CountsChanged { .. }
            | ChangeEvent::Resync => AppEffect::RefreshData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_api::{Importance, Kind, Relations, Status, Task, Timestamp};

    fn sample_task(id: &str, ctx: &str, title: &str) -> TaskSummary {
        let id_parsed: TaskId = id.parse().unwrap();
        let ctx_parsed: ContextName = ctx.parse().unwrap();
        let ts = Timestamp::from_unix_secs(1_791_792_000).unwrap();
        TaskSummary {
            id: id_parsed.clone(),
            prefix: id[..4].to_owned(),
            context: ctx_parsed.clone(),
            task: Some(Task {
                id: id_parsed,
                context: ctx_parsed,
                title: title.to_owned(),
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
        }
    }

    #[test]
    fn preview_pane_hides_below_120_columns_or_when_toggled_off() {
        let mut state = AppState::default();
        assert!(state.show_preview(120));
        assert!(state.show_preview(160));
        assert!(!state.show_preview(119));
        assert!(!state.show_preview(80));

        let pipe = KeyEvent::new(KeyCode::Char('|'), KeyModifiers::NONE);
        state.handle_key(pipe);
        assert!(!state.show_preview(140));
        state.handle_key(pipe);
        assert!(state.show_preview(140));
    }

    #[test]
    fn navigation_opens_object_page_and_returns_with_selection_intact() {
        let mut state = AppState::default();
        state.set_queues(
            vec![
                sample_task("k3q7zm2a", "company-a", "First task"),
                sample_task("h8w2xp9b", "personal", "Second task"),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        let down = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

        assert_eq!(
            state.handle_key(down),
            AppEffect::FetchPreviewTask("h8w2xp9b".parse().unwrap())
        );
        assert_eq!(state.active_selected_index(), 1);

        assert_eq!(
            state.handle_key(enter),
            AppEffect::FetchObject(ObjectTarget::Task("h8w2xp9b".parse().unwrap()))
        );
        assert_eq!(state.active_view(), ViewKind::ObjectPage);

        state.handle_key(esc);
        assert_eq!(state.active_view(), ViewKind::Queue);
        assert_eq!(state.active_selected_index(), 1);
    }

    #[test]
    fn sse_change_events_trigger_refresh_for_matching_context() {
        let mut state = AppState {
            context_filter: Some("company-a".parse().unwrap()),
            ..Default::default()
        };

        let other = ChangeEvent::TaskChanged {
            id: "k3q7zm2a".parse().unwrap(),
            context: "personal".parse().unwrap(),
        };
        assert_eq!(state.apply_change_event(&other), AppEffect::None);

        let matching = ChangeEvent::TaskChanged {
            id: "k3q7zm2a".parse().unwrap(),
            context: "company-a".parse().unwrap(),
        };
        assert_eq!(state.apply_change_event(&matching), AppEffect::RefreshData);
        assert_eq!(
            state.apply_change_event(&ChangeEvent::Resync),
            AppEffect::RefreshData
        );
    }
}
