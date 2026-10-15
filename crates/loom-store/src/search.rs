//! `loom search` over the repositories and the tasks: full-text matching by the `repos_fts` and
//! `tasks_fts` tables (FTS5), ranking by [`rank_repos`], [`rank_tasks`] and [`merge`].
//!
//! A query is split on whitespace; every term must match (AND), each as the prefix of a word.
//! Terms are quoted, so FTS5 syntax (`"`, `*`, `:`, `(`, `)`, `^`, `-`, `+`, `NEAR`, `AND`,
//! `OR`, `NOT`) is searched as text, never run as an operator. Case and diacritics are folded
//! by the tables' tokenizer (`unicode61 remove_diacritics 2`), which also splits words on
//! every character that is not a letter or a digit.
//!
//! Repositories and tasks are read in one snapshot, then ranked by [`Group`] first: active and
//! ready tasks, then repositories, then the inbox, then closed tasks, then archived tasks.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::PathBuf;

use loom_api::{RepoMatch, SearchField, SearchKind, SearchResult, TaskMatch, TaskSearchField};
use loom_core::{ContextName, StatusName, TaskId};
use rusqlite::{Connection, params};

use crate::StoreError;

/// Marks around a matched word in `highlight()` output: characters no indexed text holds.
const OPEN: char = '\u{1}';
const CLOSE: char = '\u{2}';

/// Where a result ranks, before anything else: groups come in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    /// Tasks being worked on or next: active, ready, waiting.
    Current,
    /// Repositories.
    Repo,
    /// Tasks to triage, and tasks whose status or file this loom cannot read.
    Inbox,
    /// Done and cancelled tasks.
    Closed,
    /// Archived tasks.
    Archived,
}

impl Group {
    /// The group of a task with status `status` (`None` for a status written by a newer loom,
    /// or an invalid `task.md`), `archived` or not.
    #[must_use]
    pub const fn of_task(status: Option<StatusName>, archived: bool) -> Self {
        if archived {
            return Self::Archived;
        }
        match status {
            Some(StatusName::Active | StatusName::Ready | StatusName::Waiting) => Self::Current,
            Some(StatusName::Inbox) | None => Self::Inbox,
            Some(StatusName::Done | StatusName::Cancelled) => Self::Closed,
        }
    }
}

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

/// How well a task matches, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskTier {
    /// The query is one of its link keys (`CORE-1042`, `!88`), case ignored.
    Key,
    /// Every term starts a word of the title.
    Title,
    /// The terms are spread over the title, the notes and the link keys.
    Other,
}

impl TaskTier {
    /// The tier of a task, from what matched: a whole link key, every term in its title.
    #[must_use]
    pub const fn of(key: bool, title: bool) -> Self {
        match (key, title) {
            (true, _) => Self::Key,
            (false, true) => Self::Title,
            (false, false) => Self::Other,
        }
    }
}

/// Whether the query made of `terms` (joined with one space) is one of `link_keys`, case
/// ignored.
#[must_use]
pub fn is_key(terms: &[&str], link_keys: &[String]) -> bool {
    if terms.is_empty() {
        return false;
    }
    let query = terms.join(" ").to_lowercase();
    link_keys.iter().any(|key| key.to_lowercase() == query)
}

/// The status named `text` as the index stores it; `None` for a status of a newer loom.
#[must_use]
pub fn status_name(text: &str) -> Option<StatusName> {
    StatusName::ALL
        .into_iter()
        .find(|name| name.to_string() == text)
}

/// A repository match before ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoCandidate {
    /// The result, as returned.
    pub result: RepoMatch,
    /// How well it matches.
    pub tier: Tier,
    /// `last_used` in Unix seconds; `None` when never used.
    pub last_used: Option<i64>,
    /// FTS5 `bm25()`: lower is more relevant.
    pub bm25: f64,
}

/// A task match before ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskCandidate {
    /// The result, as returned.
    pub result: TaskMatch,
    /// Its group, from its status.
    pub group: Group,
    /// How well it matches.
    pub tier: TaskTier,
    /// Last change in Unix seconds; `None` for an invalid `task.md`.
    pub updated: Option<i64>,
    /// FTS5 `bm25()`: lower is more relevant.
    pub bm25: f64,
}

