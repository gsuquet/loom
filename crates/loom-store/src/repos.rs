//! The `repos` table: reads, scan changes, registration and removal.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use loom_api::{ContextAssignment, ForgeKind, Remote, RepoIdentity, RepoSummary, Timestamp};
use loom_repo::{ScanDiff, assign_unique_slugs};
use rusqlite::{Connection, OptionalExtension as _, Row, TransactionBehavior, params};

use crate::StoreError;

/// Which repositories [`crate::Reader::list`] returns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoFilter {
    /// Only the repositories of this context (`unassigned` included).
    pub context: Option<String>,
    /// Only the repositories whose name, slug, path, remote URLs or context contain this text,
    /// ASCII case ignored.
    pub query: Option<String>,
}

const COLUMNS: &str = "r.path, r.name, r.slug, r.assignment, r.remotes, r.primary_remote, \
                       r.forge, r.registered, r.last_used";

/// Repositories matching `filter`, sorted by slug, in one statement (one snapshot).
pub fn list(conn: &Connection, filter: &RepoFilter) -> Result<Vec<RepoSummary>, StoreError> {
    let pattern = filter.query.as_deref().map(like_pattern);
    let sql = format!(
        "SELECT {COLUMNS} FROM repos r JOIN repos_fts f ON f.rowid = r.id \
         WHERE (?1 IS NULL OR r.context = ?1) \
           AND (?2 IS NULL OR f.name LIKE ?2 ESCAPE '\\' OR f.slug LIKE ?2 ESCAPE '\\' \
                OR f.path LIKE ?2 ESCAPE '\\' OR f.remotes LIKE ?2 ESCAPE '\\' \
                OR f.context LIKE ?2 ESCAPE '\\') \
         ORDER BY r.slug"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params![filter.context, pattern], read_row)?;
    rows.map(|row| row?).collect()
}

/// The repository with `slug`.
pub fn get(conn: &Connection, slug: &str) -> Result<Option<RepoSummary>, StoreError> {
    let sql = format!("SELECT {COLUMNS} FROM repos r WHERE r.slug = ?1");
    conn.query_row(&sql, [slug], read_row)
        .optional()?
        .transpose()
}

/// Paths of the registered repositories, sorted.
pub fn registered_paths(conn: &Connection) -> Result<Vec<PathBuf>, StoreError> {
    let mut statement =
        conn.prepare("SELECT path FROM repos WHERE registered = 1 ORDER BY path")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.map(|row| row.map(PathBuf::from).map_err(StoreError::from))
        .collect()
}

pub fn apply_scan(
    conn: &mut Connection,
    diff: &ScanDiff,
    now: Timestamp,
) -> Result<(), StoreError> {
    let now = unix(now)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for repo in &diff.removed {
        tx.execute("DELETE FROM repos WHERE path = ?1", [text(&repo.path)?])?;
    }
    // Slugs are unique at every statement: an updated repository may take the slug another
    // one gives up later in this transaction, so the changing slugs are first parked on a
    // placeholder no slug can equal (`~` never starts a context name).
    for repo in &diff.updated {
        tx.execute(
            "UPDATE repos SET slug = '~' || id WHERE path = ?1 AND slug <> ?2",
            params![text(&repo.path)?, repo.slug],
        )?;
    }
    for repo in diff.updated.iter().chain(&diff.added) {
        upsert(&tx, repo, false, now)?;
    }
    tx.commit()?;
    Ok(())
}

pub fn register(
    conn: &mut Connection,
    mut identity: RepoIdentity,
    now: Timestamp,
) -> Result<RepoSummary, StoreError> {
    let now = unix(now)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let known = list(&tx, &RepoFilter::default())?;
    let existing: BTreeMap<PathBuf, String> = known
        .iter()
        .map(|repo| (repo.identity.path.clone(), repo.identity.slug.clone()))
        .collect();
    let mut all: Vec<RepoIdentity> = known
        .into_iter()
        .map(|repo| repo.identity)
        .filter(|repo| repo.path != identity.path)
        .collect();
    all.push(identity.clone());
    assign_unique_slugs(&existing, &mut all);
    if let Some(assigned) = all.iter().find(|repo| repo.path == identity.path) {
        identity.slug.clone_from(&assigned.slug);
    }
    upsert(&tx, &identity, true, now)?;
    let summary = get(&tx, &identity.slug)?
        .ok_or_else(|| StoreError::Corrupt(format!("{} vanished", identity.slug)))?;
    tx.commit()?;
    Ok(summary)
}

