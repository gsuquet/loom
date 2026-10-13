//! The SQLite index `index.db`: repositories, their state and their search table (ADR-0012).
//!
//! One [`Store`] writes (loomd owns it); any number of [`Reader`]s read at the same time. The
//! database runs in WAL mode, so a reader sees the state before or after a write transaction,
//! never a part of it, and never waits for it.
//!
//! The schema is a list of numbered SQL migrations, applied in one transaction when the
//! [`Store`] opens and recorded in `PRAGMA user_version`. A database newer than this code is
//! refused, never downgraded.

mod migrate;
mod repos;

use std::fs::{self, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use loom_api::{LastUpdate, RepoIdentity, RepoState, RepoSummary, Timestamp};
use loom_repo::ScanDiff;
use rusqlite::{Connection, OpenFlags};
use thiserror::Error;
use tracing::debug_span;

pub use migrate::SCHEMA_VERSION;
pub use repos::RepoFilter;

/// Mode of `index.db` and of the WAL files SQLite creates beside it: owner read and write.
pub const INDEX_FILE_MODE: u32 = 0o600;

/// How long a connection waits for a lock held by another one before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// An index operation failed.
#[derive(Debug, Error)]
pub enum StoreError {
    /// The database file cannot be created or its mode set.
    #[error("cannot create the index {path}: {source}")]
    Create {
        /// The database file.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// The database cannot be opened or configured.
    #[error("cannot open the index {path}: {source}")]
    Open {
        /// The database file.
        path: PathBuf,
        /// The cause.
        source: rusqlite::Error,
    },
    /// The database was written by a newer loom.
    #[error(
        "the index {path} has schema version {found}, this loom supports up to {supported}: upgrade loom, or stop loomd and delete the file to rebuild it"
    )]
    TooNew {
        /// The database file.
        path: PathBuf,
        /// Version found in the database.
        found: u32,
        /// Latest version this code knows.
        supported: u32,
    },
    /// A reader opened a database the writer has not migrated yet.
    #[error("the index {path} has schema version {found}, expected {expected}: is loomd running?")]
    NotMigrated {
        /// The database file.
        path: PathBuf,
        /// Version found in the database.
        found: u32,
        /// Version this code expects.
        expected: u32,
    },
    /// A migration failed; the database is left at its previous version.
    #[error("cannot migrate the index to schema version {version}: {source}")]
    Migration {
        /// Version of the failed migration.
        version: u32,
        /// The cause.
        source: rusqlite::Error,
    },
    /// A query failed.
    #[error("index query failed: {0}")]
    Sql(#[from] rusqlite::Error),
    /// A row cannot be read back: invalid JSON, timestamp or value.
    #[error("invalid row in the index: {0}")]
    Corrupt(String),
    /// A path that is not valid UTF-8 cannot be stored.
    #[error("path {} is not valid UTF-8", .0.display())]
    NonUtf8Path(PathBuf),
}

/// The writer of the index: the only connection that changes it.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens the index at `path`, creating it (mode `0600`) when missing, and migrates it to
    /// [`SCHEMA_VERSION`] in one transaction.
    ///
    /// # Errors
    ///
    /// [`StoreError::Create`] or [`StoreError::Open`] when the file cannot be created or
    /// opened, [`StoreError::TooNew`] for a database written by a newer loom,
    /// [`StoreError::Migration`] when a migration fails.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let _span = debug_span!("store.open", path = %path.display()).entered();
        create_private(path)?;
        let mut conn = Connection::open(path).map_err(|source| open_error(path, source))?;
        configure(&conn).map_err(|source| open_error(path, source))?;
        // WAL is persistent in the file, but only a write connection can switch to it.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|source| open_error(path, source))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|source| open_error(path, source))?;
        migrate::migrate(&mut conn, path)?;
        Ok(Self { conn })
    }

    /// Every repository, sorted by slug.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the query fails or a row is invalid.
    pub fn repos(&self) -> Result<Vec<RepoSummary>, StoreError> {
        repos::list(&self.conn, &RepoFilter::default())
    }

    /// Paths of the registered repositories, sorted.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the query fails or a path is invalid.
    pub fn registered_paths(&self) -> Result<Vec<PathBuf>, StoreError> {
        repos::registered_paths(&self.conn)
    }

    /// Applies the changes of a scan in one transaction, at `now`: removes `diff.removed`,
    /// updates `diff.updated` (keeping `registered`, `first_seen` and `last_used`), inserts
    /// `diff.added`.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when a write fails (nothing is changed then), such as a slug taken twice
    /// or a path that is not valid UTF-8.
    pub fn apply_scan(&mut self, diff: &ScanDiff, now: Timestamp) -> Result<(), StoreError> {
        let _span = debug_span!(
            "store.apply_scan",
            added = diff.added.len(),
            updated = diff.updated.len(),
            removed = diff.removed.len(),
        )
        .entered();
        repos::apply_scan(&mut self.conn, diff, now)
    }

    /// Adds or updates `identity` as a registered repository, at `now`, in one transaction.
    /// Its slug is made unique against the index, keeping its current one while its context
    /// and name do not change.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the write fails; nothing is changed then.
    pub fn register(
        &mut self,
        identity: RepoIdentity,
        now: Timestamp,
    ) -> Result<RepoSummary, StoreError> {
        let _span = debug_span!("store.register", path = %identity.path.display()).entered();
        repos::register(&mut self.conn, identity, now)
    }

    /// Removes the repository with `slug`; `None` when there is none.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the query fails.
    pub fn forget(&mut self, slug: &str) -> Result<Option<RepoSummary>, StoreError> {
        let _span = debug_span!("store.forget", slug).entered();
        repos::forget(&mut self.conn, slug)
    }

    /// The repository with `slug`.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the query fails or the row is invalid.
    pub fn get(&self, slug: &str) -> Result<Option<RepoSummary>, StoreError> {
        repos::get(&self.conn, slug)
    }

    /// The repository at `path`.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the query fails or the row is invalid.
    pub fn get_by_path(&self, path: &Path) -> Result<Option<RepoSummary>, StoreError> {
        repos::get_by_path(&self.conn, path)
    }

    /// Replaces the state of the repository at `path` with `state`; the search table follows
    /// its branch. Returns `false`, changing nothing, when no repository has this path (removed
    /// since the state was read).
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the write fails, or the path is not valid UTF-8.
    pub fn save_state(&mut self, path: &Path, state: &RepoState) -> Result<bool, StoreError> {
        let _span = debug_span!("store.save_state", path = %path.display()).entered();
        repos::save_state(&self.conn, path, state)
    }
}

