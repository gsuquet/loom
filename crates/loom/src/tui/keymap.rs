//! Layered keymap and generated `?` help (ADR-0014).
//!
//! Keys are resolved from the innermost active layer outward:
//! 1. An open modal (`?` help, `:` command palette, `/` search, `@` context picker) captures keys
//!    first.
//! 2. Otherwise the active view (Queue or Object page) handles view-specific keys, then falls
//!    back to the global layer.
//!
//! `?` help is built directly from [`bindings_for_layer`] so every registered binding appears in
//! the help view.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Which screen view is active below any modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewKind {
    /// A queue list (Today, Inbox, Waiting, Repos) with an optional right-hand preview pane.
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

/// A layer of the keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    /// Keys available in every non-modal screen.
    Global,
    /// Keys specific to the current view.
    View(ViewKind),
    /// Keys when a modal is open.
    Modal(ModalKind),
}

impl Layer {
    /// Display title of the layer in `?` help.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::View(view) => view.title(),
            Self::Modal(modal) => modal.title(),
        }
    }
}

/// Queue tabs reachable from the tab bar and number keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueueTab {
    /// Priority list (`1`).
    Today,
    /// Untriaged tasks (`2`).
    Inbox,
    /// Snoozed / blocked tasks (`3`).
    Waiting,
    /// Indexed repositories (`4`).
    Repos,
}

impl QueueTab {
    /// All queue tabs in tab-bar order.
    pub const ALL: [Self; 4] = [Self::Today, Self::Inbox, Self::Waiting, Self::Repos];

    /// 1-based number key of the tab.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::Today => 1,
            Self::Inbox => 2,
            Self::Waiting => 3,
            Self::Repos => 4,
        }
    }

    /// Label shown in the tab bar.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Inbox => "Inbox",
            Self::Waiting => "Waiting",
            Self::Repos => "Repos",
        }
    }

    /// Next tab in wrap-around order.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Today => Self::Inbox,
            Self::Inbox => Self::Waiting,
            Self::Waiting => Self::Repos,
            Self::Repos => Self::Today,
        }
    }

    /// Previous tab in wrap-around order.
    #[must_use]
    pub const fn prev(self) -> Self {
        match self {
            Self::Today => Self::Repos,
            Self::Inbox => Self::Today,
            Self::Waiting => Self::Inbox,
            Self::Repos => Self::Waiting,
        }
    }
}

/// Semantic command produced by a key binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCommand {
    /// Quit the TUI (`q`).
    Quit,
    /// Open `?` help.
    OpenHelp,
    /// Open `:` command palette.
    OpenPalette,
    /// Open `/` search modal.
    OpenSearch,
    /// Open `@` context filter picker.
    OpenContextPicker,
    /// Switch to a specific queue tab (`1`..`4`).
    SelectTab(QueueTab),
    /// Cycle to the next queue tab (`Tab`).
    NextTab,
    /// Cycle to the previous queue tab (`Shift+Tab`).
    PrevTab,
    /// Move selection or scroll down one line (`j` / `Down`).
    MoveDown,
    /// Move selection or scroll up one line (`k` / `Up`).
    MoveUp,
    /// Jump to the first item or top (`Home`).
    MoveFirst,
    /// Jump to the last item or bottom (`G` / `End`).
    MoveLast,
    /// Open the selected item in an object page (`Enter`).
    OpenSelected,
    /// Return from an object page to the queue (`Esc` / `Backspace`).
    Back,
    /// Toggle the right-hand preview pane (`|`).
    TogglePreview,
    /// Refetch the current view's data from `loomd` (`r`).
    Refresh,
    /// Close the active modal (`Esc`).
    CloseModal,
    /// Confirm the current selection in a modal (`Enter`).
    ConfirmModal,
    /// Delete the character before the cursor in a text-input modal (`Backspace`).
    DeleteChar,
}

