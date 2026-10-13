use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use loom_api::{
    ContextAssignment, MaybeKnown, Remote, RepoHead, RepoIdentity, RepoState, SearchField,
    SearchKind, Timestamp, Untracked, Upstream,
};
use loom_repo::ScanDiff;
use rusqlite::Connection;
use tempfile::TempDir;

use crate::migrate::{migrate_with, migrations_up_to, user_version};
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

/// A state with every field known but `behind`, on `branch`.
fn state_on(branch: &str, refreshed_at: u64) -> RepoState {
    RepoState {
        refreshed_at: at(refreshed_at),
        head: MaybeKnown::Known(RepoHead::Branch {
            name: branch.to_owned(),
        }),
        dirty: MaybeKnown::Known(2),
        untracked: MaybeKnown::Known(Untracked {
            count: 1000,
            capped: true,
        }),
        operation: MaybeKnown::Known(None),
        upstream: MaybeKnown::Known(Some(Upstream {
            name: format!("origin/{branch}"),
            ahead: MaybeKnown::Known(1),
            behind: MaybeKnown::Unknown,
        })),
        default_branch: MaybeKnown::Known(Some("main".to_owned())),
        last_commit: MaybeKnown::Known(Some(at(50))),
        last_fetch: MaybeKnown::Known(None),
        error: Some("ahead/behind: time budget of 2s exceeded".to_owned()),
    }
}

#[test]
fn states_are_saved_read_back_and_searched_by_branch() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    let mut store = Store::open(&path).unwrap();
    store
        .apply_scan(
            &added(vec![
                repo("/w/a", "p", "p/a", &[]),
                repo("/w/b", "p", "p/b", &[]),
            ]),
            at(100),
        )
        .unwrap();
    // Not refreshed yet.
    assert_eq!(store.get("p/a").unwrap().unwrap().state, None);

    let state = state_on("feat/x", 150);
    assert!(store.save_state(Path::new("/w/a"), &state).unwrap());
    let reader = Reader::open(&path).unwrap();
    assert_eq!(
        reader.get("p/a").unwrap().unwrap().state,
        Some(state.clone())
    );
    assert_eq!(
        store
            .get_by_path(Path::new("/w/a"))
            .unwrap()
            .unwrap()
            .identity
            .slug,
        "p/a"
    );
    assert_eq!(store.get_by_path(Path::new("/w/none")).unwrap(), None);
    assert_eq!(fts_rows(&path), ["a|p/a|/w/a||p|feat/x", "b|p/b|/w/b||p|"]);

    // Replaced; an unknown head leaves an empty branch.
    let unknown = RepoState {
        refreshed_at: at(160),
        head: MaybeKnown::Unknown,
        dirty: MaybeKnown::Unknown,
        untracked: MaybeKnown::Unknown,
        operation: MaybeKnown::Unknown,
        upstream: MaybeKnown::Unknown,
        default_branch: MaybeKnown::Unknown,
        last_commit: MaybeKnown::Unknown,
        last_fetch: MaybeKnown::Unknown,
        error: Some("cannot open the repository".to_owned()),
    };
    assert!(store.save_state(Path::new("/w/b"), &unknown).unwrap());
    assert_eq!(reader.get("p/b").unwrap().unwrap().state, Some(unknown));
    let detached = RepoState {
        head: MaybeKnown::Known(RepoHead::Detached {
            commit: "abc1234".to_owned(),
        }),
        ..state_on("x", 170)
    };
    assert!(store.save_state(Path::new("/w/a"), &detached).unwrap());
    assert_eq!(fts_rows(&path), ["a|p/a|/w/a||p|", "b|p/b|/w/b||p|"]);
    assert!(
        store
            .save_state(Path::new("/w/a"), &state_on("main", 180))
            .unwrap()
    );

    // A scan update keeps the state and its branch in the search table.
    store
        .apply_scan(
            &ScanDiff {
                updated: vec![repo("/w/a", "w", "w/a", &["https://x.example/a.git"])],
                ..ScanDiff::default()
            },
            at(200),
        )
        .unwrap();
    assert_eq!(
        store.get("w/a").unwrap().unwrap().state,
        Some(state_on("main", 180))
    );
    assert_eq!(
        fts_rows(&path),
        [
            "b|p/b|/w/b||p|",
            "a|w/a|/w/a|https://x.example/a.git|w|main"
        ]
    );
    assert!(
        reader
            .list(&RepoFilter::default())
            .unwrap()
            .iter()
            .all(|repo| repo.state.is_some())
    );

    // A state read for a repository removed since is dropped.
    assert!(!store.save_state(Path::new("/w/gone"), &state).unwrap());
    // Removing a repository removes its state.
    store.forget("w/a").unwrap();
    let conn = Connection::open(&path).unwrap();
    let states: i64 = conn
        .query_row("SELECT count(*) FROM repo_state", [], |row| row.get(0))
        .unwrap();
    assert_eq!(states, 1);
    assert_eq!(fts_orphans(&path), (0, 0));
}

