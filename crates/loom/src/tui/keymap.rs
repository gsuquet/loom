//! Layered keymap and generated `?` help (ADR-0014).
//!
//! Keys are resolved from the innermost active layer outward:
//! 1. An open modal (`?` help, `:` command palette, `/` search, `@` context picker) captures keys
//!    first. While a modal is open, unmapped keys are dropped and never fall through to views.
//! 2. Otherwise the active view (Queue or Object page) handles view-specific keys, then falls
//!    back to the global layer.
//!
//! A single [`BINDINGS`] table defines both the key matching logic and the help entries,
//! ensuring they never drift.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::events::EventsFilter;

/// Which queue tab is displayed in the queue view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueueTab {
    /// Actionable tasks that are `ready` or `active`.
    Today,
    /// Unsorted incoming tasks (`inbox`).
    Inbox,
    /// Blocked or deferred tasks (`waiting`).
    Waiting,
}

impl QueueTab {
    /// All active queue tabs in display order.
    pub const ALL: [Self; 3] = [Self::Today, Self::Inbox, Self::Waiting];

    /// Display label of the tab.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Inbox => "Inbox",
            Self::Waiting => "Waiting",
        }
    }

    /// Single-digit key shortcut (`1`..`3`).
    #[must_use]
    pub const fn shortcut(self) -> char {
        match self {
            Self::Today => '1',
            Self::Inbox => '2',
            Self::Waiting => '3',
        }
    }

    /// Next queue tab in cyclic order.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Today => Self::Inbox,
            Self::Inbox => Self::Waiting,
            Self::Waiting => Self::Today,
        }
    }

    /// Previous queue tab in cyclic order.
    #[must_use]
    pub const fn prev(self) -> Self {
        match self {
            Self::Today => Self::Waiting,
            Self::Inbox => Self::Today,
            Self::Waiting => Self::Inbox,
        }
    }
}

/// Which screen view is active below any modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewKind {
    /// A queue list (Today, Inbox, Waiting) with an optional right-hand preview pane.
    Queue,
    /// A full-screen object page opened from a queue or search result with `Enter`.
    ObjectPage,
}

impl ViewKind {
    /// All view kinds.
    pub const ALL: [Self; 2] = [Self::Queue, Self::ObjectPage];

    /// Display title of the view layer in `?` help.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Queue => "Queue",
            Self::ObjectPage => "Object page",
        }
    }
}

/// Which modal overlay is open on top of the active view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModalKind {
    /// `?` key bindings help.
    Help,
    /// `:` fuzzy command palette.
    Palette,
    /// `/` live search over `/v1/search`.
    Search,
    /// `@` context filter picker.
    ContextPicker,
    /// `a` accept modal.
    Accept,
    /// `z` snooze modal.
    Snooze,
    /// `+` new task modal.
    NewTask,
    /// `t`, `i`, `d` field picker of the task page.
    Pick,
}

impl ModalKind {
    /// All modal kinds.
    pub const ALL: [Self; 8] = [
        Self::Help,
        Self::Palette,
        Self::Search,
        Self::ContextPicker,
        Self::Accept,
        Self::Snooze,
        Self::NewTask,
        Self::Pick,
    ];

    /// Display title of the modal layer in `?` help.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Help => "Help modal",
            Self::Palette => "Command palette",
            Self::Search => "Search modal",
            Self::ContextPicker => "Context picker",
            Self::Accept => "Accept task",
            Self::Snooze => "Snooze task",
            Self::NewTask => "New task",
            Self::Pick => "Change field",
        }
    }
}

/// A task field that the task page edits with a picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickField {
    /// The kind of the task.
    Kind,
    /// The importance of the task.
    Importance,
    /// The due date of the task.
    Due,
    /// The status of the task.
    Status,
    /// The context of the task.
    Context,
}

impl PickField {
    /// Name of the field in the title of the picker.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Kind => "kind",
            Self::Importance => "importance",
            Self::Due => "due date",
            Self::Status => "status",
            Self::Context => "context",
        }
    }
}

/// Which resolution layer handles a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    /// Modal overlay.
    Modal(ModalKind),
    /// Active view under any modal.
    View(ViewKind),
    /// Global shortcuts active everywhere unless overridden by a modal.
    Global,
}

