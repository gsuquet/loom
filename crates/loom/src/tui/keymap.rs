//! Layered keymap and generated `?` help (ADR-0014).
//!
//! Keys are resolved from the innermost active layer outward:
//! 1. An open modal (`?` help, `:` command palette, `/` search, `@` context picker) captures keys
//!    first.
//! 2. Otherwise the active view (Queue or Object page) handles view-specific keys, then falls
//!    back to the global layer.
//!
//! A single [`BINDINGS`] table defines both the key matching logic and the help entries,
//! ensuring they never drift.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
}

impl ModalKind {
    /// All modal kinds.
    pub const ALL: [Self; 4] = [Self::Help, Self::Palette, Self::Search, Self::ContextPicker];

    /// Display title of the modal layer in `?` help.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Help => "Help modal",
            Self::Palette => "Command palette",
            Self::Search => "Search modal",
            Self::ContextPicker => "Context picker",
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
    // Object page actions
    Back,
    // Modal actions
    CloseModal,
    ConfirmModal,
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
    // View: ObjectPage
    KeyBinding {
        layer: Layer::View(ViewKind::ObjectPage),
        keys: &[(KeyCode::Esc, KeyModifiers::NONE)],
        command: KeyCommand::Back,
        label: "Esc",
        description: "Return to queue",
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
];

/// Matches `key` in `layer` using the single canonical [`BINDINGS`] table.
#[must_use]
pub fn match_in_layer(layer: Layer, key: KeyEvent) -> Option<KeyCommand> {
    for binding in BINDINGS {
        if binding.layer == layer {
            for &(code, modifiers) in binding.keys {
                if key.code == code && key.modifiers == modifiers {
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

/// Resolves a key event starting at any active modal, falling back to the view, then global.
#[must_use]
pub fn resolve_key(modal: Option<ModalKind>, view: ViewKind, key: KeyEvent) -> Option<KeyCommand> {
    if let Some(cmd) = modal.and_then(|m| match_in_layer(Layer::Modal(m), key)) {
        return Some(cmd);
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
    fn ctrl_c_and_q_are_both_global_quit() {
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        assert_eq!(match_in_layer(Layer::Global, q), Some(KeyCommand::Quit));
        assert_eq!(
            match_in_layer(Layer::Global, ctrl_c),
            Some(KeyCommand::Quit)
        );
    }

    #[test]
    fn every_layer_has_non_empty_bindings_and_labels() {
        for binding in BINDINGS {
            assert_ne!(binding.label, "");
            assert_ne!(binding.description, "");
            assert_ne!(binding.keys, []);
        }
    }
}