#[test]
fn version_1_database_migrates_to_the_latest_with_its_repositories() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    let mut conn = Connection::open(&path).unwrap();
    migrate_with(&mut conn, &path, migrations_up_to(1)).unwrap();
    assert_eq!(user_version(&conn).unwrap(), 1);
    // A row as the version-1 store writes it.
    conn.execute(
        "INSERT INTO repos (path, name, slug, context, assignment, remotes, registered, \
                            first_seen, updated_at) \
         VALUES ('/w/a', 'a', 'unassigned/a', 'unassigned', '{\"rule\":\"unassigned\"}', \
                 '[{\"name\":\"origin\",\"url\":\"git@github.com:me/a.git\"}]', 1, 10, 10)",
        [],
    )
    .unwrap();
    drop(conn);

    let mut store = Store::open(&path).unwrap();
    assert_eq!(user_version(&store.conn).unwrap(), SCHEMA_VERSION);
    let repo = store.get("unassigned/a").unwrap().unwrap();
    assert!(repo.registered);
    assert_eq!(repo.state, None);
    assert_eq!(
        fts_rows(&path),
        ["a|unassigned/a|/w/a|git@github.com:me/a.git|unassigned|"]
    );
    assert!(
        store
            .save_state(Path::new("/w/a"), &state_on("dev", 20))
            .unwrap()
    );
    assert_eq!(
        fts_rows(&path),
        ["a|unassigned/a|/w/a|git@github.com:me/a.git|unassigned|dev"]
    );
    let reader = Reader::open(&path).unwrap();
    assert_eq!(
        reader.get("unassigned/a").unwrap().unwrap().state,
        Some(state_on("dev", 20))
    );
}

#[test]
fn last_used_is_set_by_path_and_kept_across_scans() {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(&db(&dir)).unwrap();
    store
        .apply_scan(&added(vec![repo("/w/x", "p", "p/x", &[])]), at(1))
        .unwrap();
    assert!(store.set_last_used(Path::new("/w/x"), at(5)).unwrap());
    assert!(!store.set_last_used(Path::new("/w/nope"), at(5)).unwrap());
    let get = |store: &Store| store.get("p/x").unwrap().unwrap();
    assert_eq!(get(&store).last_used, Some(at(5)));
    assert!(!get(&store).registered);

    store
        .apply_scan(
            &ScanDiff {
                updated: vec![repo("/w/x", "p", "p/x", &["ssh://h/a/x"])],
                ..ScanDiff::default()
            },
            at(6),
        )
        .unwrap();
    assert_eq!(get(&store).last_used, Some(at(5)));
}

#[test]
fn replace_all_drops_everything_else() {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(&db(&dir)).unwrap();
    store
        .apply_scan(&added(vec![repo("/w/x", "p", "p/x", &[])]), at(1))
        .unwrap();
    store
        .register(repo("/elsewhere/y", "p", "p/y", &[]), at(2))
        .unwrap();
    store.set_last_used(Path::new("/w/x"), at(3)).unwrap();
    store
        .save_state(Path::new("/w/x"), &state_on("main", 3))
        .unwrap();

    store
        .replace_all(
            &[repo("/w/x", "p", "p/x", &[]), repo("/w/z", "p", "p/z", &[])],
            at(4),
        )
        .unwrap();
    assert_eq!(slugs(&store), ["p/x", "p/z"]);
    let x = store.get("p/x").unwrap().unwrap();
    assert_eq!(x.last_used, None);
    assert_eq!(x.state, None);
    assert!(!x.registered);
    assert_eq!(store.registered_paths().unwrap(), Vec::<PathBuf>::new());
    assert_eq!(fts_orphans(&db(&dir)), (0, 0));
}

/// Slugs found by `query`, in context `context`, best first.
fn found(reader: &Reader, query: &str, context: Option<&str>) -> Vec<String> {
    reader
        .search(query, context, 100)
        .unwrap()
        .into_iter()
        .map(|result| result.slug)
        .collect()
}

