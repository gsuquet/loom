//! Terminal user interface (ADR-0014): pure state, layered keymap, fuzzy command palette,
//! 16-color ANSI theme, SSE frame parser, terminal guard and ratatui rendering.

pub mod color;
pub mod keymap;
pub mod palette;
pub mod render;
pub mod sse;
pub mod state;
pub mod terminal;