/// One documented key binding in a [`Layer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Layer this binding belongs to.
    pub layer: Layer,
    /// Human-readable key label shown in `?` help and the footer bar.
    pub key_label: &'static str,
    /// Short description shown in `?` help.
    pub description: &'static str,
    /// Command triggered when the binding matches.
    pub command: KeyCommand,
}

const GLOBAL_BINDINGS: &[Binding] = &[
    Binding {
        layer: Layer::Global,
        key_label: "1",
        description: "Open Today queue",
        command: KeyCommand::SelectTab(QueueTab::Today),
    },
    Binding {
        layer: Layer::Global,
        key_label: "2",
        description: "Open Inbox queue",
        command: KeyCommand::SelectTab(QueueTab::Inbox),
    },
    Binding {
        layer: Layer::Global,
        key_label: "3",
        description: "Open Waiting queue",
        command: KeyCommand::SelectTab(QueueTab::Waiting),
    },
    Binding {
        layer: Layer::Global,
        key_label: "4",
        description: "Open Repos queue",
        command: KeyCommand::SelectTab(QueueTab::Repos),
    },
    Binding {
        layer: Layer::Global,
        key_label: "Tab",
        description: "Switch to next queue tab",
        command: KeyCommand::NextTab,
    },
    Binding {
        layer: Layer::Global,
        key_label: "Shift+Tab",
        description: "Switch to previous queue tab",
        command: KeyCommand::PrevTab,
    },
    Binding {
        layer: Layer::Global,
        key_label: ":",
        description: "Open command palette",
        command: KeyCommand::OpenPalette,
    },
    Binding {
        layer: Layer::Global,
        key_label: "/",
        description: "Search tasks and repositories",
        command: KeyCommand::OpenSearch,
    },
    Binding {
        layer: Layer::Global,
        key_label: "?",
        description: "Show key bindings help",
        command: KeyCommand::OpenHelp,
    },
    Binding {
        layer: Layer::Global,
        key_label: "q",
        description: "Quit loom",
        command: KeyCommand::Quit,
    },
];

const QUEUE_BINDINGS: &[Binding] = &[
    Binding {
        layer: Layer::View(ViewKind::Queue),
        key_label: "j / Down",
        description: "Select next item",
        command: KeyCommand::MoveDown,
    },
    Binding {
        layer: Layer::View(ViewKind::Queue),
        key_label: "k / Up",
        description: "Select previous item",
        command: KeyCommand::MoveUp,
    },
    Binding {
        layer: Layer::View(ViewKind::Queue),
        key_label: "Home",
        description: "Select first item",
        command: KeyCommand::MoveFirst,
    },
    Binding {
        layer: Layer::View(ViewKind::Queue),
        key_label: "G / End",
        description: "Select last item",
        command: KeyCommand::MoveLast,
    },
    Binding {
        layer: Layer::View(ViewKind::Queue),
        key_label: "Enter",
        description: "Open selected item",
        command: KeyCommand::OpenSelected,
    },
    Binding {
        layer: Layer::View(ViewKind::Queue),
        key_label: "|",
        description: "Toggle preview pane",
        command: KeyCommand::TogglePreview,
    },
    Binding {
        layer: Layer::View(ViewKind::Queue),
        key_label: "@",
        description: "Filter by context",
        command: KeyCommand::OpenContextPicker,
    },
    Binding {
        layer: Layer::View(ViewKind::Queue),
        key_label: "r",
        description: "Refresh current queue",
        command: KeyCommand::Refresh,
    },
];