/// Semantic command produced by key resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCommand {
    // Global
    Quit,
    SelectTab(QueueTab),
    NextTab,
    PrevTab,
    OpenPalette,
    OpenSearch,
    OpenHelp,
    OpenContextPicker,
    // Navigation
    MoveDown,
    MoveUp,
    MoveFirst,
    MoveLast,
    PageDown,
    PageUp,
    // Queue actions
    OpenSelected,
    TogglePreview,
    Refresh,
    // Triage actions
    TriageAccept,
    TriageSnooze,
    TriageCancel,
    TriageDone,
    TriageGo,
    Undo,
    NewTask,
    ToggleWhy,
    ToggleGroupByParent,
    // Object page actions
    OpenPicker(PickField),
    /// Edit the task file in the editor.
    EditBody,
    /// Cycle a filter of the Events tab.
    CycleEventsFilter(EventsFilter),
    Back,
    NextPageTab,
    PrevPageTab,
    // Modal actions
    CloseModal,
    ConfirmModal,
    NextField,
    PrevField,
    PrevOption,
    NextOption,
}

/// One declarative key binding definition used for both key matching and `?` help generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBinding {
    /// The layer this binding is registered in.
    pub layer: Layer,
    /// Key combinations that trigger this command.
    pub keys: &'static [(KeyCode, KeyModifiers)],
    /// Resulting command.
    pub command: KeyCommand,
    /// Display label for `?` help (e.g. `q, Ctrl+c`, `j, ↓`).
    pub label: &'static str,
    /// Human-readable explanation.
    pub description: &'static str,
}

