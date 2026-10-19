//! `:` fuzzy command palette over views, actions and contexts (ADR-0014).

use loom_api::ContextName;

use crate::tui::keymap::QueueTab;

/// An entry in the `:` command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    /// Switch to a queue view.
    SwitchView(QueueTab),
    /// Filter queues to a single context, or clear the context filter when `None`.
    FilterContext(Option<ContextName>),
    /// Toggle the right-hand preview pane.
    TogglePreview,
    /// Refetch data from `loomd`.
    Refresh,
    /// Open `?` key bindings help.
    OpenHelp,
    /// Open `/` search modal.
    OpenSearch,
    /// Quit the TUI.
    Quit,
}

/// A candidate row in the command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    /// Category prefix (`view`, `action`, `context`).
    pub category: &'static str,
    /// Primary text matched and displayed.
    pub label: String,
    /// Key hint shown on the right when one exists.
    pub shortcut: &'static str,
    /// Action executed when selected.
    pub action: PaletteAction,
}

/// Builds the full list of command palette items for the known `contexts`.
#[must_use]
pub fn build_palette_items(contexts: &[ContextName]) -> Vec<PaletteItem> {
    let mut items = vec![
        PaletteItem {
            category: "view",
            label: "Today queue".to_owned(),
            shortcut: "1",
            action: PaletteAction::SwitchView(QueueTab::Today),
        },
        PaletteItem {
            category: "view",
            label: "Inbox queue".to_owned(),
            shortcut: "2",
            action: PaletteAction::SwitchView(QueueTab::Inbox),
        },
        PaletteItem {
            category: "view",
            label: "Waiting queue".to_owned(),
            shortcut: "3",
            action: PaletteAction::SwitchView(QueueTab::Waiting),
        },
        PaletteItem {
            category: "view",
            label: "Repos queue".to_owned(),
            shortcut: "4",
            action: PaletteAction::SwitchView(QueueTab::Repos),
        },
        PaletteItem {
            category: "action",
            label: "Toggle preview pane".to_owned(),
            shortcut: "|",
            action: PaletteAction::TogglePreview,
        },
        PaletteItem {
            category: "action",
            label: "Refresh data".to_owned(),
            shortcut: "r",
            action: PaletteAction::Refresh,
        },
        PaletteItem {
            category: "action",
            label: "Search tasks and repositories".to_owned(),
            shortcut: "/",
            action: PaletteAction::OpenSearch,
        },
        PaletteItem {
            category: "action",
            label: "Show key bindings help".to_owned(),
            shortcut: "?",
            action: PaletteAction::OpenHelp,
        },
        PaletteItem {
            category: "context",
            label: "All contexts (clear filter)".to_owned(),
            shortcut: "@",
            action: PaletteAction::FilterContext(None),
        },
    ];
    for ctx in contexts {
        items.push(PaletteItem {
            category: "context",
            label: format!("@{ctx} (filter context)"),
            shortcut: "@",
            action: PaletteAction::FilterContext(Some(ctx.clone())),
        });
    }
    items.push(PaletteItem {
        category: "action",
        label: "Quit loom".to_owned(),
        shortcut: "q",
        action: PaletteAction::Quit,
    });
    items
}

/// Scores `query` as a case-insensitive subsequence of `candidate`.
///
/// Returns `None` when any non-whitespace character of `query` is missing in order. Higher scores
/// indicate tighter matches (prefix match, consecutive character runs, word-boundary hits).
#[must_use]
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i64> {
    let query_chars: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query_chars.is_empty() {
        return Some(0);
    }
    let cand_chars: Vec<char> = candidate.chars().flat_map(char::to_lowercase).collect();
    let mut q_idx = 0;
    let mut score: i64 = 0;
    let mut prev_match_idx: Option<usize> = None;

    for (c_idx, &ch) in cand_chars.iter().enumerate() {
        if q_idx < query_chars.len() && ch == query_chars[q_idx] {
            score += 10;
            if c_idx == 0 {
                score += 15;
            } else if matches!(
                cand_chars.get(c_idx.saturating_sub(1)),
                Some(' ' | '-' | '_' | '@' | ':')
            ) {
                score += 12;
            }
            if let Some(prev) = prev_match_idx {
                if c_idx == prev + 1 {
                    score += 8;
                } else {
                    score -= i64::try_from(c_idx - prev - 1).unwrap_or(0);
                }
            }
            prev_match_idx = Some(c_idx);
            q_idx += 1;
        }
    }

    if q_idx == query_chars.len() {
        Some(score)
    } else {
        None
    }
}

/// Filters and ranks `items` against `query`.
#[must_use]
pub fn filter_palette(items: &[PaletteItem], query: &str) -> Vec<PaletteItem> {
    if query.trim().is_empty() {
        return items.to_vec();
    }
    let mut scored: Vec<(i64, usize, &PaletteItem)> = items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let full = format!("{} {}", item.category, item.label);
            let score = fuzzy_score(query, &item.label).or_else(|| fuzzy_score(query, &full))?;
            Some((score, idx, item))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .map(|(_, _, item)| item.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matcher_ranks_tighter_matches_first() {
        let work: ContextName = "company-a".parse().unwrap();
        let perso: ContextName = "personal".parse().unwrap();
        let items = build_palette_items(&[work.clone(), perso]);

        let filtered = filter_palette(&items, "inb");
        assert_ne!(filtered.len(), 0);
        assert_eq!(
            filtered[0].action,
            PaletteAction::SwitchView(QueueTab::Inbox)
        );

        let ctx_filtered = filter_palette(&items, "compa");
        assert_ne!(ctx_filtered.len(), 0);
        assert_eq!(
            ctx_filtered[0].action,
            PaletteAction::FilterContext(Some(work))
        );

        assert_eq!(filter_palette(&items, "zzzznotfound").len(), 0);
    }
}