pub fn forget(conn: &mut Connection, slug: &str) -> Result<Option<RepoSummary>, StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let found = get(&tx, slug)?;
    if found.is_some() {
        tx.execute("DELETE FROM repos WHERE slug = ?1", [slug])?;
    }
    tx.commit()?;
    Ok(found)
}

/// Inserts `repo`, or updates its identity; `registered` is set when true, never cleared.
fn upsert(
    conn: &Connection,
    repo: &RepoIdentity,
    registered: bool,
    now: i64,
) -> Result<(), StoreError> {
    let assignment = serde_json::to_string(&repo.context)
        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    let remotes = serde_json::to_string(&repo.remotes)
        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    conn.execute(
        "INSERT INTO repos (path, name, slug, context, assignment, remotes, primary_remote, \
                            forge, registered, first_seen, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10) \
         ON CONFLICT (path) DO UPDATE SET \
             name = excluded.name, slug = excluded.slug, context = excluded.context, \
             assignment = excluded.assignment, remotes = excluded.remotes, \
             primary_remote = excluded.primary_remote, forge = excluded.forge, \
             registered = max(registered, excluded.registered), updated_at = excluded.updated_at",
        params![
            text(&repo.path)?,
            repo.name,
            repo.slug,
            repo.context.context_name(),
            assignment,
            remotes,
            repo.primary_remote,
            repo.forge.map(ForgeKind::as_str),
            registered,
            now,
        ],
    )?;
    Ok(())
}

/// A row of [`COLUMNS`]; the inner error is an invalid value.
fn read_row(row: &Row<'_>) -> rusqlite::Result<Result<RepoSummary, StoreError>> {
    let path: String = row.get(0)?;
    let name: String = row.get(1)?;
    let slug: String = row.get(2)?;
    let assignment: String = row.get(3)?;
    let remotes: String = row.get(4)?;
    let primary_remote: Option<String> = row.get(5)?;
    let forge: Option<String> = row.get(6)?;
    let registered: bool = row.get(7)?;
    let last_used: Option<i64> = row.get(8)?;
    Ok(build(
        path,
        name,
        slug,
        &assignment,
        &remotes,
        primary_remote,
        forge.as_deref(),
        registered,
        last_used,
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "one argument per column, named like them"
)]
fn build(
    path: String,
    name: String,
    slug: String,
    assignment: &str,
    remotes: &str,
    primary_remote: Option<String>,
    forge: Option<&str>,
    registered: bool,
    last_used: Option<i64>,
) -> Result<RepoSummary, StoreError> {
    let corrupt = |what: &str, error: &dyn std::fmt::Display| {
        StoreError::Corrupt(format!("{what} of {slug}: {error}"))
    };
    let context: ContextAssignment =
        serde_json::from_str(assignment).map_err(|error| corrupt("assignment", &error))?;
    let remotes: Vec<Remote> =
        serde_json::from_str(remotes).map_err(|error| corrupt("remotes", &error))?;
    let forge = match forge {
        None => None,
        Some("github") => Some(ForgeKind::Github),
        Some("gitlab") => Some(ForgeKind::Gitlab),
        Some(other) => return Err(corrupt("forge", &other)),
    };
    let last_used = last_used
        .map(|secs| {
            u64::try_from(secs)
                .ok()
                .and_then(Timestamp::from_unix_secs)
                .ok_or_else(|| corrupt("last_used", &secs))
        })
        .transpose()?;
    Ok(RepoSummary {
        identity: RepoIdentity {
            path: PathBuf::from(path),
            name,
            slug,
            context,
            remotes,
            primary_remote,
            forge,
        },
        registered,
        last_used,
    })
}

/// `path` as stored: UTF-8 text.
fn text(path: &Path) -> Result<&str, StoreError> {
    path.to_str()
        .ok_or_else(|| StoreError::NonUtf8Path(path.to_path_buf()))
}

/// Unix seconds as stored.
fn unix(at: Timestamp) -> Result<i64, StoreError> {
    i64::try_from(at.unix_secs()).map_err(|error| StoreError::Corrupt(error.to_string()))
}

/// `%text%` for `LIKE … ESCAPE '\'`, with `%`, `_` and `\` matched literally.
fn like_pattern(text: &str) -> String {
    let mut pattern = String::with_capacity(text.len() + 2);
    pattern.push('%');
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}
