//! Pure application state and transitions of the TUI (ADR-0014).
//!
//! No I/O happens in this module: key events and SSE [`ChangeEvent`]s mutate [`AppState`] and
//! return an [`AppEffect`] that the runtime loop in the binary executes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use loom_api::{ChangeEvent, ContextName, SearchResult, TaskDetail, TaskId, TaskSummary};

use super::color::Theme;
use super::keymap::{KeyCommand, ModalKind, QueueTab, ViewKind, resolve_key};
use super::palette::{PaletteAction, PaletteItem, build_palette_items, filter_palette};

/// Minimum terminal width (in columns) required to display the right-hand preview pane.
pub const PREVIEW_MIN_WIDTH: u16 = 120;

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
            status_message: None,
            last_error: None,
            sse_state: SseState::Disconnected,
            should_quit: false,
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
        // Modal text input for `:` and `/` takes priority for character typing.
        if let Some(modal) = self.modal {
            match modal {
                ModalKind::Palette => {
                    if let KeyCode::Char(c) = key.code {
                        if !key.modifiers.contains(KeyModifiers::CONTROL) {
                            self.palette_query.push(c);
                            self.filtered_palette =
                                filter_palette(&self.palette_items, &self.palette_query);
                            self.palette_selected = 0;
                            return AppEffect::None;
                        }
                    } else if key.code == KeyCode::Backspace {
                        self.palette_query.pop();
                        self.filtered_palette =
                            filter_palette(&self.palette_items, &self.palette_query);
                        self.palette_selected = 0;
                        return AppEffect::None;
                    }
                }
                ModalKind::Search => {
                    if let KeyCode::Char(c) = key.code {
                        if !key.modifiers.contains(KeyModifiers::CONTROL) {
                            self.search_query.push(c);
                            return AppEffect::Search(self.search_query.clone());
                        }
                    } else if key.code == KeyCode::Backspace {
                        self.search_query.pop();
                        return AppEffect::Search(self.search_query.clone());
                    }
                }
                _ => {}
            }
        }

        let cmd = resolve_key(self.modal, self.active_view(), key);
        let Some(command) = cmd else {
            return AppEffect::None;
        };

        self.apply_command(command)
    }

    fn apply_command(&mut self, command: KeyCommand) -> AppEffect {
        match command {
            KeyCommand::Quit => {
                self.should_quit = true;
                AppEffect::None
            }
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
            KeyCommand::OpenPalette => {
                self.modal = Some(ModalKind::Palette);
                self.palette_query.clear();
                self.filtered_palette = filter_palette(&self.palette_items, "");
                self.palette_selected = 0;
                AppEffect::None
            }
            KeyCommand::OpenSearch => {
                self.modal = Some(ModalKind::Search);
                self.search_query.clear();
                self.search_results.clear();
                self.search_selected = 0;
                AppEffect::None
            }
            KeyCommand::OpenHelp => {
                self.modal = Some(ModalKind::Help);
                self.help_scroll = 0;
                AppEffect::None
            }
            KeyCommand::OpenContextPicker => {
                self.modal = Some(ModalKind::ContextPicker);
                self.context_picker_selected = 0;
                AppEffect::None
            }
            KeyCommand::CloseModal => {
                self.modal = None;
                AppEffect::None
            }
            KeyCommand::ConfirmModal => self.confirm_active_modal(),
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
                if self.preview_enabled {
                    self.update_preview_effect()
                } else {
                    AppEffect::None
                }
            }
            KeyCommand::Refresh => AppEffect::Refresh,
            KeyCommand::Back => {
                self.page_stack.pop();
                self.update_preview_effect()
            }
        }
    }

    fn confirm_active_modal(&mut self) -> AppEffect {
        let Some(modal) = self.modal.take() else {
            return AppEffect::None;
        };

        match modal {
            ModalKind::Palette => {
                if let Some((_, item)) = self.filtered_palette.get(self.palette_selected) {
                    match item.action.clone() {
                        PaletteAction::SelectTab(tab) => {
                            self.active_tab = tab;
                            self.update_preview_effect()
                        }
                        PaletteAction::OpenSearch => {
                            self.modal = Some(ModalKind::Search);
                            self.search_query.clear();
                            self.search_results.clear();
                            AppEffect::None
                        }
                        PaletteAction::OpenContextPicker => {
                            self.modal = Some(ModalKind::ContextPicker);
                            self.context_picker_selected = 0;
                            AppEffect::None
                        }
                        PaletteAction::SetContext(ctx) => {
                            self.active_context = ctx;
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
                } else {
                    AppEffect::None
                }
            }
            ModalKind::ContextPicker => {
                if self.context_picker_selected == 0 {
                    self.active_context = None;
                } else if let Some(ctx) = self.contexts.get(self.context_picker_selected - 1) {
                    self.active_context = Some(ctx.clone());
                }
                AppEffect::Refresh
            }
            ModalKind::Search => {
                if let Some(res) = self.search_results.get(self.search_selected) {
                    match res {
                        SearchResult::Task(task) => {
                            self.page_stack.push(ObjectPageState {
                                target: ObjectTarget::Task(task.id.clone()),
                                content: ObjectPageContent::Loading(ObjectTarget::Task(
                                    task.id.clone(),
                                )),
                                scroll: 0,
                            });
                            AppEffect::FetchTask(task.id.clone())
                        }
                        SearchResult::Repo(_) => AppEffect::None,
                    }
                } else {
                    AppEffect::None
                }
            }
            ModalKind::Help => AppEffect::None,
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
            }
            return;
        }

        if !self.page_stack.is_empty() {
            if let Some(page) = self.page_stack.last_mut() {
                page.scroll = page.scroll.saturating_add(1);
            }
            return;
        }

        let len = self.current_queue_len();
        if len > 0 {
            let curr = self.current_selected();
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
            }
            return;
        }

        if !self.page_stack.is_empty() {
            if let Some(page) = self.page_stack.last_mut() {
                page.scroll = page.scroll.saturating_sub(1);
            }
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
            ChangeEvent::TaskChanged { context, id }
            | ChangeEvent::TaskArchived { context, id } => {
                if self.active_context.as_ref().is_some_and(|c| c != context) {
                    return AppEffect::None;
                }
                // Refresh if the changed task is currently viewed, or refresh queues.
                if let Some(page) = self.page_stack.last() {
                    let ObjectTarget::Task(page_id) = &page.target;
                    if page_id == id {
                        return AppEffect::FetchTask(id.clone());
                    }
                }
                AppEffect::Refresh
            }
            ChangeEvent::CountsChanged { context, .. } => {
                if self.active_context.as_ref().is_some_and(|c| c != context) {
                    return AppEffect::None;
                }
                AppEffect::Refresh
            }
            ChangeEvent::RepoChanged { .. } | ChangeEvent::Resync => AppEffect::Refresh,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn test_theme() -> Theme {
        Theme::from_config(&BTreeMap::new(), false)
    }

    #[test]
    fn initial_state_has_three_tabs_and_empty_queues() {
        let state = AppState::new(test_theme(), None, Vec::new());
        assert_eq!(state.active_tab, QueueTab::Today);
        assert_eq!(state.current_queue_len(), 0);
        assert!(!state.should_quit);
    }

    #[test]
    fn quit_key_sets_should_quit() {
        let mut state = AppState::new(test_theme(), None, Vec::new());
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(state.handle_key(q), AppEffect::None);
        assert!(state.should_quit);
    }

    #[test]
    fn cycling_tabs_preserves_selection_bounds() {
        let mut state = AppState::new(test_theme(), None, Vec::new());
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);

        state.handle_key(tab);
        assert_eq!(state.active_tab, QueueTab::Inbox);

        state.handle_key(tab);
        assert_eq!(state.active_tab, QueueTab::Waiting);

        state.handle_key(tab);
        assert_eq!(state.active_tab, QueueTab::Today);
    }

    #[test]
    fn error_state_can_be_set_and_cleared() {
        let mut state = AppState::new(test_theme(), None, Vec::new());
        assert!(state.last_error.is_none());

        state.set_error("daemon connection lost");
        assert_eq!(state.last_error.as_deref(), Some("daemon connection lost"));

        state.clear_error();
        assert!(state.last_error.is_none());
    }
}
