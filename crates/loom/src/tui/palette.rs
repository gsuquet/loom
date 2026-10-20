//! Command palette fuzzy matcher and actions list (ADR-0014).
//!
//! Opened with `:`, filters available commands and views as the user types.

use loom_api::ContextName;

use super::keymap::QueueTab;

/// One selectable action in the `:` command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    /// Select a queue tab.
    SelectTab(QueueTab),
    /// Open the `/` search modal.
    OpenSearch,
    /// Open the `@` context picker modal.
    OpenContextPicker,
    /// Set context filter to `all` or a specific context.
    SetContext(Option<ContextName>),
    /// Toggle the preview pane on or off.
    TogglePreview,
    /// Refresh data from loomd.
    Refresh,
    /// Open `?` help.
    OpenHelp,
    /// Exit the TUI.
    Quit,
}

/// One row in the command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    /// Action triggered on confirmation.
    pub action: PaletteAction,
    /// Display category (e.g. `queue`, `context`, `action`).
    pub category: &'static str,
    /// Display name.
    pub name: String,
}

impl PaletteItem {
    /// Builds a searchable display string: `<category>: <name>`.
    #[must_use]
    pub fn display(&self) -> String {
        format!("{}: {}", self.category, self.name)
    }
}

/// Builds all standard palette actions for the given `contexts`.
#[must_use]
pub fn build_palette_items(contexts: &[ContextName]) -> Vec<PaletteItem> {
    let mut items = vec![
        PaletteItem {
            action: PaletteAction::SelectTab(QueueTab::Today),
            category: "queue",
            name: "Today".to_owned(),
        },
        PaletteItem {
            action: PaletteAction::SelectTab(QueueTab::Inbox),
            category: "queue",
            name: "Inbox".to_owned(),
        },
        PaletteItem {
            action: PaletteAction::SelectTab(QueueTab::Waiting),
            category: "queue",
            name: "Waiting".to_owned(),
        },
        PaletteItem {
            action: PaletteAction::OpenSearch,
            category: "action",
            name: "Search (/...)".to_owned(),
        },
        PaletteItem {
            action: PaletteAction::OpenContextPicker,
            category: "action",
            name: "Pick context filter (@)".to_owned(),
        },
        PaletteItem {
            action: PaletteAction::TogglePreview,
            category: "action",
            name: "Toggle preview pane (|)".to_owned(),
        },
        PaletteItem {
            action: PaletteAction::Refresh,
            category: "action",
            name: "Refresh from loomd (r)".to_owned(),
        },
        PaletteItem {
            action: PaletteAction::OpenHelp,
            category: "action",
            name: "Help (?)".to_owned(),
        },
        PaletteItem {
            action: PaletteAction::Quit,
            category: "action",
            name: "Quit loom (q)".to_owned(),
        },
        PaletteItem {
            action: PaletteAction::SetContext(None),
            category: "context",
            name: "all (show all contexts)".to_owned(),
        },
    ];

    for ctx in contexts {
        items.push(PaletteItem {
            action: PaletteAction::SetContext(Some(ctx.clone())),
            category: "context",
            name: ctx.to_string(),
        });
    }

    items
}

/// Filters and scores `items` against `query`.
#[must_use]
pub fn filter_palette(items: &[PaletteItem], query: &str) -> Vec<(usize, PaletteItem)> {
    let query_lower = query.trim().to_lowercase();
    if query_lower.is_empty() {
        return items
            .iter()
            .enumerate()
            .map(|(i, item)| (i, item.clone()))
            .collect();
    }

    let mut matches = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let display = item.display().to_lowercase();
        if let Some(score) = score_match(&display, &query_lower) {
            matches.push((score, index, item.clone()));
        }
    }

    // Sort by match score descending, then original index ascending.
    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    matches.into_iter().map(|(_, i, item)| (i, item)).collect()
}

#[allow(clippy::cast_possible_truncation)]
fn score_match(haystack: &str, needle: &str) -> Option<u32> {
    if let Some(pos) = haystack.find(needle) {
        // Substring match gets higher score; earlier position is better.
        return Some(1000 - pos.min(900) as u32);
    }

    // Subsequence match
    let mut score = 0u32;
    let mut hay_chars = haystack.char_indices();
    for n in needle.chars() {
        let mut matched = false;
        for (_, h) in hay_chars.by_ref() {
            if h == n {
                matched = true;
                score += 10;
                break;
            }
        }
        if !matched {
            return None;
        }
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_palette_items_by_query() {
        let ctx: ContextName = "work".parse().unwrap();
        let items = build_palette_items(&[ctx]);

        let matched = filter_palette(&items, "today");
        assert_eq!(matched.len(), 1);
        assert_eq!(
            matched[0].1.action,
            PaletteAction::SelectTab(QueueTab::Today)
        );

        let prefix_matched = filter_palette(&items, "tod");
        assert_ne!(prefix_matched.len(), 0);
        assert_eq!(
            prefix_matched[0].1.action,
            PaletteAction::SelectTab(QueueTab::Today)
        );

        let context_matches = filter_palette(&items, "work");
        assert_ne!(context_matches.len(), 0);
        assert_eq!(
            context_matches[0].1.action,
            PaletteAction::SetContext(Some("work".parse().unwrap()))
        );
    }
}
