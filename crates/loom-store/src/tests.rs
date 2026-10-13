use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use loom_api::{ContextAssignment, Remote, RepoIdentity, Timestamp};
use loom_repo::ScanDiff;
use rusqlite::Connection;
use tempfile::TempDir;

use crate::migrate::{migrate_with, user_version};
use crate::{Reader, RepoFilter, SCHEMA_VERSION, Store, StoreError};

fn at(secs: u64) -> Timestamp {
    Timestamp::from_unix_secs(secs).unwrap()
}

fn db(dir: &TempDir) -> PathBuf {
    dir.path().join("index.db")
}

/// A repository at `path` in context `context`, with the slug `slug` and remote URLs `urls`.
fn repo(path: &str, context: &str, slug: &str, urls: &[&str]) -> RepoIdentity {
    RepoIdentity {
        path: PathBuf::from(path),
        name: path.rsplit('/').next().unwrap().to_owned(),
        slug: slug.to_owned(),
        context: ContextAssignment::Default {
            context: context.parse().unwrap(),
        },
        remotes: urls
            .iter()
            .enumerate()
            .map(|(index, url)| Remote {
                name: format!("r{index}"),
                url: (*url).to_owned(),
                location: None,
            })
            .collect(),
        primary_remote: (!urls.is_empty()).then(|| "r0".to_owned()),
        forge: None,
    }
}

fn added(repos: Vec<RepoIdentity>) -> ScanDiff {
    ScanDiff {
        added: repos,
        ..ScanDiff::default()
    }
}

fn slugs(store: &Store) -> Vec<String> {
    store
        .repos()
        .unwrap()
        .into_iter()
        .map(|repo| repo.identity.slug)
        .collect()
}

/// Rows of the search table, by rowid, as `name|slug|path|remotes|context|branch`.
fn fts_rows(path: &Path) -> Vec<String> {
    let conn = Connection::open(path).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT name || '|' || slug || '|' || path || '|' || remotes || '|' || context \
             || '|' || branch FROM repos_fts ORDER BY slug",
        )
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Rows of `repos` with no search row, and search rows with no repository.
fn fts_orphans(path: &Path) -> (i64, i64) {
    let conn = Connection::open(path).unwrap();
    let missing = conn
        .query_row(
            "SELECT count(*) FROM repos WHERE id NOT IN (SELECT rowid FROM repos_fts)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let extra = conn
        .query_row(
            "SELECT count(*) FROM repos_fts WHERE rowid NOT IN (SELECT id FROM repos)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    (missing, extra)
}

#[test]
fn bundled_sqlite_has_fts5_and_json() {
    let conn = Connection::open_in_memory().unwrap();
    let enabled: bool = conn
        .query_row(
            "SELECT sqlite_compileoption_used('ENABLE_FTS5')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(enabled);
    conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(x); SELECT json('[]');")
        .unwrap();
}

#[test]
fn open_creates_a_private_wal_database_at_the_current_version() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mode: String = store
        .conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let foreign_keys: bool = store
        .conn
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap();
    assert!(foreign_keys);
    assert_eq!(user_version(&store.conn).unwrap(), SCHEMA_VERSION);
    assert_eq!(store.repos().unwrap(), []);
    drop(store);

    // An existing file with a wider mode is restricted; reopening changes nothing else.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let store = Store::open(&path).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(user_version(&store.conn).unwrap(), SCHEMA_VERSION);
}

#[test]
fn migrations_apply_from_empty_and_from_the_previous_version() {
    let migrations = [
        "CREATE TABLE a (x INTEGER);",
        "ALTER TABLE a ADD COLUMN y TEXT; INSERT INTO a VALUES (1, 'two');",
    ];
    let dir = TempDir::new().unwrap();
    let path = db(&dir);

    let mut conn = Connection::open(&path).unwrap();
    migrate_with(&mut conn, &path, &migrations).unwrap();
    assert_eq!(user_version(&conn).unwrap(), 2);
    drop(conn);

    let other = dir.path().join("other.db");
    let mut conn = Connection::open(&other).unwrap();
    migrate_with(&mut conn, &other, &migrations[..1]).unwrap();
    conn.execute("INSERT INTO a VALUES (0)", []).unwrap();
    assert_eq!(user_version(&conn).unwrap(), 1);
    migrate_with(&mut conn, &other, &migrations).unwrap();
    assert_eq!(user_version(&conn).unwrap(), 2);
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM a", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 2);
    // Up to date: nothing to apply.
    migrate_with(&mut conn, &other, &migrations).unwrap();
    assert_eq!(user_version(&conn).unwrap(), 2);
}

#[test]
fn failed_migration_leaves_the_previous_version() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    let mut conn = Connection::open(&path).unwrap();
    let error = migrate_with(
        &mut conn,
        &path,
        &["CREATE TABLE a (x INTEGER);", "NOT SQL"],
    )
    .unwrap_err();
    assert!(
        matches!(error, StoreError::Migration { version: 2, .. }),
        "{error}"
    );
    assert_eq!(user_version(&conn).unwrap(), 0);
    let tables: i64 = conn
        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tables, 0);
}