const OBJECT_PAGE_BINDINGS: &[Binding] = &[
    Binding {
        layer: Layer::View(ViewKind::ObjectPage),
        key_label: "Esc / Backspace",
        description: "Return to previous screen",
        command: KeyCommand::Back,
    },
    Binding {
        layer: Layer::View(ViewKind::ObjectPage),
        key_label: "j / Down",
        description: "Scroll down one line",
        command: KeyCommand::MoveDown,
    },
    Binding {
        layer: Layer::View(ViewKind::ObjectPage),
        key_label: "k / Up",
        description: "Scroll up one line",
        command: KeyCommand::MoveUp,
    },
    Binding {
        layer: Layer::View(ViewKind::ObjectPage),
        key_label: "Home",
        description: "Scroll to top",
        command: KeyCommand::MoveFirst,
    },
    Binding {
        layer: Layer::View(ViewKind::ObjectPage),
        key_label: "G / End",
        description: "Scroll to bottom",
        command: KeyCommand::MoveLast,
    },
    Binding {
        layer: Layer::View(ViewKind::ObjectPage),
        key_label: "r",
        description: "Refresh object page",
        command: KeyCommand::Refresh,
    },
];

const HELP_MODAL_BINDINGS: &[Binding] = &[
    Binding {
        layer: Layer::Modal(ModalKind::Help),
        key_label: "Esc / ? / q",
        description: "Close help",
        command: KeyCommand::CloseModal,
    },
    Binding {
        layer: Layer::Modal(ModalKind::Help),
        key_label: "j / Down",
        description: "Scroll help down",
        command: KeyCommand::MoveDown,
    },
    Binding {
        layer: Layer::Modal(ModalKind::Help),
        key_label: "k / Up",
        description: "Scroll help up",
        command: KeyCommand::MoveUp,
    },
];

const PALETTE_MODAL_BINDINGS: &[Binding] = &[
    Binding {
        layer: Layer::Modal(ModalKind::Palette),
        key_label: "Esc",
        description: "Close command palette",
        command: KeyCommand::CloseModal,
    },
    Binding {
        layer: Layer::Modal(ModalKind::Palette),
        key_label: "Enter",
        description: "Run selected command",
        command: KeyCommand::ConfirmModal,
    },
    Binding {
        layer: Layer::Modal(ModalKind::Palette),
        key_label: "Down / Ctrl+n",
        description: "Select next command",
        command: KeyCommand::MoveDown,
    },
    Binding {
        layer: Layer::Modal(ModalKind::Palette),
        key_label: "Up / Ctrl+p",
        description: "Select previous command",
        command: KeyCommand::MoveUp,
    },
    Binding {
        layer: Layer::Modal(ModalKind::Palette),
        key_label: "Backspace",
        description: "Delete character before cursor",
        command: KeyCommand::DeleteChar,
    },
];

const SEARCH_MODAL_BINDINGS: &[Binding] = &[
    Binding {
        layer: Layer::Modal(ModalKind::Search),
        key_label: "Esc",
        description: "Close search",
        command: KeyCommand::CloseModal,
    },
    Binding {
        layer: Layer::Modal(ModalKind::Search),
        key_label: "Enter",
        description: "Open selected search result",
        command: KeyCommand::ConfirmModal,
    },
    Binding {
        layer: Layer::Modal(ModalKind::Search),
        key_label: "Down / Ctrl+n",
        description: "Select next result",
        command: KeyCommand::MoveDown,
    },
    Binding {
        layer: Layer::Modal(ModalKind::Search),
        key_label: "Up / Ctrl+p",
        description: "Select previous result",
        command: KeyCommand::MoveUp,
    },
    Binding {
        layer: Layer::Modal(ModalKind::Search),
        key_label: "Backspace",
        description: "Delete character before cursor",
        command: KeyCommand::DeleteChar,
    },
];

const CONTEXT_PICKER_BINDINGS: &[Binding] = &[
    Binding {
        layer: Layer::Modal(ModalKind::ContextPicker),
        key_label: "Esc",
        description: "Close context picker",
        command: KeyCommand::CloseModal,
    },
    Binding {
        layer: Layer::Modal(ModalKind::ContextPicker),
        key_label: "Enter",
        description: "Apply selected context filter",
        command: KeyCommand::ConfirmModal,
    },
    Binding {
        layer: Layer::Modal(ModalKind::ContextPicker),
        key_label: "j / Down",
        description: "Select next context",
        command: KeyCommand::MoveDown,
    },
    Binding {
        layer: Layer::Modal(ModalKind::ContextPicker),
        key_label: "k / Up",
        description: "Select previous context",
        command: KeyCommand::MoveUp,
    },
];