/// A store and a reader on it, with:
///
/// - `p/loom` (`/w/loom`): its name is `loom`;
/// - `p/loom-api` (`/w/loom-api`): its name starts with `loom`;
/// - `loom/tool` (`/w/tool`): `loom` is its context, so in its slug;
/// - `p/inner` (`/src/loom/inner`): `loom` is in its path only;
/// - `p/widget` (`/w/widget`), with a GitHub remote;
/// - `p/cafe` (`/w/Café-Crème`).
fn search_index(dir: &TempDir) -> (Store, Reader) {
    let path = db(dir);
    let mut store = Store::open(&path).unwrap();
    store
        .apply_scan(
            &added(vec![
                repo("/w/loom", "p", "p/loom", &[]),
                repo("/w/loom-api", "p", "p/loom-api", &[]),
                repo("/w/tool", "loom", "loom/tool", &[]),
                repo("/src/loom/inner", "p", "p/inner", &[]),
                repo(
                    "/w/widget",
                    "p",
                    "p/widget",
                    &["https://github.com/me/gadget.git"],
                ),
                repo("/w/Café-Crème", "p", "p/cafe", &[]),
            ]),
            at(1),
        )
        .unwrap();
    let reader = Reader::open(&path).unwrap();
    (store, reader)
}

#[test]
fn search_ranks_exact_name_then_name_prefix_then_slug_then_other_fields() {
    let dir = TempDir::new().unwrap();
    let (mut store, reader) = search_index(&dir);
    // Recency never beats a better tier.
    store
        .set_last_used(Path::new("/src/loom/inner"), at(9))
        .unwrap();
    store.set_last_used(Path::new("/w/tool"), at(8)).unwrap();
    assert_eq!(
        found(&reader, "loom", None),
        ["p/loom", "p/loom-api", "loom/tool", "p/inner"]
    );

    let results = reader.search("loom", None, 100).unwrap();
    let fields: Vec<Vec<SearchField>> = results
        .iter()
        .map(|result| result.matched_fields.clone())
        .collect();
    assert_eq!(
        fields,
        [
            vec![SearchField::Name, SearchField::Slug, SearchField::Path],
            vec![SearchField::Name, SearchField::Slug, SearchField::Path],
            vec![SearchField::Slug, SearchField::Context],
            vec![SearchField::Path],
        ]
    );
    assert!(results.iter().all(|result| result.kind == SearchKind::Repo));
    assert!(results.iter().all(|result| result.score.is_finite()));
    assert_eq!(results[3].name, "inner");
    assert_eq!(results[3].context, "p");
    assert_eq!(results[3].path, PathBuf::from("/src/loom/inner"));

    // `loom-api` names `p/loom-api` exactly, whatever its case and separators.
    assert_eq!(found(&reader, "LOOM api", None)[0], "p/loom-api");
    assert_eq!(found(&reader, "loom-api", None)[0], "p/loom-api");
}

#[test]
fn search_orders_a_tier_by_last_used_then_relevance_then_slug() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    let mut store = Store::open(&path).unwrap();
    store
        .apply_scan(
            &added(vec![
                repo("/w/app-a", "p", "p/app-a", &[]),
                repo("/w/app-b", "p", "p/app-b", &[]),
                repo("/w/app-c", "p", "p/app-c", &[]),
            ]),
            at(1),
        )
        .unwrap();
    let reader = Reader::open(&path).unwrap();
    assert_eq!(
        found(&reader, "app", None),
        ["p/app-a", "p/app-b", "p/app-c"]
    );
    store.set_last_used(Path::new("/w/app-c"), at(5)).unwrap();
    store.set_last_used(Path::new("/w/app-b"), at(7)).unwrap();
    assert_eq!(
        found(&reader, "app", None),
        ["p/app-b", "p/app-c", "p/app-a"]
    );
}

#[test]
fn search_terms_are_prefixes_that_must_all_match() {
    let dir = TempDir::new().unwrap();
    let (_store, reader) = search_index(&dir);
    assert_eq!(found(&reader, "lo ap", None), ["p/loom-api"]);
    assert_eq!(found(&reader, "loom zzz", None), Vec::<String>::new());
    assert_eq!(found(&reader, "wid", None), ["p/widget"]);
    // Remote URLs are searched.
    let results = reader.search("gadget", None, 100).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].slug, "p/widget");
    assert_eq!(results[0].matched_fields, [SearchField::Remotes]);
    assert_eq!(found(&reader, "github me", None), ["p/widget"]);
    // Nothing to search.
    assert_eq!(found(&reader, "", None), Vec::<String>::new());
    assert_eq!(found(&reader, " - * ", None), Vec::<String>::new());
}

