//! Pure rendering of a task body (Markdown) into styled terminal lines.
//!
//! This module covers the Markdown that task notes use: headings, bullet and numbered lists,
//! quotes, fenced code, rules, and the inline marks `**bold**`, `*italic*` and `` `code` ``.
//! It keeps the text of anything else as written. It uses attributes and the terminal's ANSI
//! colors only, so the TUI follows the terminal theme (ADR-0014).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Renders `body` as lines. Text lines do not wrap here: the caller wraps them. `width` limits
/// the horizontal rule, which the caller cannot wrap.
#[must_use]
pub fn render(body: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut fence: Option<Fence> = None;
    for raw in body.lines() {
        if let Some(open) = fence {
            if open.is_closed_by(raw) {
                fence = None;
            } else {
                lines.push(Line::styled(format!("  {raw}"), code_style()));
            }
            continue;
        }
        if let Some(open) = Fence::opened_by(raw) {
            fence = Some(open);
            continue;
        }
        lines.push(render_line(raw, width));
    }
    lines
}

/// An open code fence: its marker character and the length of its opening run.
#[derive(Clone, Copy)]
struct Fence {
    marker: char,
    len: usize,
}

impl Fence {
    /// A run of three or more `` ` `` or `~` opens a fence.
    fn opened_by(line: &str) -> Option<Self> {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
        let len = trimmed.chars().take_while(|c| *c == marker).count();
        (len >= 3).then_some(Self { marker, len })
    }

    /// A line of the same marker, at least as long as the opening run, and nothing else.
    fn is_closed_by(self, line: &str) -> bool {
        let trimmed = line.trim();
        let len = trimmed.chars().take_while(|c| *c == self.marker).count();
        len >= self.len && len == trimmed.chars().count()
    }
}

fn code_style() -> Style {
    Style::default().fg(Color::Cyan)
}