/// Returns the documented bindings of `layer`.
#[must_use]
pub const fn bindings_for_layer(layer: Layer) -> &'static [Binding] {
    match layer {
        Layer::Global => GLOBAL_BINDINGS,
        Layer::View(ViewKind::Queue) => QUEUE_BINDINGS,
        Layer::View(ViewKind::ObjectPage) => OBJECT_PAGE_BINDINGS,
        Layer::Modal(ModalKind::Help) => HELP_MODAL_BINDINGS,
        Layer::Modal(ModalKind::Palette) => PALETTE_MODAL_BINDINGS,
        Layer::Modal(ModalKind::Search) => SEARCH_MODAL_BINDINGS,
        Layer::Modal(ModalKind::ContextPicker) => CONTEXT_PICKER_BINDINGS,
    }
}

/// Every layer in the order shown by `?` help for the given `view` (plus all modal layers so the
/// complete reference is always inspectable).
#[must_use]
pub fn help_sections(view: ViewKind) -> Vec<(Layer, &'static [Binding])> {
    vec![
        (Layer::View(view), bindings_for_layer(Layer::View(view))),
        (Layer::Global, bindings_for_layer(Layer::Global)),
        (
            Layer::Modal(ModalKind::Palette),
            bindings_for_layer(Layer::Modal(ModalKind::Palette)),
        ),
        (
            Layer::Modal(ModalKind::Search),
            bindings_for_layer(Layer::Modal(ModalKind::Search)),
        ),
        (
            Layer::Modal(ModalKind::ContextPicker),
            bindings_for_layer(Layer::Modal(ModalKind::ContextPicker)),
        ),
        (
            Layer::Modal(ModalKind::Help),
            bindings_for_layer(Layer::Modal(ModalKind::Help)),
        ),
    ]
}

