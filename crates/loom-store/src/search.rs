//! `loom search` over the repositories: full-text matching by the `repos_fts` table (FTS5),
//! ranking by [`rank`].
//!
//! A query is split on whitespace; every term must match (AND), each as the prefix of a word.
//! Terms are quoted, so FTS5 syntax (`"`, `*`, `:`, `(`, `)`, `^`, `-`, `+`, `NEAR`, `AND`,
//! `OR`, `NOT`) is searched as text, never run as an operator. Case and diacritics are folded
//! by the table's tokenizer (`unicode61 remove_diacritics 2`), which also splits words on
//! every character that is not a letter or a digit.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::PathBuf;

use loom_api::{SearchField, SearchKind, SearchResult};
use rusqlite::{Connection, params};

use crate::StoreError;

/// Marks around a matched word in `highlight()` output: characters no indexed text holds.
const OPEN: char = '\u{1}';
const CLOSE: char = '\u{2}';

/// How well a repository matches, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// The name's words are the query's words, in order (`loom-api` for `Loom API`).
    ExactName,
    /// Every term starts a word of the name.
    NamePrefix,
    /// Every term starts a word of the slug.
    Slug,
    /// The terms are spread over the fields (path, remotes, context, branch…).
    Other,
}

impl Tier {
    /// The tier of a repository, from what matched: its whole name, every term in its name,
    /// every term in its slug.
    #[must_use]
    pub const fn of(exact_name: bool, name: bool, slug: bool) -> Self {
        match (exact_name, name, slug) {
            (true, _, _) => Self::ExactName,
            (false, true, _) => Self::NamePrefix,
            (false, false, true) => Self::Slug,
            (false, false, false) => Self::Other,
        }
    }
}

/// A match before ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The result, as returned.
    pub result: SearchResult,
    /// How well it matches.
    pub tier: Tier,
    /// `last_used` in Unix seconds; `None` when never used.
    pub last_used: Option<i64>,
    /// FTS5 `bm25()`: lower is more relevant.
    pub bm25: f64,
}

/// Sorts `candidates` best first: by [`Tier`], then most recently used (never used last),
/// then by `bm25`, then by slug, so that the order is total and stable.
pub fn rank(candidates: &mut [Candidate]) {
    candidates.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| recency(a.last_used, b.last_used))
            .then_with(|| a.bm25.total_cmp(&b.bm25))
            .then_with(|| a.result.slug.cmp(&b.result.slug))
    });
}

/// Most recent first, never used last.
fn recency(a: Option<i64>, b: Option<i64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// The terms of `query`: its whitespace-separated words holding at least one letter or digit.
/// A word of punctuation only would be an empty phrase for FTS5, which matches nothing.
#[must_use]
pub fn terms(query: &str) -> Vec<&str> {
    query
        .split_whitespace()
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .collect()
}

/// The FTS5 expressions of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expressions {
    /// Every term, as a prefix, in any column.
    pub all: String,
    /// Every term, as a prefix, in the name.
    pub name: String,
    /// Every term, as a prefix, in the slug.
    pub slug: String,
    /// The terms as one phrase starting the name; whole words.
    pub exact_name: String,
}

/// The expressions for `terms`; `None` without terms.
#[must_use]
pub fn expressions(terms: &[&str]) -> Option<Expressions> {
    if terms.is_empty() {
        return None;
    }
    let all = terms
        .iter()
        .map(|term| format!("{}*", quoted(term)))
        .collect::<Vec<_>>()
        .join(" AND ");
    Some(Expressions {
        name: format!("{{name}} : ({all})"),
        slug: format!("{{slug}} : ({all})"),
        exact_name: format!("{{name}} : ^{}", quoted(&terms.join(" "))),
        all,
    })
}

/// `text` as an FTS5 string: in double quotes, inner double quotes doubled.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

/// Whether `highlighted` (a column through `highlight()` with [`OPEN`] and [`CLOSE`]) has every
/// letter and digit inside a match.
#[must_use]
pub fn covers_whole(highlighted: &str) -> bool {
    let mut inside = false;
    for c in highlighted.chars() {
        match c {
            OPEN => inside = true,
            CLOSE => inside = false,
            c if !inside && c.is_alphanumeric() => return false,
            _ => {}
        }
    }
    true
}

