//! ANSI 16-color palette for contexts and UI elements (ADR-0014).
//!
//! Only the terminal's 16 ANSI colors are used so the TUI follows the terminal theme (light or
//! dark). When colors are disabled (`--color=never` or `NO_COLOR`), every style is uncolored.

use std::collections::BTreeMap;

use loom_api::{ContextName, Importance, StatusName, UNASSIGNED};
use loom_config::repos::{ContextColor, ContextConfig};
use ratatui::style::{Color, Modifier, Style};

/// Default cycle of readable ANSI colors for contexts without an explicit `color` in
/// `config.toml`. Excludes `black` and `white` so badges remain legible on both dark and light
/// terminal themes.
pub const DEFAULT_PALETTE: [ContextColor; 12] = [
    ContextColor::Cyan,
    ContextColor::Green,
    ContextColor::Yellow,
    ContextColor::Blue,
    ContextColor::Magenta,
    ContextColor::BrightCyan,
    ContextColor::BrightGreen,
    ContextColor::BrightYellow,
    ContextColor::BrightBlue,
    ContextColor::BrightMagenta,
    ContextColor::Red,
    ContextColor::BrightRed,
];

/// Converts a configured [`ContextColor`] to ratatui's 16-color [`Color`].
#[must_use]
pub const fn to_ratatui_color(color: ContextColor) -> Color {
    match color {
        ContextColor::Black => Color::Black,
        ContextColor::Red => Color::Red,
        ContextColor::Green => Color::Green,
        ContextColor::Yellow => Color::Yellow,
        ContextColor::Blue => Color::Blue,
        ContextColor::Magenta => Color::Magenta,
        ContextColor::Cyan => Color::Cyan,
        ContextColor::White => Color::Gray,
        ContextColor::BrightBlack => Color::DarkGray,
        ContextColor::BrightRed => Color::LightRed,
        ContextColor::BrightGreen => Color::LightGreen,
        ContextColor::BrightYellow => Color::LightYellow,
        ContextColor::BrightBlue => Color::LightBlue,
        ContextColor::BrightMagenta => Color::LightMagenta,
        ContextColor::BrightCyan => Color::LightCyan,
        ContextColor::BrightWhite => Color::White,
    }
}

/// Deterministic fallback color from [`DEFAULT_PALETTE`] for `context_name`.
#[must_use]
pub fn default_context_color(context_name: &str) -> ContextColor {
    if context_name == UNASSIGNED {
        return ContextColor::BrightBlack;
    }
    // FNV-1a 32-bit hash for deterministic cross-platform assignment.
    let mut hash: u32 = 2_166_136_261;
    for byte in context_name.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    DEFAULT_PALETTE[(hash as usize) % DEFAULT_PALETTE.len()]
}

/// Resolved context palette and color switch for the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Configured color per context name.
    configured: BTreeMap<String, ContextColor>,
    /// Whether ANSI colors are emitted.
    enabled: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            configured: BTreeMap::new(),
            enabled: true,
        }
    }
}

impl Theme {
    /// Builds a theme from the `[contexts]` table of `config.toml` and whether color output is
    /// enabled.
    #[must_use]
    pub fn from_config(contexts: &BTreeMap<ContextName, ContextConfig>, enabled: bool) -> Self {
        let configured = contexts
            .iter()
            .filter_map(|(name, cfg)| cfg.color.map(|color| (name.as_str().to_owned(), color)))
            .collect();
        Self {
            configured,
            enabled,
        }
    }

    /// Builds a theme from explicit `(context, ContextColor)` pairs.
    #[must_use]
    pub const fn new(configured: BTreeMap<String, ContextColor>, enabled: bool) -> Self {
        Self {
            configured,
            enabled,
        }
    }

    /// Whether ANSI colors are enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The [`ContextColor`] of `context`: the configured color when set, otherwise the
    /// deterministic fallback from [`DEFAULT_PALETTE`].
    #[must_use]
    pub fn context_color(&self, context: &str) -> ContextColor {
        self.configured
            .get(context)
            .copied()
            .unwrap_or_else(|| default_context_color(context))
    }

    /// Foreground [`Style`] for `context` (uncolored when colors are disabled).
    #[must_use]
    pub fn context_style(&self, context: &str) -> Style {
        if !self.enabled {
            return Style::default();
        }
        Style::default().fg(to_ratatui_color(self.context_color(context)))
    }

    /// Muted style for secondary text.
    #[must_use]
    pub const fn muted(&self) -> Style {
        if self.enabled {
            Style::new().fg(Color::DarkGray)
        } else {
            Style::new()
        }
    }

    /// Bold accent style for active tabs and headers.
    #[must_use]
    pub const fn active_tab(&self) -> Style {
        if self.enabled {
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        }
    }

    /// Style for the selected row in a list or modal.
    #[must_use]
    pub const fn selected_row(&self) -> Style {
        Style::new().add_modifier(Modifier::REVERSED)
    }

    /// Style for a task's [`Importance`].
    #[must_use]
    pub const fn importance_style(&self, importance: Importance) -> Style {
        if !self.enabled {
            return Style::new();
        }
        match importance {
            Importance::P0 => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            Importance::P1 => Style::new().fg(Color::Yellow),
            Importance::P2 => Style::new().fg(Color::Blue),
            Importance::P3 => Style::new().fg(Color::DarkGray),
        }
    }
}

/// Glyph for a task's status in a queue row (`docs/design/user-workflow.md`).
#[must_use]
pub const fn status_glyph(status: Option<StatusName>) -> &'static str {
    match status {
        Some(StatusName::Active) => "●",
        Some(StatusName::Waiting) => "◐",
        Some(StatusName::Ready | StatusName::Inbox) => "○",
        Some(StatusName::Done) => "✓",
        Some(StatusName::Cancelled) => "×",
        None => "!",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_context_color_maps_to_a_sixteen_color_ansi_variant() {
        for color in ContextColor::ALL {
            let ratatui_color = to_ratatui_color(color);
            assert!(
                !matches!(ratatui_color, Color::Rgb(_, _, _) | Color::Indexed(_)),
                "{color:?} mapped to non-16-ANSI color {ratatui_color:?}"
            );
        }
    }

    #[test]
    fn configured_color_overrides_default_palette_and_disabled_theme_is_uncolored() {
        let mut map = BTreeMap::new();
        map.insert("company-a".to_owned(), ContextColor::Magenta);
        let theme = Theme::new(map.clone(), true);
        assert_eq!(theme.context_color("company-a"), ContextColor::Magenta);
        assert_eq!(
            theme.context_style("company-a"),
            Style::default().fg(Color::Magenta)
        );
        assert_eq!(theme.context_color(UNASSIGNED), ContextColor::BrightBlack);

        let no_color = Theme::new(map, false);
        assert_eq!(no_color.context_style("company-a"), Style::default());
        assert_eq!(no_color.muted(), Style::default());
        assert_eq!(no_color.importance_style(Importance::P0), Style::default());
    }
}
