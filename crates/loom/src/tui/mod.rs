//! Terminal user interface (ADR-0014).

pub mod color;
pub mod keymap;
pub mod palette;
pub mod render;
pub mod state;
pub mod terminal;

pub use keymap::QueueTab;
pub use state::{AppEffect, AppState, ObjectPageContent, ObjectPageState, ObjectTarget, SseState};