/// The fields whose highlighted text (in column order) holds a match.
fn matched_fields(highlighted: &[Option<String>; 6]) -> Vec<SearchField> {
    SearchField::ALL
        .into_iter()
        .zip(highlighted)
        .filter(|(_, text)| text.as_deref().is_some_and(|text| text.contains(OPEN)))
        .map(|(field, _)| field)
        .collect()
}

/// The repositories matching `query` (in `context` when given), ranked, at most `limit`, read
/// in one snapshot.
///
/// # Errors
///
/// [`StoreError`] when a query fails.
pub fn search(
    conn: &Connection,
    query: &str,
    context: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchResult>, StoreError> {
    let Some(expressions) = expressions(&terms(query)) else {
        return Ok(Vec::new());
    };
    let tx = conn.unchecked_transaction()?;
    let exact = exact_names(&tx, &expressions.exact_name)?;
    let mut statement = tx.prepare(
        "SELECT repos_fts.rowid, r.slug, r.name, r.context, r.path, r.last_used, \
                bm25(repos_fts), \
                highlight(repos_fts, 0, char(1), char(2)), \
                highlight(repos_fts, 1, char(1), char(2)), \
                highlight(repos_fts, 2, char(1), char(2)), \
                highlight(repos_fts, 3, char(1), char(2)), \
                highlight(repos_fts, 4, char(1), char(2)), \
                highlight(repos_fts, 5, char(1), char(2)), \
                repos_fts.rowid IN (SELECT rowid FROM repos_fts WHERE repos_fts MATCH ?2), \
                repos_fts.rowid IN (SELECT rowid FROM repos_fts WHERE repos_fts MATCH ?3) \
         FROM repos_fts JOIN repos r ON r.id = repos_fts.rowid \
         WHERE repos_fts MATCH ?1 AND (?4 IS NULL OR r.context = ?4)",
    )?;
    let rows = statement.query_map(
        params![expressions.all, expressions.name, expressions.slug, context],
        |row| {
            let id: i64 = row.get(0)?;
            let bm25: f64 = row.get(6)?;
            let highlighted = [
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
            ];
            Ok(Candidate {
                result: SearchResult {
                    kind: SearchKind::Repo,
                    slug: row.get(1)?,
                    name: row.get(2)?,
                    context: row.get(3)?,
                    path: PathBuf::from(row.get::<_, String>(4)?),
                    score: -bm25,
                    matched_fields: matched_fields(&highlighted),
                },
                tier: Tier::of(exact.contains(&id), row.get(13)?, row.get(14)?),
                last_used: row.get(5)?,
                bm25,
            })
        },
    )?;
    let mut candidates = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    tx.finish()?;
    rank(&mut candidates);
    Ok(candidates
        .into_iter()
        .take(limit)
        .map(|candidate| candidate.result)
        .collect())
}

/// Row ids of the repositories whose whole name the phrase `expression` matches.
fn exact_names(conn: &Connection, expression: &str) -> Result<HashSet<i64>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT rowid, highlight(repos_fts, 0, char(1), char(2)) FROM repos_fts \
         WHERE repos_fts MATCH ?1",
    )?;
    let rows = statement.query_map([expression], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut exact = HashSet::new();
    for row in rows {
        let (id, name) = row?;
        if covers_whole(&name) {
            exact.insert(id);
        }
    }
    Ok(exact)
}

#[cfg(test)]
mod tests {
    use loom_api::{SearchField, SearchKind, SearchResult};

    use super::{Candidate, Expressions, Tier, covers_whole, expressions, rank, terms};

    fn candidate(slug: &str, tier: Tier, last_used: Option<i64>, bm25: f64) -> Candidate {
        Candidate {
            result: SearchResult {
                kind: SearchKind::Repo,
                slug: slug.to_owned(),
                name: slug.rsplit('/').next().unwrap_or(slug).to_owned(),
                context: "p".to_owned(),
                path: format!("/w/{slug}").into(),
                score: -bm25,
                matched_fields: vec![SearchField::Name],
            },
            tier,
            last_used,
            bm25,
        }
    }

    fn ranked(mut candidates: Vec<Candidate>) -> Vec<String> {
        rank(&mut candidates);
        candidates
            .into_iter()
            .map(|candidate| candidate.result.slug)
            .collect()
    }