/// Sorts `candidates` best first: by [`Tier`], then most recently used (never used last),
/// then by `bm25`, then by slug, so that the order is total and stable.
pub fn rank_repos(candidates: &mut [RepoCandidate]) {
    candidates.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| recency(a.last_used, b.last_used))
            .then_with(|| a.bm25.total_cmp(&b.bm25))
            .then_with(|| a.result.slug.cmp(&b.result.slug))
    });
}

/// Sorts `candidates` best first: by [`Group`], then [`TaskTier`], then most recently changed
/// (invalid files last), then by `bm25`, then by ID, so that the order is total and stable.
pub fn rank_tasks(candidates: &mut [TaskCandidate]) {
    candidates.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then_with(|| a.tier.cmp(&b.tier))
            .then_with(|| recency(a.updated, b.updated))
            .then_with(|| a.bm25.total_cmp(&b.bm25))
            .then_with(|| a.result.id.cmp(&b.result.id))
    });
}

/// The results of `repos` and `tasks`, best first, at most `limit`: by [`Group`], each kind
/// ranked by [`rank_repos`] or [`rank_tasks`] within it.
#[must_use]
pub fn merge(
    mut repos: Vec<RepoCandidate>,
    mut tasks: Vec<TaskCandidate>,
    limit: usize,
) -> Vec<SearchResult> {
    rank_repos(&mut repos);
    rank_tasks(&mut tasks);
    // No group holds both kinds: a stable sort by group keeps each kind's own ranking.
    let mut all: Vec<(Group, SearchResult)> = tasks
        .into_iter()
        .map(|task| (task.group, SearchResult::Task(task.result)))
        .chain(
            repos
                .into_iter()
                .map(|repo| (Group::Repo, SearchResult::Repo(repo.result))),
        )
        .collect();
    all.sort_by_key(|(group, _)| *group);
    all.into_iter()
        .take(limit)
        .map(|(_, result)| result)
        .collect()
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
    /// At least one term, as a prefix: put in a column filter, whether the column holds one.
    pub any: String,
    /// Every term, as a prefix, in the name.
    pub name: String,
    /// Every term, as a prefix, in the slug.
    pub slug: String,
    /// The terms as one phrase starting the name; whole words.
    pub exact_name: String,
}

impl Expressions {
    /// `expression` restricted to the column `column`.
    #[must_use]
    pub fn in_column(column: &str, expression: &str) -> String {
        format!("{{{column}}} : ({expression})")
    }
}