/// Canonical key bindings table for the entire TUI. Single source of truth.
pub static BINDINGS: &[KeyBinding] = &[
    // Global layer
    KeyBinding {
        layer: Layer::Global,
        keys: &[
            (KeyCode::Char('q'), KeyModifiers::NONE),
            (KeyCode::Char('c'), KeyModifiers::CONTROL),
        ],
        command: KeyCommand::Quit,
        label: "q, Ctrl+c",
        description: "Quit loom",
    },
    KeyBinding {
        layer: Layer::Global,
        keys: &[(KeyCode::Char('1'), KeyModifiers::NONE)],
        command: KeyCommand::SelectTab(QueueTab::Today),
        label: "1",
        description: "Select Today queue",
    },
    KeyBinding {
        layer: Layer::Global,
        keys: &[(KeyCode::Char('2'), KeyModifiers::NONE)],
        command: KeyCommand::SelectTab(QueueTab::Inbox),
        label: "2",
        description: "Select Inbox queue",
    },
    KeyBinding {
        layer: Layer::Global,
        keys: &[(KeyCode::Char('3'), KeyModifiers::NONE)],
        command: KeyCommand::SelectTab(QueueTab::Waiting),
        label: "3",
        description: "Select Waiting queue",
    },
    KeyBinding {
        layer: Layer::Global,
        keys: &[(KeyCode::Tab, KeyModifiers::NONE)],
        command: KeyCommand::NextTab,
        label: "Tab",
        description: "Next queue tab",
    },
    KeyBinding {
        layer: Layer::Global,
        keys: &[(KeyCode::BackTab, KeyModifiers::SHIFT)],
        command: KeyCommand::PrevTab,
        label: "Shift+Tab",
        description: "Previous queue tab",
    },
    KeyBinding {
        layer: Layer::Global,
        keys: &[(KeyCode::Char(':'), KeyModifiers::NONE)],
        command: KeyCommand::OpenPalette,
        label: ":",
        description: "Open command palette",
    },
    KeyBinding {
        layer: Layer::Global,
        keys: &[(KeyCode::Char('/'), KeyModifiers::NONE)],
        command: KeyCommand::OpenSearch,
        label: "/",
        description: "Open live search",
    },
    KeyBinding {
        layer: Layer::Global,
        keys: &[(KeyCode::Char('?'), KeyModifiers::NONE)],
        command: KeyCommand::OpenHelp,
        label: "?",
        description: "Show key bindings help",
    },
    KeyBinding {
        layer: Layer::Global,
        keys: &[(KeyCode::Char('@'), KeyModifiers::NONE)],
        command: KeyCommand::OpenContextPicker,
        label: "@",
        description: "Pick context filter",
    },
    // View: Queue
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[
            (KeyCode::Char('j'), KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
        ],
        command: KeyCommand::MoveDown,
        label: "j, ↓",
        description: "Select next row",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[
            (KeyCode::Char('k'), KeyModifiers::NONE),
            (KeyCode::Up, KeyModifiers::NONE),
        ],
        command: KeyCommand::MoveUp,
        label: "k, ↑",
        description: "Select previous row",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Home, KeyModifiers::NONE)],
        command: KeyCommand::MoveFirst,
        label: "Home",
        description: "Select first row",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[
            (KeyCode::Char('G'), KeyModifiers::SHIFT),
            (KeyCode::End, KeyModifiers::NONE),
        ],
        command: KeyCommand::MoveLast,
        label: "G, End",
        description: "Select last row",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::PageDown, KeyModifiers::NONE)],
        command: KeyCommand::PageDown,
        label: "PgDn",
        description: "Scroll down one page",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::PageUp, KeyModifiers::NONE)],
        command: KeyCommand::PageUp,
        label: "PgUp",
        description: "Scroll up one page",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Enter, KeyModifiers::NONE)],
        command: KeyCommand::OpenSelected,
        label: "Enter",
        description: "Open object page",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Char('|'), KeyModifiers::NONE)],
        command: KeyCommand::TogglePreview,
        label: "|",
        description: "Toggle preview pane",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Char('r'), KeyModifiers::NONE)],
        command: KeyCommand::Refresh,
        label: "r",
        description: "Refresh from loomd",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Char('a'), KeyModifiers::NONE)],
        command: KeyCommand::TriageAccept,
        label: "a",
        description: "Accept inbox task",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Char('z'), KeyModifiers::NONE)],
        command: KeyCommand::TriageSnooze,
        label: "z",
        description: "Snooze task until date",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Char('x'), KeyModifiers::NONE)],
        command: KeyCommand::TriageCancel,
        label: "x",
        description: "Cancel task",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Char('c'), KeyModifiers::NONE)],
        command: KeyCommand::TriageDone,
        label: "c",
        description: "Mark task done",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Char('g'), KeyModifiers::NONE)],
        command: KeyCommand::TriageGo,
        label: "g",
        description: "Focus task and cd to repository on quit",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Char('u'), KeyModifiers::NONE)],
        command: KeyCommand::Undo,
        label: "u",
        description: "Undo last task change",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Char('+'), KeyModifiers::NONE)],
        command: KeyCommand::NewTask,
        label: "+",
        description: "Create new task",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Char('w'), KeyModifiers::NONE)],
        command: KeyCommand::ToggleWhy,
        label: "w",
        description: "Toggle priority reasons (--why)",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::Queue),
        keys: &[(KeyCode::Char('p'), KeyModifiers::NONE)],
        command: KeyCommand::ToggleGroupByParent,
        label: "p",
        description: "Toggle grouping by project",
    },
    // View: ObjectPage
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[
            (KeyCode::Esc, KeyModifiers::NONE),
            (KeyCode::Backspace, KeyModifiers::NONE),
        ],
        command: KeyCommand::Back,
        label: "Esc, Backspace",
        description: "Go back to the previous page or the queue",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[
            (KeyCode::Char(']'), KeyModifiers::NONE),
            (KeyCode::Right, KeyModifiers::NONE),
        ],
        command: KeyCommand::NextPageTab,
        label: "], →",
        description: "Next page tab",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[
            (KeyCode::Char('['), KeyModifiers::NONE),
            (KeyCode::Left, KeyModifiers::NONE),
        ],
        command: KeyCommand::PrevPageTab,
        label: "[, ←",
        description: "Previous page tab",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[
            (KeyCode::Char('j'), KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
        ],
        command: KeyCommand::MoveDown,
        label: "j, ↓",
        description: "Scroll down",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[
            (KeyCode::Char('k'), KeyModifiers::NONE),
            (KeyCode::Up, KeyModifiers::NONE),
        ],
        command: KeyCommand::MoveUp,
        label: "k, ↑",
        description: "Scroll up",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Enter, KeyModifiers::NONE)],
        command: KeyCommand::OpenSelected,
        label: "Enter",
        description: "Open the selected relation (Relations tab)",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Home, KeyModifiers::NONE)],
        command: KeyCommand::MoveFirst,
        label: "Home",
        description: "Scroll to the top; select the first task (Relations tab)",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::End, KeyModifiers::NONE)],
        command: KeyCommand::MoveLast,
        label: "End",
        description: "Scroll to the oldest event (Events tab); select the last task (Relations tab)",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::PageDown, KeyModifiers::NONE)],
        command: KeyCommand::PageDown,
        label: "PgDn",
        description: "Scroll down one page",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::PageUp, KeyModifiers::NONE)],
        command: KeyCommand::PageUp,
        label: "PgUp",
        description: "Scroll up one page",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Char('r'), KeyModifiers::NONE)],
        command: KeyCommand::Refresh,
        label: "r",
        description: "Refresh object",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Char('e'), KeyModifiers::NONE)],
        command: KeyCommand::EditBody,
        label: "e",
        description: "Edit the task file in the editor",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Char('t'), KeyModifiers::NONE)],
        command: KeyCommand::OpenPicker(PickField::Kind),
        label: "t",
        description: "Change the kind",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Char('i'), KeyModifiers::NONE)],
        command: KeyCommand::OpenPicker(PickField::Importance),
        label: "i",
        description: "Change the importance",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Char('s'), KeyModifiers::NONE)],
        command: KeyCommand::OpenPicker(PickField::Status),
        label: "s",
        description: "Change the status",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Char('d'), KeyModifiers::NONE)],
        command: KeyCommand::OpenPicker(PickField::Due),
        label: "d",
        description: "Change the due date",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Char('c'), KeyModifiers::NONE)],
        command: KeyCommand::OpenPicker(PickField::Context),
        label: "c",
        description: "Move to another context",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Char('u'), KeyModifiers::NONE)],
        command: KeyCommand::Undo,
        label: "u",
        description: "Undo last task change",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Char('f'), KeyModifiers::NONE)],
        command: KeyCommand::CycleEventsFilter(EventsFilter::Type),
        label: "f",
        description: "Filter the events by type",
    },
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Char('a'), KeyModifiers::NONE)],
        command: KeyCommand::CycleEventsFilter(EventsFilter::Actor),
        label: "a",
        description: "Filter the events by actor",
    },
    // Modal: Help
    KeyBinding {
        layer: Layer::Modal(ModalKind::Help),
        keys: &[
            (KeyCode::Esc, KeyModifiers::NONE),
            (KeyCode::Char('?'), KeyModifiers::NONE),
            (KeyCode::Char('q'), KeyModifiers::NONE),
        ],
        command: KeyCommand::CloseModal,
        label: "Esc, ?, q",
        description: "Close help",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Help),
        keys: &[
            (KeyCode::Char('j'), KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
        ],
        command: KeyCommand::MoveDown,
        label: "j, ↓",
        description: "Scroll help down",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Help),
        keys: &[
            (KeyCode::Char('k'), KeyModifiers::NONE),
            (KeyCode::Up, KeyModifiers::NONE),
        ],
        command: KeyCommand::MoveUp,
        label: "k, ↑",
        description: "Scroll help up",
    },
    // Modal: ContextPicker
    KeyBinding {
        layer: Layer::Modal(ModalKind::ContextPicker),
        keys: &[(KeyCode::Esc, KeyModifiers::NONE)],
        command: KeyCommand::CloseModal,
        label: "Esc",
        description: "Close context picker",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::ContextPicker),
        keys: &[
            (KeyCode::Char('j'), KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
        ],
        command: KeyCommand::MoveDown,
        label: "j, ↓",
        description: "Select next context",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::ContextPicker),
        keys: &[
            (KeyCode::Char('k'), KeyModifiers::NONE),
            (KeyCode::Up, KeyModifiers::NONE),
        ],
        command: KeyCommand::MoveUp,
        label: "k, ↑",
        description: "Select previous context",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::ContextPicker),
        keys: &[(KeyCode::Enter, KeyModifiers::NONE)],
        command: KeyCommand::ConfirmModal,
        label: "Enter",
        description: "Apply selected context",
    },
    // Modal: Palette
    KeyBinding {
        layer: Layer::Modal(ModalKind::Palette),
        keys: &[(KeyCode::Esc, KeyModifiers::NONE)],
        command: KeyCommand::CloseModal,
        label: "Esc",
        description: "Close palette",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Palette),
        keys: &[
            (KeyCode::Down, KeyModifiers::NONE),
            (KeyCode::Char('n'), KeyModifiers::CONTROL),
        ],
        command: KeyCommand::MoveDown,
        label: "↓, Ctrl+n",
        description: "Select next command",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Palette),
        keys: &[
            (KeyCode::Up, KeyModifiers::NONE),
            (KeyCode::Char('p'), KeyModifiers::CONTROL),
        ],
        command: KeyCommand::MoveUp,
        label: "↑, Ctrl+p",
        description: "Select previous command",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Palette),
        keys: &[(KeyCode::Enter, KeyModifiers::NONE)],
        command: KeyCommand::ConfirmModal,
        label: "Enter",
        description: "Execute selected command",
    },
    // Modal: Search
    KeyBinding {
        layer: Layer::Modal(ModalKind::Search),
        keys: &[(KeyCode::Esc, KeyModifiers::NONE)],
        command: KeyCommand::CloseModal,
        label: "Esc",
        description: "Close search",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Search),
        keys: &[
            (KeyCode::Down, KeyModifiers::NONE),
            (KeyCode::Char('n'), KeyModifiers::CONTROL),
        ],
        command: KeyCommand::MoveDown,
        label: "↓, Ctrl+n",
        description: "Select next result",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Search),
        keys: &[
            (KeyCode::Up, KeyModifiers::NONE),
            (KeyCode::Char('p'), KeyModifiers::CONTROL),
        ],
        command: KeyCommand::MoveUp,
        label: "↑, Ctrl+p",
        description: "Select previous result",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Search),
        keys: &[(KeyCode::Enter, KeyModifiers::NONE)],
        command: KeyCommand::ConfirmModal,
        label: "Enter",
        description: "Open selected result",
    },
    // Modal: Accept
    KeyBinding {
        layer: Layer::Modal(ModalKind::Accept),
        keys: &[(KeyCode::Esc, KeyModifiers::NONE)],
        command: KeyCommand::CloseModal,
        label: "Esc",
        description: "Close accept modal",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Accept),
        keys: &[(KeyCode::Enter, KeyModifiers::NONE)],
        command: KeyCommand::ConfirmModal,
        label: "Enter",
        description: "Confirm and accept task",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Accept),
        keys: &[
            (KeyCode::Tab, KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
        ],
        command: KeyCommand::NextField,
        label: "Tab, ↓",
        description: "Next field",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Accept),
        keys: &[
            (KeyCode::BackTab, KeyModifiers::SHIFT),
            (KeyCode::Up, KeyModifiers::NONE),
        ],
        command: KeyCommand::PrevField,
        label: "Shift+Tab, ↑",
        description: "Previous field",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Accept),
        keys: &[
            (KeyCode::Left, KeyModifiers::NONE),
            (KeyCode::Char('h'), KeyModifiers::NONE),
        ],
        command: KeyCommand::PrevOption,
        label: "Left, h",
        description: "Previous option",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Accept),
        keys: &[
            (KeyCode::Right, KeyModifiers::NONE),
            (KeyCode::Char('l'), KeyModifiers::NONE),
        ],
        command: KeyCommand::NextOption,
        label: "Right, l",
        description: "Next option",
    },
    // Modal: Snooze
    KeyBinding {
        layer: Layer::Modal(ModalKind::Snooze),
        keys: &[(KeyCode::Esc, KeyModifiers::NONE)],
        command: KeyCommand::CloseModal,
        label: "Esc",
        description: "Close snooze modal",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Snooze),
        keys: &[(KeyCode::Enter, KeyModifiers::NONE)],
        command: KeyCommand::ConfirmModal,
        label: "Enter",
        description: "Confirm and snooze task",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Snooze),
        keys: &[
            (KeyCode::Tab, KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
        ],
        command: KeyCommand::NextField,
        label: "Tab, ↓",
        description: "Next field",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Snooze),
        keys: &[
            (KeyCode::BackTab, KeyModifiers::SHIFT),
            (KeyCode::Up, KeyModifiers::NONE),
        ],
        command: KeyCommand::PrevField,
        label: "Shift+Tab, ↑",
        description: "Previous field",
    },
    // Modal: NewTask
    KeyBinding {
        layer: Layer::Modal(ModalKind::NewTask),
        keys: &[(KeyCode::Esc, KeyModifiers::NONE)],
        command: KeyCommand::CloseModal,
        label: "Esc",
        description: "Close new task modal",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::NewTask),
        keys: &[(KeyCode::Enter, KeyModifiers::NONE)],
        command: KeyCommand::ConfirmModal,
        label: "Enter",
        description: "Confirm and create task",
    },
    // Modal: Pick
    KeyBinding {
        layer: Layer::Modal(ModalKind::Pick),
        keys: &[(KeyCode::Esc, KeyModifiers::NONE)],
        command: KeyCommand::CloseModal,
        label: "Esc",
        description: "Close without changing",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Pick),
        keys: &[(KeyCode::Enter, KeyModifiers::NONE)],
        command: KeyCommand::ConfirmModal,
        label: "Enter",
        description: "Apply the choice",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Pick),
        keys: &[
            (KeyCode::Char('j'), KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
        ],
        command: KeyCommand::MoveDown,
        label: "j, ↓",
        description: "Next choice",
    },
    KeyBinding {
        layer: Layer::Modal(ModalKind::Pick),
        keys: &[
            (KeyCode::Char('k'), KeyModifiers::NONE),
            (KeyCode::Up, KeyModifiers::NONE),
        ],
        command: KeyCommand::MoveUp,
        label: "k, ↑",
        description: "Previous choice",
    },
];