#[test]
fn newer_database_is_refused_and_left_untouched() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    drop(Store::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    drop(conn);

    let error = Store::open(&path).unwrap_err();
    assert!(
        matches!(error, StoreError::TooNew { found, supported, .. }
            if found == SCHEMA_VERSION + 1 && supported == SCHEMA_VERSION),
        "{error}"
    );
    assert!(error.to_string().contains("delete the file"), "{error}");
    let error = Reader::open(&path).unwrap_err();
    assert!(matches!(error, StoreError::TooNew { .. }), "{error}");
    let conn = Connection::open(&path).unwrap();
    assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION + 1);
}

#[test]
fn reader_needs_a_migrated_database() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    assert!(matches!(
        Reader::open(&path).unwrap_err(),
        StoreError::Open { .. }
    ));
    assert!(!path.exists());
    drop(Connection::open(&path).unwrap());
    assert!(matches!(
        Reader::open(&path).unwrap_err(),
        StoreError::NotMigrated { found: 0, .. }
    ));
}

#[test]
fn scan_changes_insert_update_and_remove() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    let mut store = Store::open(&path).unwrap();
    store
        .apply_scan(
            &added(vec![
                repo("/w/a", "p", "p/a", &["git@github.com:me/a.git"]),
                repo("/w/b", "p", "p/b", &[]),
            ]),
            at(100),
        )
        .unwrap();
    assert_eq!(slugs(&store), ["p/a", "p/b"]);

    let updated = repo("/w/a", "w", "w/a", &["https://gitlab.com/x/a.git"]);
    store
        .apply_scan(
            &ScanDiff {
                added: vec![repo("/w/c", "p", "p/c", &[])],
                updated: vec![updated.clone()],
                removed: vec![repo("/w/b", "p", "p/b", &[])],
            },
            at(200),
        )
        .unwrap();
    assert_eq!(slugs(&store), ["p/c", "w/a"]);
    let a = store.get("w/a").unwrap().unwrap();
    assert_eq!(a.identity, updated);
    assert!(!a.registered);
    assert_eq!(a.last_used, None);
    assert_eq!(store.get("p/a").unwrap(), None);

    let times: (i64, i64) = store
        .conn
        .query_row(
            "SELECT first_seen, updated_at FROM repos WHERE path = '/w/a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(times, (100, 200));
}

#[test]
fn a_failed_scan_changes_nothing() {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(&db(&dir)).unwrap();
    store
        .apply_scan(&added(vec![repo("/w/a", "p", "p/x", &[])]), at(1))
        .unwrap();
    // Two added repositories with the same slug: the whole scan, removal included, is refused.
    let error = store
        .apply_scan(
            &ScanDiff {
                added: vec![repo("/w/b", "p", "p/z", &[]), repo("/w/c", "p", "p/z", &[])],
                updated: vec![],
                removed: vec![repo("/w/a", "p", "p/x", &[])],
            },
            at(2),
        )
        .unwrap_err();
    assert!(matches!(error, StoreError::Sql(_)), "{error}");
    assert_eq!(slugs(&store), ["p/x"]);
    assert_eq!(
        store.get("p/x").unwrap().unwrap().identity.path,
        Path::new("/w/a")
    );
}