fn render_line(raw: &str, width: usize) -> Line<'static> {
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
        return Line::styled(
            "─".repeat(width.clamp(1, 40)),
            Style::default().fg(Color::DarkGray),
        );
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
///
/// `*` follows the CommonMark flanking rule: an opening mark has a non-space after it and a
/// closing mark has a non-space before it. A single `*` between word characters stays as
/// written. A mark with no valid partner stays as written. `_` is never a mark.
fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut previous: Option<char> = None;
    let mut rest = text;
    while !rest.is_empty() {
        if let Some((mark_len, end, style)) = mark_at(rest, previous, base) {
            if !plain.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut plain), base));
            }
            let inner = &rest[mark_len..mark_len + end];
            spans.push(Span::styled(inner.to_owned(), style));
            previous = inner.chars().last();
            rest = &rest[mark_len + end + mark_len..];
        } else if let Some(c) = rest.chars().next() {
            plain.push(c);
            previous = Some(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    if !plain.is_empty() {
        spans.push(Span::styled(plain, base));
    }
    spans
}

/// The mark that starts `rest`: its length, the length of its content, and the style.
fn mark_at(rest: &str, previous: Option<char>, base: Style) -> Option<(usize, usize, Style)> {
    if let Some(inner) = rest.strip_prefix('`') {
        let end = inner.find('`').filter(|end| *end > 0)?;
        return Some((1, end, code_style()));
    }
    let (mark, style) = if rest.starts_with("**") {
        ("**", base.add_modifier(Modifier::BOLD))
    } else if rest.starts_with('*') {
        // `a*b*c`: a star after a word character does not open emphasis.
        if previous.is_some_and(char::is_alphanumeric) {
            return None;
        }
        ("*", base.add_modifier(Modifier::ITALIC))
    } else {
        return None;
    };
    let inner = &rest[mark.len()..];
    if inner.chars().next().is_none_or(char::is_whitespace) {
        return None;
    }
    let end = closing(inner, mark)?;
    Some((mark.len(), end, style))
}

/// Byte offset of the first valid closing `mark` in `inner`.
fn closing(inner: &str, mark: &str) -> Option<usize> {
    inner.match_indices(mark).find_map(|(at, _)| {
        let before = inner[..at].chars().last()?;
        let after = inner[at + mark.len()..].chars().next();
        let valid =
            !before.is_whitespace() && (mark == "**" || !after.is_some_and(char::is_alphanumeric));
        valid.then_some(at)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render80(body: &str) -> Vec<Line<'static>> {
        render(body, 80)
    }

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
        let lines = render80("Mobile users time out.\n\nSecond paragraph.");
        assert_eq!(lines.len(), 3);
        assert_eq!(text(&lines[0]), "Mobile users time out.");
        assert_eq!(text(&lines[1]), "");
        assert_eq!(text(&lines[2]), "Second paragraph.");
    }

    #[test]
    fn a_heading_loses_its_marks_and_is_bold() {
        let lines = render80("## Symptoms");
        assert_eq!(text(&lines[0]), "Symptoms");
        assert!(has(&lines[0], "Symptoms", Modifier::BOLD));
    }

    #[test]
    fn a_hash_without_a_space_is_not_a_heading() {
        assert_eq!(text(&render80("#tag")[0]), "#tag");
        assert_eq!(text(&render80("####### seven")[0]), "####### seven");
    }

    #[test]
    fn lists_get_bullets_and_keep_numbers_and_indent() {
        let lines = render80("- one\n  * nested\n3. three");
        assert_eq!(text(&lines[0]), "• one");
        assert_eq!(text(&lines[1]), "  • nested");
        assert_eq!(text(&lines[2]), "3. three");
    }

    #[test]
    fn inline_marks_are_styled_and_unmatched_marks_stay() {
        let lines = render80("a **bold** b *it* c `code` d **open");
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
        let lines = render80("```rust\nlet **x** = 1;\n```\nafter");
        assert_eq!(lines.len(), 2);
        assert_eq!(text(&lines[0]), "  let **x** = 1;");
        assert_eq!(text(&lines[1]), "after");
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        let lines = render80("```\none\ntwo");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn a_quote_and_a_rule_are_drawn() {
        let lines = render80("> quoted\n---");
        assert_eq!(text(&lines[0]), "▎ quoted");
        assert!(text(&lines[1]).starts_with('─'));
    }

    #[test]
    fn multibyte_text_does_not_split_a_character() {
        let lines = render80("café *crème* — 日本語");
        assert_eq!(text(&lines[0]), "café crème — 日本語");
    }

    #[test]
    fn stars_around_spaces_are_literal() {
        assert_eq!(text(&render80("2 * 3 * 4")[0]), "2 * 3 * 4");
        assert_eq!(text(&render80("*.rs and *.toml")[0]), "*.rs and *.toml");
        assert_eq!(text(&render80("a*b*c")[0]), "a*b*c");
        assert_eq!(text(&render80("** not bold **")[0]), "** not bold **");
        let lines = render80("*.rs and *.toml files* end");
        assert_eq!(text(&lines[0]), ".rs and *.toml files end");
        assert!(has(&lines[0], ".rs and *.toml files", Modifier::ITALIC));
    }

    #[test]
    fn underscores_are_never_marks() {
        assert_eq!(
            text(&render80("snake_case_name and _x_")[0]),
            "snake_case_name and _x_"
        );
    }

    #[test]
    fn a_fence_closes_only_on_the_same_marker_and_length() {
        // A shorter run, another marker, or trailing text does not close the fence.
        let lines = render80("````\n```\n~~~~\n```` x\n````\nafter");
        assert_eq!(lines.len(), 4);
        assert_eq!(text(&lines[0]), "  ```");
        assert_eq!(text(&lines[1]), "  ~~~~");
        assert_eq!(text(&lines[2]), "  ```` x");
        assert_eq!(text(&lines[3]), "after");
    }

    #[test]
    fn a_longer_closing_run_closes_the_fence() {
        let lines = render80("```\ncode\n`````  \nafter");
        assert_eq!(lines.len(), 2);
        assert_eq!(text(&lines[1]), "after");
    }

    #[test]
    fn a_rule_never_exceeds_the_width() {
        assert_eq!(text(&render("---", 10)[0]).chars().count(), 10);
        assert_eq!(text(&render("---", 100)[0]).chars().count(), 40);
    }

    #[test]
    fn an_empty_body_has_no_lines() {
        assert_eq!(render80("").len(), 0);
    }
}