impl Store {
    /// Replaces the last update of the repository at `path` with `update` and, when given, its
    /// state with `state`, in one transaction. Returns `false`, changing nothing, when no
    /// repository has this path (removed since the update started).
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the write fails (nothing is changed then), or the path is not valid
    /// UTF-8.
    pub fn save_update(
        &mut self,
        path: &Path,
        update: &LastUpdate,
        state: Option<&RepoState>,
    ) -> Result<bool, StoreError> {
        let _span = debug_span!("store.save_update", path = %path.display()).entered();
        let tx = self.conn.transaction()?;
        if !repos::save_update(&tx, path, update)? {
            return Ok(false);
        }
        if let Some(state) = state {
            repos::save_state(&tx, path, state)?;
        }
        tx.commit()?;
        Ok(true)
    }
}

/// A read-only connection to the index, for the API handlers. It never blocks on the writer.
#[derive(Debug)]
pub struct Reader {
    conn: Connection,
}

impl Reader {
    /// Opens the index at `path`, which a [`Store`] has already created and migrated.
    ///
    /// # Errors
    ///
    /// [`StoreError::Open`] when it cannot be opened (missing file included),
    /// [`StoreError::TooNew`] or [`StoreError::NotMigrated`] when its schema version is not
    /// [`SCHEMA_VERSION`].
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        // Read-write without create: a WAL reader may need to write the shared-memory index.
        // `query_only` forbids every change through this connection.
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn =
            Connection::open_with_flags(path, flags).map_err(|source| open_error(path, source))?;
        configure(&conn).map_err(|source| open_error(path, source))?;
        conn.pragma_update(None, "query_only", true)
            .map_err(|source| open_error(path, source))?;
        let found = migrate::user_version(&conn).map_err(|source| open_error(path, source))?;
        migrate::check_reader_version(found, path)?;
        Ok(Self { conn })
    }

    /// Repositories matching `filter`, sorted by slug, read in one snapshot.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the query fails or a row is invalid.
    pub fn list(&self, filter: &RepoFilter) -> Result<Vec<RepoSummary>, StoreError> {
        let _span = debug_span!("store.list").entered();
        repos::list(&self.conn, filter)
    }

    /// The repository with `slug`.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the query fails or the row is invalid.
    pub fn get(&self, slug: &str) -> Result<Option<RepoSummary>, StoreError> {
        let _span = debug_span!("store.get", slug).entered();
        repos::get(&self.conn, slug)
    }
}

/// Settings of every connection: wait for locks, enforce foreign keys.
fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", true)
}

fn open_error(path: &Path, source: rusqlite::Error) -> StoreError {
    StoreError::Open {
        path: path.to_path_buf(),
        source,
    }
}

/// Creates `path` with mode `0600` when missing, and restricts an existing one to `0600`.
/// SQLite gives its WAL and shared-memory files the mode of the database.
fn create_private(path: &Path) -> Result<(), StoreError> {
    let error = |source| StoreError::Create {
        path: path.to_path_buf(),
        source,
    };
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(INDEX_FILE_MODE)
        .open(path)
        .map_err(error)?;
    fs::set_permissions(path, Permissions::from_mode(INDEX_FILE_MODE)).map_err(error)
}

#[cfg(test)]
mod tests;