#[test]
fn slugs_are_unique_but_can_be_swapped_in_one_scan() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    let mut store = Store::open(&path).unwrap();
    store
        .apply_scan(
            &added(vec![
                repo("/a/x", "p", "p/x", &[]),
                repo("/b/x", "w", "w/x", &[]),
            ]),
            at(1),
        )
        .unwrap();
    // Constraint: two repositories never share a slug.
    let error = store
        .conn
        .execute("UPDATE repos SET slug = 'p/x' WHERE path = '/b/x'", [])
        .unwrap_err();
    assert!(error.to_string().contains("UNIQUE"), "{error}");

    // Contexts swapped by a rule change: each takes the other's slug.
    store
        .apply_scan(
            &ScanDiff {
                updated: vec![repo("/a/x", "w", "w/x", &[]), repo("/b/x", "p", "p/x", &[])],
                ..ScanDiff::default()
            },
            at(2),
        )
        .unwrap();
    let paths: Vec<_> = store
        .repos()
        .unwrap()
        .into_iter()
        .map(|repo| (repo.identity.slug, repo.identity.path))
        .collect();
    assert_eq!(
        paths,
        [
            ("p/x".to_owned(), PathBuf::from("/b/x")),
            ("w/x".to_owned(), PathBuf::from("/a/x"))
        ]
    );
    assert_eq!(fts_orphans(&path), (0, 0));
}

#[test]
fn search_table_follows_inserts_updates_and_deletes() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    let mut store = Store::open(&path).unwrap();
    store
        .apply_scan(
            &added(vec![
                repo(
                    "/w/a",
                    "p",
                    "p/a",
                    &["git@github.com:me/a.git", "https://gitlab.com/up/a"],
                ),
                repo("/w/b", "p", "p/b", &[]),
            ]),
            at(1),
        )
        .unwrap();
    assert_eq!(
        fts_rows(&path),
        [
            "a|p/a|/w/a|git@github.com:me/a.git https://gitlab.com/up/a|p|",
            "b|p/b|/w/b||p|"
        ]
    );
    store
        .apply_scan(
            &ScanDiff {
                updated: vec![repo("/w/a", "w", "w/a", &["https://example.com/a"])],
                removed: vec![repo("/w/b", "p", "p/b", &[])],
                ..ScanDiff::default()
            },
            at(2),
        )
        .unwrap();
    assert_eq!(fts_rows(&path), ["a|w/a|/w/a|https://example.com/a|w|"]);
    store.forget("w/a").unwrap();
    assert_eq!(fts_rows(&path), Vec::<String>::new());
    assert_eq!(fts_orphans(&path), (0, 0));

    // FTS5 matching works on the indexed columns.
    store
        .apply_scan(
            &added(vec![repo(
                "/w/loom",
                "p",
                "p/loom",
                &["git@github.com:gsuquet/loom.git"],
            )]),
            at(3),
        )
        .unwrap();
    let conn = Connection::open(&path).unwrap();
    let hits: i64 = conn
        .query_row(
            "SELECT count(*) FROM repos_fts WHERE repos_fts MATCH 'remotes:gsuquet'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hits, 1);
}

#[test]
fn register_keeps_the_flag_across_scans_until_forget() {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(&db(&dir)).unwrap();
    store
        .apply_scan(&added(vec![repo("/w/x", "p", "p/x", &[])]), at(1))
        .unwrap();

    // A new registered repository with the same base slug gets a suffix.
    let summary = store
        .register(repo("/elsewhere/x", "p", "p/x", &[]), at(2))
        .unwrap();
    assert_eq!(summary.identity.slug, "p/x-2");
    assert!(summary.registered);
    assert_eq!(
        store.registered_paths().unwrap(),
        [PathBuf::from("/elsewhere/x")]
    );

    // Registering a scanned repository flags it and keeps its slug.
    let summary = store
        .register(repo("/w/x", "p", "p/x", &[]), at(3))
        .unwrap();
    assert_eq!(summary.identity.slug, "p/x");
    assert!(summary.registered);

    // A scan updating a registered repository keeps the flag.
    store
        .apply_scan(
            &ScanDiff {
                updated: vec![repo(
                    "/elsewhere/x",
                    "p",
                    "p/x-2",
                    &["https://example.com/x"],
                )],
                ..ScanDiff::default()
            },
            at(4),
        )
        .unwrap();
    assert!(store.get("p/x-2").unwrap().unwrap().registered);

    let forgotten = store.forget("p/x-2").unwrap().unwrap();
    assert_eq!(forgotten.identity.path, Path::new("/elsewhere/x"));
    assert_eq!(store.forget("p/x-2").unwrap(), None);
    assert_eq!(store.registered_paths().unwrap(), [PathBuf::from("/w/x")]);
}