    #[test]
    fn ranking_table() {
        use Tier::{ExactName, NamePrefix, Other, Slug};
        let cases: Vec<(&str, Vec<Candidate>, Vec<&str>)> = vec![
            (
                "tier first, whatever the recency and relevance",
                vec![
                    candidate("p/other", Other, Some(900), -9.0),
                    candidate("p/slug", Slug, Some(800), -8.0),
                    candidate("p/prefix", NamePrefix, Some(700), -7.0),
                    candidate("p/exact", ExactName, None, -1.0),
                ],
                vec!["p/exact", "p/prefix", "p/slug", "p/other"],
            ),
            (
                "same tier: most recently used first, never used last",
                vec![
                    candidate("p/never", NamePrefix, None, -9.0),
                    candidate("p/old", NamePrefix, Some(10), -1.0),
                    candidate("p/new", NamePrefix, Some(20), -1.0),
                ],
                vec!["p/new", "p/old", "p/never"],
            ),
            (
                "same tier and recency: lower bm25 first",
                vec![
                    candidate("p/a", Other, Some(5), -1.0),
                    candidate("p/b", Other, Some(5), -3.0),
                    candidate("p/c", Other, Some(5), -2.0),
                ],
                vec!["p/b", "p/c", "p/a"],
            ),
            (
                "full tie: by slug",
                vec![
                    candidate("p/z", Slug, None, -1.0),
                    candidate("p/a", Slug, None, -1.0),
                    candidate("o/m", Slug, None, -1.0),
                ],
                vec!["o/m", "p/a", "p/z"],
            ),
            ("nothing", vec![], vec![]),
        ];
        for (name, candidates, expected) in cases {
            assert_eq!(ranked(candidates), expected, "{name}");
        }
    }

    #[test]
    fn ranking_is_stable_under_permutation() {
        let all = || {
            vec![
                candidate("p/a", Tier::Other, None, -1.0),
                candidate("p/b", Tier::Other, Some(3), -1.0),
                candidate("p/c", Tier::Slug, None, -2.0),
                candidate("p/d", Tier::ExactName, Some(1), -0.5),
            ]
        };
        let expected = ranked(all());
        let mut reversed = all();
        reversed.reverse();
        assert_eq!(ranked(reversed), expected);
        assert_eq!(expected, ["p/d", "p/c", "p/b", "p/a"]);
    }

    #[test]
    fn tier_table() {
        let cases = [
            ((true, true, true), Tier::ExactName),
            ((true, false, false), Tier::ExactName),
            ((false, true, true), Tier::NamePrefix),
            ((false, true, false), Tier::NamePrefix),
            ((false, false, true), Tier::Slug),
            ((false, false, false), Tier::Other),
        ];
        for ((exact, name, slug), expected) in cases {
            assert_eq!(
                Tier::of(exact, name, slug),
                expected,
                "{exact} {name} {slug}"
            );
        }
    }

    #[test]
    fn terms_split_on_whitespace_and_drop_punctuation_only_words() {
        assert_eq!(terms("  loom\tapi \n"), ["loom", "api"]);
        assert_eq!(terms("- loom -- * \"\""), ["loom"]);
        assert_eq!(terms("café 42"), ["café", "42"]);
        assert_eq!(terms(""), [] as [&str; 0]);
        assert_eq!(terms("  "), [] as [&str; 0]);
    }

    #[test]
    fn expressions_quote_every_term() {
        assert_eq!(expressions(&[]), None);
        assert_eq!(
            expressions(&["lo", "api"]),
            Some(Expressions {
                all: r#""lo"* AND "api"*"#.to_owned(),
                name: r#"{name} : ("lo"* AND "api"*)"#.to_owned(),
                slug: r#"{slug} : ("lo"* AND "api"*)"#.to_owned(),
                exact_name: r#"{name} : ^"lo api""#.to_owned(),
            })
        );
        // FTS5 syntax stays inside quotes; inner quotes are doubled.
        let syntax = expressions(&["NEAR(a", "b\"c", "x:y", "-z", "^w", "OR"]).unwrap();
        assert_eq!(
            syntax.all,
            r#""NEAR(a"* AND "b""c"* AND "x:y"* AND "-z"* AND "^w"* AND "OR"*"#
        );
    }

    #[test]
    fn covers_whole_needs_every_letter_and_digit_inside_a_match() {
        assert!(covers_whole("\u{1}loom\u{2}"));
        assert!(covers_whole("\u{1}loom-api\u{2}"));
        assert!(covers_whole("\u{1}loom\u{2}-\u{1}api\u{2}."));
        assert!(!covers_whole("\u{1}loom\u{2}-api"));
        assert!(!covers_whole("x\u{1}loom\u{2}"));
        assert!(!covers_whole("loom"));
    }
}