/// The expressions for `terms`; `None` without terms.
#[must_use]
pub fn expressions(terms: &[&str]) -> Option<Expressions> {
    if terms.is_empty() {
        return None;
    }
    let prefixes: Vec<String> = terms
        .iter()
        .map(|term| format!("{}*", quoted(term)))
        .collect();
    let all = prefixes.join(" AND ");
    Some(Expressions {
        any: prefixes.join(" OR "),
        name: Expressions::in_column("name", &all),
        slug: Expressions::in_column("slug", &all),
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

/// The repositories and tasks matching `query` (in `context` when given, of `kind` when
/// given), ranked by [`merge`], at most `limit`, read in one snapshot.
///
/// # Errors
///
/// [`StoreError`] when a query fails or a task row is invalid.
pub fn search(
    conn: &Connection,
    query: &str,
    context: Option<&str>,
    kind: Option<SearchKind>,
    limit: usize,
) -> Result<Vec<SearchResult>, StoreError> {
    let terms = terms(query);
    let Some(expressions) = expressions(&terms) else {
        return Ok(Vec::new());
    };
    let (repos, tasks) = match kind {
        None => (true, true),
        Some(SearchKind::Repo) => (true, false),
        Some(SearchKind::Task) => (false, true),
    };
    // One read transaction: both kinds come from the same state of the index.
    let tx = conn.unchecked_transaction()?;
    let repos = if repos {
        repo_candidates(&tx, &expressions, context)?
    } else {
        Vec::new()
    };
    let tasks = if tasks {
        task_candidates(&tx, &terms, &expressions, context)?
    } else {
        Vec::new()
    };
    tx.finish()?;
    Ok(merge(repos, tasks, limit))
}

/// The repositories matching `expressions`, unranked.
fn repo_candidates(
    conn: &Connection,
    expressions: &Expressions,
    context: Option<&str>,
) -> Result<Vec<RepoCandidate>, StoreError> {
    let exact = exact_names(conn, &expressions.exact_name)?;
    let mut statement = conn.prepare(
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
            Ok(RepoCandidate {
                result: RepoMatch {
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
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
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

/// A task row as the search reads it, before its values are checked.
struct TaskRow {
    id: String,
    context: String,
    title: String,
    status: Option<String>,
    updated: Option<i64>,
    link_keys: String,
    archived: bool,
    bm25: f64,
    title_all: bool,
    matched: [bool; 3],
}

/// The tasks matching `expressions`, unranked. Which column holds a term is asked of the
/// search table (one subquery per column), so that the notes are never read back.
fn task_candidates(
    conn: &Connection,
    terms: &[&str],
    expressions: &Expressions,
    context: Option<&str>,
) -> Result<Vec<TaskCandidate>, StoreError> {
    let column = |name: &str| Expressions::in_column(name, &expressions.any);
    let mut statement = conn.prepare(
        "SELECT t.id, t.context, t.title, t.status, t.updated, t.link_keys, bm25(tasks_fts), \
                t.archived IS NOT NULL, \
                tasks_fts.rowid IN (SELECT rowid FROM tasks_fts WHERE tasks_fts MATCH ?2), \
                tasks_fts.rowid IN (SELECT rowid FROM tasks_fts WHERE tasks_fts MATCH ?3), \
                tasks_fts.rowid IN (SELECT rowid FROM tasks_fts WHERE tasks_fts MATCH ?4), \
                tasks_fts.rowid IN (SELECT rowid FROM tasks_fts WHERE tasks_fts MATCH ?5) \
         FROM tasks_fts JOIN tasks t ON t.key = tasks_fts.rowid \
         WHERE tasks_fts MATCH ?1 AND (?6 IS NULL OR t.context = ?6)",
    )?;
    let rows = statement.query_map(
        params![
            expressions.all,
            Expressions::in_column("title", &expressions.all),
            column("title"),
            column("body"),
            column("link_keys"),
            context,
        ],
        |row| {
            Ok(TaskRow {
                id: row.get(0)?,
                context: row.get(1)?,
                title: row.get(2)?,
                status: row.get(3)?,
                updated: row.get(4)?,
                link_keys: row.get(5)?,
                bm25: row.get(6)?,
                archived: row.get(7)?,
                title_all: row.get(8)?,
                matched: [row.get(9)?, row.get(10)?, row.get(11)?],
            })
        },
    )?;
    let mut candidates = Vec::new();
    for row in rows {
        candidates.push(task_candidate(terms, row?)?);
    }
    Ok(candidates)
}

/// The candidate of `row`, its values checked.
fn task_candidate(terms: &[&str], row: TaskRow) -> Result<TaskCandidate, StoreError> {
    let corrupt = |message: String| StoreError::Corrupt(message);
    let id: TaskId = row
        .id
        .parse()
        .map_err(|error: loom_core::IdError| corrupt(error.to_string()))?;
    let context: ContextName = row
        .context
        .parse()
        .map_err(|error: loom_core::ContextNameError| corrupt(error.to_string()))?;
    let link_keys: Vec<String> = row
        .link_keys
        .split(' ')
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .collect();
    let group = Group::of_task(row.status.as_deref().and_then(status_name), row.archived);
    let tier = TaskTier::of(is_key(terms, &link_keys), row.title_all);
    let matched_fields = TaskSearchField::ALL
        .into_iter()
        .zip(row.matched)
        .filter(|(_, matched)| *matched)
        .map(|(field, _)| field)
        .collect();
    Ok(TaskCandidate {
        result: TaskMatch {
            id,
            context,
            title: row.title,
            status: row.status,
            link_keys,
            score: -row.bm25,
            matched_fields,
        },
        group,
        tier,
        updated: row.updated,
        bm25: row.bm25,
    })
}

#[cfg(test)]
mod tests;