fn key_matches(key: KeyEvent, expected_code: KeyCode, expected_modifiers: KeyModifiers) -> bool {
    if key.code != expected_code {
        return false;
    }
    match expected_code {
        KeyCode::Char(_) => {
            // Ignore SHIFT for Char keys: terminals or keyboard protocols (kitty, Windows)
            // often report SHIFT on shifted characters like '?', ':', '|', '@'.
            let actual = key.modifiers - KeyModifiers::SHIFT;
            let expected = expected_modifiers - KeyModifiers::SHIFT;
            actual == expected
        }
        _ => key.modifiers == expected_modifiers,
    }
}

/// Matches `key` in `layer` using the single canonical [`BINDINGS`] table.
#[must_use]
pub fn match_in_layer(layer: Layer, key: KeyEvent) -> Option<KeyCommand> {
    for binding in BINDINGS {
        if binding.layer == layer {
            for &(code, modifiers) in binding.keys {
                if key_matches(key, code, modifiers) {
                    return Some(binding.command);
                }
            }
        }
    }
    None
}

/// Returns all bindings registered for `layer`.
pub fn bindings_for_layer(layer: Layer) -> impl Iterator<Item = &'static KeyBinding> {
    BINDINGS.iter().filter(move |b| b.layer == layer)
}