#[test]
fn list_filters_by_context_and_substring() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    let mut store = Store::open(&path).unwrap();
    store
        .apply_scan(
            &added(vec![
                repo(
                    "/w/loom",
                    "p",
                    "p/loom",
                    &["git@github.com:gsuquet/loom.git"],
                ),
                repo("/w/app", "w", "w/app", &["https://gitlab.com/Company/app"]),
                repo("/w/100%_done", "w", "w/100%_done", &[]),
            ]),
            at(1),
        )
        .unwrap();
    let reader = Reader::open(&path).unwrap();
    let slugs = |filter: RepoFilter| -> Vec<String> {
        reader
            .list(&filter)
            .unwrap()
            .into_iter()
            .map(|repo| repo.identity.slug)
            .collect()
    };
    let filter = |context: Option<&str>, query: Option<&str>| RepoFilter {
        context: context.map(str::to_owned),
        query: query.map(str::to_owned),
    };
    assert_eq!(
        slugs(filter(None, None)),
        ["p/loom", "w/100%_done", "w/app"]
    );
    assert_eq!(slugs(filter(Some("w"), None)), ["w/100%_done", "w/app"]);
    assert_eq!(slugs(filter(Some("x"), None)), Vec::<String>::new());
    // Remote URLs, ASCII case ignored.
    assert_eq!(slugs(filter(None, Some("company"))), ["w/app"]);
    assert_eq!(slugs(filter(None, Some("GSUQUET"))), ["p/loom"]);
    assert_eq!(slugs(filter(Some("w"), Some("loom"))), Vec::<String>::new());
    // `%` and `_` are literal.
    assert_eq!(slugs(filter(None, Some("%_"))), ["w/100%_done"]);
    assert_eq!(slugs(filter(None, Some("_"))), ["w/100%_done"]);
    assert_eq!(reader.get("w/app").unwrap().unwrap().identity.name, "app");
    assert_eq!(reader.get("nope").unwrap(), None);
}

#[test]
fn reader_sees_a_snapshot_never_a_partial_write() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    let mut store = Store::open(&path).unwrap();
    store
        .apply_scan(&added(vec![repo("/w/a", "p", "p/a", &[])]), at(1))
        .unwrap();
    let reader = Reader::open(&path).unwrap();

    // A write transaction in progress, as during a scan.
    let tx = store.conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO repos (path, name, slug, context, assignment, remotes, first_seen, updated_at) \
         VALUES ('/w/b', 'b', 'p/b', 'p', '{\"rule\":\"unassigned\"}', '[]', 2, 2)",
        [],
    )
    .unwrap();
    assert_eq!(reader.list(&RepoFilter::default()).unwrap().len(), 1);
    tx.commit().unwrap();
    assert_eq!(reader.list(&RepoFilter::default()).unwrap().len(), 2);

    // Readers cannot write.
    let error = reader.conn.execute("DELETE FROM repos", []).unwrap_err();
    assert!(error.to_string().contains("readonly"), "{error}");
}

#[test]
fn deleted_index_is_recreated_empty() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    let mut store = Store::open(&path).unwrap();
    store
        .apply_scan(&added(vec![repo("/w/a", "p", "p/a", &[])]), at(1))
        .unwrap();
    drop(store);
    for suffix in ["", "-wal", "-shm"] {
        let file = PathBuf::from(format!("{}{suffix}", path.display()));
        if file.exists() {
            fs::remove_file(file).unwrap();
        }
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.repos().unwrap(), []);
    assert_eq!(user_version(&store.conn).unwrap(), SCHEMA_VERSION);
}

#[test]
fn non_utf8_paths_are_refused() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let dir = TempDir::new().unwrap();
    let mut store = Store::open(&db(&dir)).unwrap();
    let mut bad = repo("/w/a", "p", "p/a", &[]);
    bad.path = PathBuf::from(OsStr::from_bytes(b"/w/\xff"));
    let error = store.apply_scan(&added(vec![bad]), at(1)).unwrap_err();
    assert!(matches!(error, StoreError::NonUtf8Path(_)), "{error}");
}
