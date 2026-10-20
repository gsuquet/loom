//! ANSI 16-color palette for contexts and UI elements (ADR-0014).
//!
//! Only the terminal's 16 ANSI colors are used so the TUI follows the terminal theme (light or
//! dark). When colors are disabled (`--color=never` or `NO_COLOR`), every style is uncolored.

use std::collections::{BTreeMap, HashMap};

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

/// Active theme with resolved styles for every element and context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    color_enabled: bool,
    context_colors: HashMap<ContextName, ContextColor>,
}

impl Theme {
    /// Builds a [`Theme`] resolving context colors from the `config.toml` contexts table.
    ///
    /// Contexts without an explicit `color = "..."` receive a deterministic ANSI color from
    /// [`DEFAULT_PALETTE`] in their configuration order.
    #[must_use]
    pub fn from_config(
        contexts: &BTreeMap<ContextName, ContextConfig>,
        color_enabled: bool,
    ) -> Self {
        let mut context_colors = HashMap::new();
        for (index, (name, config)) in contexts.iter().enumerate() {
            let color = config
                .color
                .unwrap_or(DEFAULT_PALETTE[index % DEFAULT_PALETTE.len()]);
            context_colors.insert(name.clone(), color);
        }
        Self {
            color_enabled,
            context_colors,
        }
    }

    /// Color assigned to `context_name`.
    #[must_use]
    pub fn context_color(&self, context_name: &str) -> Color {
        if !self.color_enabled {
            return Color::Reset;
        }
        if context_name == UNASSIGNED {
            return Color::DarkGray;
        }
        if context_name == "all" {
            return Color::White;
        }
        self.context_colors
            .iter()
            .find(|(k, _)| k.as_str() == context_name)
            .map_or(Color::Cyan, |(_, &v)| to_ratatui_color(v))
    }

    /// Style for a context badge (`[<context>]`).
    #[must_use]
    pub fn context_badge(&self, context_name: &str) -> Style {
        if !self.color_enabled {
            return Style::default();
        }
        Style::default().fg(self.context_color(context_name)).bold()
    }

    /// Style for task status.
    #[must_use]
    pub fn status_style(&self, status: Option<StatusName>) -> Style {
        if !self.color_enabled {
            return Style::default();
        }
        match status {
            Some(StatusName::Active) => Style::default().fg(Color::Yellow).bold(),
            Some(StatusName::Ready) => Style::default().fg(Color::Green),
            Some(StatusName::Inbox) => Style::default().fg(Color::Cyan),
            Some(StatusName::Waiting | StatusName::Done | StatusName::Cancelled) | None => {
                Style::default().fg(Color::DarkGray)
            }
        }
    }

    /// Style for importance indicators (`p0`..`p3`).
    #[must_use]
    pub fn importance_style(&self, importance: Importance) -> Style {
        if !self.color_enabled {
            return Style::default();
        }
        match importance {
            Importance::P0 => Style::default().fg(Color::LightRed).bold(),
            Importance::P1 => Style::default().fg(Color::LightYellow).bold(),
            Importance::P2 => Style::default().fg(Color::Reset),
            Importance::P3 => Style::default().fg(Color::DarkGray),
        }
    }

    /// Header line style.
    #[must_use]
    pub fn header_style(&self) -> Style {
        if !self.color_enabled {
            return Style::default().add_modifier(Modifier::BOLD);
        }
        Style::default().fg(Color::White).bold()
    }

    /// Selection bar / highlight style.
    #[must_use]
    pub fn selected_style(&self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED)
    }

    /// Dimmed / muted text style.
    #[must_use]
    pub fn dim_style(&self) -> Style {
        if !self.color_enabled {
            return Style::default();
        }
        Style::default().fg(Color::DarkGray)
    }

    /// Error badge or banner style.
    #[must_use]
    pub fn error_style(&self) -> Style {
        if !self.color_enabled {
            return Style::default().bold();
        }
        Style::default().fg(Color::LightRed).bold()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_palette_assigns_distinct_colors_by_config_order() {
        let mut contexts = BTreeMap::new();
        for i in 0..12 {
            let name: ContextName = format!("ctx{i:02}").parse().unwrap();
            contexts.insert(name, ContextConfig::default());
        }
        let theme = Theme::from_config(&contexts, true);
        let mut seen = Vec::new();
        for i in 0..12 {
            let name = format!("ctx{i:02}");
            let color = theme.context_color(&name);
            assert!(
                !seen.contains(&color),
                "color collision at context index {i}: {color:?}"
            );
            seen.push(color);
        }
    }

    #[test]
    fn explicit_color_overrides_default_palette() {
        let mut contexts = BTreeMap::new();
        let name: ContextName = "work".parse().unwrap();
        let cfg = ContextConfig {
            color: Some(ContextColor::BrightYellow),
            ..Default::default()
        };
        contexts.insert(name, cfg);

        let theme = Theme::from_config(&contexts, true);
        assert_eq!(theme.context_color("work"), Color::LightYellow);

        let disabled = Theme::from_config(&contexts, false);
        assert_eq!(disabled.context_color("work"), Color::Reset);
    }
}