/// Resolves a key event. When a modal is open, it captures all keys and never falls through
/// to the underlying view or global shortcuts (ADR-0014 §4).
#[must_use]
pub fn resolve_key(modal: Option<ModalKind>, view: ViewKind, key: KeyEvent) -> Option<KeyCommand> {
    if let Some(m) = modal {
        return match_in_layer(Layer::Modal(m), key);
    }
    if let Some(cmd) = match_in_layer(Layer::View(view), key) {
        return Some(cmd);
    }
    match_in_layer(Layer::Global, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_of_every_binding_triggers_its_command() {
        for binding in BINDINGS {
            for &(code, modifiers) in binding.keys {
                let event = KeyEvent::new(code, modifiers);
                let matched = match_in_layer(binding.layer, event);
                assert_eq!(
                    matched,
                    Some(binding.command),
                    "key {code:?} with modifiers {modifiers:?} in layer {:?} did not trigger {:?}",
                    binding.layer,
                    binding.command
                );
            }
        }
    }

    #[test]
    fn modal_layer_intercepts_keys_and_does_not_fall_through() {
        // 'q' quits in Global, but in ContextPicker it is unmapped so it must be None (not Quit!)
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(
            resolve_key(Some(ModalKind::ContextPicker), ViewKind::Queue, q),
            None
        );

        // 'Enter' opens task in Queue view, but in Help modal it is unmapped so it must be None
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            resolve_key(Some(ModalKind::Help), ViewKind::Queue, enter),
            None
        );

        // 'Tab' switches tabs in Global, but in Search modal it is unmapped so it must be None
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(
            resolve_key(Some(ModalKind::Search), ViewKind::Queue, tab),
            None
        );
    }

    #[test]
    fn char_keys_ignore_terminal_shift_modifier() {
        let question_plain = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
        let question_shifted = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(
            match_in_layer(Layer::Global, question_plain),
            Some(KeyCommand::OpenHelp)
        );
        assert_eq!(
            match_in_layer(Layer::Global, question_shifted),
            Some(KeyCommand::OpenHelp)
        );

        let colon_plain = KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE);
        let colon_shifted = KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT);
        assert_eq!(
            match_in_layer(Layer::Global, colon_plain),
            Some(KeyCommand::OpenPalette)
        );
        assert_eq!(
            match_in_layer(Layer::Global, colon_shifted),
            Some(KeyCommand::OpenPalette)
        );
    }

    #[test]
    fn ctrl_c_and_q_are_both_global_quit() {
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            resolve_key(None, ViewKind::Queue, q),
            Some(KeyCommand::Quit)
        );
        assert_eq!(
            resolve_key(None, ViewKind::Queue, ctrl_c),
            Some(KeyCommand::Quit)
        );
    }

    #[test]
    fn triage_queue_keys_resolve_in_queue_view() {
        let test_cases = [
            ('a', KeyCommand::TriageAccept),
            ('z', KeyCommand::TriageSnooze),
            ('x', KeyCommand::TriageCancel),
            ('c', KeyCommand::TriageDone),
            ('g', KeyCommand::TriageGo),
            ('u', KeyCommand::Undo),
            ('+', KeyCommand::NewTask),
            ('w', KeyCommand::ToggleWhy),
            ('p', KeyCommand::ToggleGroupByParent),
        ];

        for (ch, expected) in test_cases {
            let event = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
            assert_eq!(
                resolve_key(None, ViewKind::Queue, event),
                Some(expected),
                "expected key '{ch}' to resolve to {expected:?}"
            );
        }
    }

    #[test]
    fn accept_modal_bindings() {
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            resolve_key(Some(ModalKind::Accept), ViewKind::Queue, esc),
            Some(KeyCommand::CloseModal)
        );
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            resolve_key(Some(ModalKind::Accept), ViewKind::Queue, enter),
            Some(KeyCommand::ConfirmModal)
        );
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(
            resolve_key(Some(ModalKind::Accept), ViewKind::Queue, tab),
            Some(KeyCommand::NextField)
        );
        let backtab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(
            resolve_key(Some(ModalKind::Accept), ViewKind::Queue, backtab),
            Some(KeyCommand::PrevField)
        );
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(
            resolve_key(Some(ModalKind::Accept), ViewKind::Queue, left),
            Some(KeyCommand::PrevOption)
        );
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            resolve_key(Some(ModalKind::Accept), ViewKind::Queue, right),
            Some(KeyCommand::NextOption)
        );
    }

    #[test]
    fn snooze_and_new_task_modal_bindings() {
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            resolve_key(Some(ModalKind::Snooze), ViewKind::Queue, esc),
            Some(KeyCommand::CloseModal)
        );
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            resolve_key(Some(ModalKind::Snooze), ViewKind::Queue, enter),
            Some(KeyCommand::ConfirmModal)
        );
        assert_eq!(
            resolve_key(Some(ModalKind::NewTask), ViewKind::Queue, esc),
            Some(KeyCommand::CloseModal)
        );
        assert_eq!(
            resolve_key(Some(ModalKind::NewTask), ViewKind::Queue, enter),
            Some(KeyCommand::ConfirmModal)
        );
    }
}