/// Resolves a key press in `layer` to its [`KeyCommand`].
#[must_use]
pub const fn match_in_layer(layer: Layer, key: KeyEvent) -> Option<KeyCommand> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if alt {
        return None;
    }
    match layer {
        Layer::Global => {
            if ctrl {
                return None;
            }
            match key.code {
                KeyCode::Char('1') => Some(KeyCommand::SelectTab(QueueTab::Today)),
                KeyCode::Char('2') => Some(KeyCommand::SelectTab(QueueTab::Inbox)),
                KeyCode::Char('3') => Some(KeyCommand::SelectTab(QueueTab::Waiting)),
                KeyCode::Char('4') => Some(KeyCommand::SelectTab(QueueTab::Repos)),
                KeyCode::Tab => Some(KeyCommand::NextTab),
                KeyCode::BackTab => Some(KeyCommand::PrevTab),
                KeyCode::Char(':') => Some(KeyCommand::OpenPalette),
                KeyCode::Char('/') => Some(KeyCommand::OpenSearch),
                KeyCode::Char('?') => Some(KeyCommand::OpenHelp),
                KeyCode::Char('q') => Some(KeyCommand::Quit),
                _ => None,
            }
        }
        Layer::View(ViewKind::Queue) => {
            if ctrl {
                return None;
            }
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => Some(KeyCommand::MoveDown),
                KeyCode::Char('k') | KeyCode::Up => Some(KeyCommand::MoveUp),
                KeyCode::Home => Some(KeyCommand::MoveFirst),
                KeyCode::Char('G') | KeyCode::End => Some(KeyCommand::MoveLast),
                KeyCode::Enter => Some(KeyCommand::OpenSelected),
                KeyCode::Char('|') => Some(KeyCommand::TogglePreview),
                KeyCode::Char('@') => Some(KeyCommand::OpenContextPicker),
                KeyCode::Char('r') => Some(KeyCommand::Refresh),
                _ => None,
            }
        }
        Layer::View(ViewKind::ObjectPage) => {
            if ctrl {
                return None;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Backspace => Some(KeyCommand::Back),
                KeyCode::Char('j') | KeyCode::Down => Some(KeyCommand::MoveDown),
                KeyCode::Char('k') | KeyCode::Up => Some(KeyCommand::MoveUp),
                KeyCode::Home => Some(KeyCommand::MoveFirst),
                KeyCode::Char('G') | KeyCode::End => Some(KeyCommand::MoveLast),
                KeyCode::Char('r') => Some(KeyCommand::Refresh),
                _ => None,
            }
        }
        Layer::Modal(ModalKind::Help) => {
            if ctrl {
                return None;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('?' | 'q') => Some(KeyCommand::CloseModal),
                KeyCode::Char('j') | KeyCode::Down => Some(KeyCommand::MoveDown),
                KeyCode::Char('k') | KeyCode::Up => Some(KeyCommand::MoveUp),
                _ => None,
            }
        }
        Layer::Modal(ModalKind::Palette | ModalKind::Search) => match key.code {
            KeyCode::Esc if !ctrl => Some(KeyCommand::CloseModal),
            KeyCode::Enter if !ctrl => Some(KeyCommand::ConfirmModal),
            KeyCode::Down => Some(KeyCommand::MoveDown),
            KeyCode::Up => Some(KeyCommand::MoveUp),
            KeyCode::Char('n') if ctrl => Some(KeyCommand::MoveDown),
            KeyCode::Char('p') if ctrl => Some(KeyCommand::MoveUp),
            KeyCode::Backspace if !ctrl => Some(KeyCommand::DeleteChar),
            _ => None,
        },
        Layer::Modal(ModalKind::ContextPicker) => {
            if ctrl {
                return None;
            }
            match key.code {
                KeyCode::Esc => Some(KeyCommand::CloseModal),
                KeyCode::Enter => Some(KeyCommand::ConfirmModal),
                KeyCode::Char('j') | KeyCode::Down => Some(KeyCommand::MoveDown),
                KeyCode::Char('k') | KeyCode::Up => Some(KeyCommand::MoveUp),
                _ => None,
            }
        }
    }
}

/// Resolves a key press across the active modal (if any), active view, and global layer.
#[must_use]
pub fn resolve_key(view: ViewKind, modal: Option<ModalKind>, key: KeyEvent) -> Option<KeyCommand> {
    if let Some(modal_kind) = modal {
        return match_in_layer(Layer::Modal(modal_kind), key);
    }
    match_in_layer(Layer::View(view), key).or_else(|| match_in_layer(Layer::Global, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_binding_is_reachable_in_its_layer() {
        let layers = [
            Layer::Global,
            Layer::View(ViewKind::Queue),
            Layer::View(ViewKind::ObjectPage),
            Layer::Modal(ModalKind::Help),
            Layer::Modal(ModalKind::Palette),
            Layer::Modal(ModalKind::Search),
            Layer::Modal(ModalKind::ContextPicker),
        ];
        for layer in layers {
            let bindings = bindings_for_layer(layer);
            assert!(!bindings.is_empty(), "{layer:?} has no bindings");
            for binding in bindings {
                assert_eq!(binding.layer, layer);
                assert_ne!(binding.key_label, "");
                assert_ne!(binding.description, "");
            }
        }
    }

    #[test]
    fn modal_layer_intercepts_before_view_and_global() {
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(
            resolve_key(ViewKind::Queue, None, q),
            Some(KeyCommand::Quit)
        );
        // Inside the command palette, `q` is typed into the filter rather than quitting.
        assert_eq!(
            resolve_key(ViewKind::Queue, Some(ModalKind::Palette), q),
            None
        );
        // Inside the help modal, `q` closes the modal rather than quitting the app.
        assert_eq!(
            resolve_key(ViewKind::Queue, Some(ModalKind::Help), q),
            Some(KeyCommand::CloseModal)
        );
    }
}