#[test]
fn search_folds_case_and_diacritics() {
    let dir = TempDir::new().unwrap();
    let (mut store, reader) = search_index(&dir);
    for query in ["cafe creme", "CAFÉ", "Crème", "café-crè", "cafe"] {
        assert_eq!(found(&reader, query, None), ["p/cafe"], "{query}");
    }
    let results = reader.search("cafe creme", None, 1).unwrap();
    assert_eq!(results[0].name, "Café-Crème");
    // Several diacritics on one letter (`remove_diacritics 2`).
    store
        .apply_scan(&added(vec![repo("/w/Việt", "p", "p/viet", &[])]), at(2))
        .unwrap();
    assert_eq!(found(&reader, "viet", None), ["p/viet"]);
}

#[test]
fn search_treats_fts5_syntax_as_text() {
    let dir = TempDir::new().unwrap();
    let (_store, reader) = search_index(&dir);
    for query in [
        "{loom}", "loom\"", "\"loom", "-loom", "+loom", "^loom", "loom*", "(loom)", "loom:",
    ] {
        assert_eq!(found(&reader, query, None)[0], "p/loom", "{query}");
    }
    // Operators are words to find, not operators.
    for query in [
        "loom OR zzz",
        "loom AND",
        "NOT loom",
        "NEAR(loom api)",
        "{name} : x",
    ] {
        assert_eq!(found(&reader, query, None), Vec::<String>::new(), "{query}");
    }
    // `slug:widget` is the words `slug` and `widget`, not a column filter.
    assert_eq!(found(&reader, "slug:widget", None), Vec::<String>::new());
}

#[test]
fn search_keeps_one_context_and_the_limit() {
    let dir = TempDir::new().unwrap();
    let (_store, reader) = search_index(&dir);
    assert_eq!(found(&reader, "loom", Some("loom")), ["loom/tool"]);
    assert_eq!(found(&reader, "loom", Some("nope")), Vec::<String>::new());
    let first: Vec<String> = reader
        .search("loom", None, 2)
        .unwrap()
        .into_iter()
        .map(|result| result.slug)
        .collect();
    assert_eq!(first, ["p/loom", "p/loom-api"]);
    assert_eq!(reader.search("loom", None, 0).unwrap(), []);
}

#[test]
fn search_finds_the_branch() {
    let dir = TempDir::new().unwrap();
    let (mut store, reader) = search_index(&dir);
    assert!(
        store
            .save_state(Path::new("/w/tool"), &state_on("feature-ünïcode", 3))
            .unwrap()
    );
    let results = reader.search("feature unicode", None, 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].slug, "loom/tool");
    assert_eq!(results[0].matched_fields, [SearchField::Branch]);
}

#[test]
fn version_2_database_migrates_its_search_table() {
    let dir = TempDir::new().unwrap();
    let path = db(&dir);
    let mut conn = Connection::open(&path).unwrap();
    migrate_with(&mut conn, &path, migrations_up_to(2)).unwrap();
    conn.execute(
        "INSERT INTO repos (path, name, slug, context, assignment, remotes, registered, \
                            first_seen, updated_at) \
         VALUES ('/w/Việt', 'Việt', 'p/viet', 'p', '{\"rule\":\"unassigned\"}', \
                 '[{\"name\":\"origin\",\"url\":\"git@github.com:me/v.git\"}]', 1, 10, 10)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO repo_state (repo_id, refreshed_at, head) \
         SELECT id, 20, '{\"kind\":\"branch\",\"name\":\"dev\"}' FROM repos",
        [],
    )
    .unwrap();
    drop(conn);

    let store = Store::open(&path).unwrap();
    assert_eq!(user_version(&store.conn).unwrap(), SCHEMA_VERSION);
    assert_eq!(
        fts_rows(&path),
        ["Việt|p/viet|/w/Việt|git@github.com:me/v.git|p|dev"]
    );
    assert_eq!(fts_orphans(&path), (0, 0));
    let reader = Reader::open(&path).unwrap();
    assert_eq!(found(&reader, "viet dev", None), ["p/viet"]);
}
