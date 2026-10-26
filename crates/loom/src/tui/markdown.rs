//! Pure rendering of a task body (Markdown) into styled terminal lines.
//!
//! This module covers the Markdown that task notes use: headings, bullet and numbered lists,
//! quotes, fenced code, rules, and the inline marks `**bold**`, `*italic*` and `` `code` ``.
//! It keeps the text of anything else as written. It uses attributes and the terminal's ANSI
//! colors only, so the TUI follows the terminal theme (ADR-0014).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Renders `body` as lines. Lines do not wrap here: the caller wraps them to its width.
#[must_use]
pub fn render(body: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut fence: Option<String> = None;
    for raw in body.lines() {
        if let Some(marker) = &fence {
            if raw.trim_start().starts_with(marker.as_str()) {
                fence = None;
            } else {
                lines.push(Line::styled(format!("  {raw}"), code_style()));
            }
            continue;
        }
        let trimmed = raw.trim_start();
        if let Some(marker) = fence_marker(trimmed) {
            fence = Some(marker);
            continue;
        }
        lines.push(render_line(raw));
    }
    lines
}

fn fence_marker(trimmed: &str) -> Option<String> {
    ["```", "~~~"]
        .iter()
        .find(|marker| trimmed.starts_with(**marker))
        .map(|marker| (*marker).to_owned())
}

fn code_style() -> Style {
    Style::default().fg(Color::Cyan)
}

fn render_line(raw: &str) -> Line<'static> {
    let trimmed = raw.trim_start();
    let indent = &raw[..raw.len() - trimmed.len()];

    if let Some((level, text)) = heading(trimmed) {
        let style = if level == 1 {
            Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        return Line::from(inline(text, style));
    }
    if is_rule(trimmed) {
        return Line::styled("─".repeat(40), Style::default().fg(Color::DarkGray));
    }
    if let Some(text) = trimmed.strip_prefix('>') {
        let mut spans = vec![Span::styled("▎ ", Style::default().fg(Color::DarkGray))];
        spans.extend(inline(
            text.trim_start(),
            Style::default().add_modifier(Modifier::ITALIC),
        ));
        return Line::from(spans);
    }
    if let Some(text) = bullet(trimmed) {
        let mut spans = vec![Span::raw(format!("{indent}• "))];
        spans.extend(inline(text, Style::default()));
        return Line::from(spans);
    }
    if let Some((number, text)) = numbered(trimmed) {
        let mut spans = vec![Span::raw(format!("{indent}{number}. "))];
        spans.extend(inline(text, Style::default()));
        return Line::from(spans);
    }
    let mut spans = vec![Span::raw(indent.to_owned())];
    spans.extend(inline(trimmed, Style::default()));
    Line::from(spans)
}

/// `# text` to `###### text`: the level and the text.
fn heading(trimmed: &str) -> Option<(usize, &str)> {
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let text = trimmed[level..].strip_prefix(' ')?;
    Some((level, text.trim()))
}

fn is_rule(trimmed: &str) -> bool {
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3
        && ["-", "*", "_"]
            .iter()
            .any(|m| compact.chars().all(|c| m.starts_with(c)))
}

fn bullet(trimmed: &str) -> Option<&str> {
    ["- ", "* ", "+ "]
        .iter()
        .find_map(|marker| trimmed.strip_prefix(marker))
}

fn numbered(trimmed: &str) -> Option<(&str, &str)> {
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = trimmed[digits..].strip_prefix(". ")?;
    Some((&trimmed[..digits], rest))
}

/// Splits `text` into spans for `**bold**`, `*italic*` and `` `code` ``, on top of `base`.
/// A mark with no closing partner stays as written.
fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let found = [
            ("**", base.add_modifier(Modifier::BOLD)),
            ("`", code_style()),
            ("*", base.add_modifier(Modifier::ITALIC)),
        ]
        .into_iter()
        .find_map(|(mark, style)| {
            let inner = rest.strip_prefix(mark)?;
            let end = inner.find(mark)?;
            (end > 0).then_some((mark.len(), end, style))
        });
        if let Some((mark_len, end, style)) = found {
            if !plain.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut plain), base));
            }
            spans.push(Span::styled(
                rest[mark_len..mark_len + end].to_owned(),
                style,
            ));
            rest = &rest[mark_len + end + mark_len..];
        } else if let Some(c) = rest.chars().next() {
            plain.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    if !plain.is_empty() {
        spans.push(Span::styled(plain, base));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn has(line: &Line<'_>, content: &str, modifier: Modifier) -> bool {
        line.spans
            .iter()
            .any(|s| s.content == content && s.style.add_modifier.contains(modifier))
    }

    #[test]
    fn plain_text_is_kept_as_written() {
        let lines = render("Mobile users time out.\n\nSecond paragraph.");
        assert_eq!(lines.len(), 3);
        assert_eq!(text(&lines[0]), "Mobile users time out.");
        assert_eq!(text(&lines[1]), "");
        assert_eq!(text(&lines[2]), "Second paragraph.");
    }

    #[test]
    fn a_heading_loses_its_marks_and_is_bold() {
        let lines = render("## Symptoms");
        assert_eq!(text(&lines[0]), "Symptoms");
        assert!(has(&lines[0], "Symptoms", Modifier::BOLD));
    }

    #[test]
    fn a_hash_without_a_space_is_not_a_heading() {
        assert_eq!(text(&render("#tag")[0]), "#tag");
        assert_eq!(text(&render("####### seven")[0]), "####### seven");
    }

    #[test]
    fn lists_get_bullets_and_keep_numbers_and_indent() {
        let lines = render("- one\n  * nested\n3. three");
        assert_eq!(text(&lines[0]), "• one");
        assert_eq!(text(&lines[1]), "  • nested");
        assert_eq!(text(&lines[2]), "3. three");
    }

    #[test]
    fn inline_marks_are_styled_and_unmatched_marks_stay() {
        let lines = render("a **bold** b *it* c `code` d **open");
        assert_eq!(text(&lines[0]), "a bold b it c code d **open");
        assert!(has(&lines[0], "bold", Modifier::BOLD));
        assert!(has(&lines[0], "it", Modifier::ITALIC));
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.content == "code" && s.style.fg == Some(Color::Cyan))
        );
    }

    #[test]
    fn a_code_fence_is_literal_and_hides_its_marks() {
        let lines = render("```rust\nlet **x** = 1;\n```\nafter");
        assert_eq!(lines.len(), 2);
        assert_eq!(text(&lines[0]), "  let **x** = 1;");
        assert_eq!(text(&lines[1]), "after");
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        let lines = render("```\none\ntwo");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn a_quote_and_a_rule_are_drawn() {
        let lines = render("> quoted\n---");
        assert_eq!(text(&lines[0]), "▎ quoted");
        assert!(text(&lines[1]).starts_with('─'));
    }

    #[test]
    fn multibyte_text_does_not_split_a_character() {
        let lines = render("café *crème* — 日本語");
        assert_eq!(text(&lines[0]), "café crème — 日本語");
    }

    #[test]
    fn an_empty_body_has_no_lines() {
        assert_eq!(render("").len(), 0);
    }
}
