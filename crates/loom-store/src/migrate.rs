//! Schema migrations: numbered SQL files, applied in order, recorded in `PRAGMA user_version`.

use std::path::Path;

use rusqlite::{Connection, TransactionBehavior};

use crate::StoreError;

/// Migration `n` (1-based) brings the schema from version `n - 1` to `n`. Append only: a
/// released migration never changes.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_repos.sql"),
    include_str!("migrations/0002_repo_state.sql"),
    include_str!("migrations/0003_repo_update.sql"),
    include_str!("migrations/0004_search_tokenizer.sql"),
    include_str!("migrations/0005_tasks.sql"),
    include_str!("migrations/0006_task_archive.sql"),
];

/// Schema version this code reads and writes: the number of migrations.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a handful of migrations, far below u32::MAX"
)]
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Brings the database to [`SCHEMA_VERSION`].
pub fn migrate(conn: &mut Connection, path: &Path) -> Result<(), StoreError> {
    migrate_with(conn, path, MIGRATIONS)
}

/// The first `version` migrations, to build an older database in tests.
#[cfg(test)]
pub fn migrations_up_to(version: usize) -> &'static [&'static str] {
    &MIGRATIONS[..version]
}

/// Applies the `migrations` the database has not seen, all in one immediate transaction: a
/// failure leaves the database at its previous version.
pub fn migrate_with(
    conn: &mut Connection,
    path: &Path,
    migrations: &[&str],
) -> Result<(), StoreError> {
    let supported = u32::try_from(migrations.len()).unwrap_or(u32::MAX);
    // Immediate: the version is read under the write lock, so two writers cannot both apply.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let found = user_version(&tx)?;
    if found > supported {
        return Err(StoreError::TooNew {
            path: path.to_path_buf(),
            found,
            supported,
        });
    }
    for (version, sql) in (1..=supported).zip(migrations).skip(found as usize) {
        tracing::debug!(version, "applying index migration");
        tx.execute_batch(sql)
            .and_then(|()| tx.pragma_update(None, "user_version", version))
            .map_err(|source| StoreError::Migration { version, source })?;
    }
    tx.commit()?;
    Ok(())
}

/// `PRAGMA user_version` of the database.
pub fn user_version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
}

/// A reader accepts only a database at [`SCHEMA_VERSION`].
pub fn check_reader_version(found: u32, path: &Path) -> Result<(), StoreError> {
    match found.cmp(&SCHEMA_VERSION) {
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Greater => Err(StoreError::TooNew {
            path: path.to_path_buf(),
            found,
            supported: SCHEMA_VERSION,
        }),
        std::cmp::Ordering::Less => Err(StoreError::NotMigrated {
            path: path.to_path_buf(),
            found,
            expected: SCHEMA_VERSION,
        }),
    }
}
