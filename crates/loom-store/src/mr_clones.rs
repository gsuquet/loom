//! The clones of the MRs of Review tasks in the index. They hold paths of this machine, so they
//! are never in a store. A new `index.db` has none: the clone of an MR is chosen again.

use std::path::{Path, PathBuf};

use loom_core::TaskId;
use rusqlite::{Connection, params};

use crate::StoreError;

/// A clone that holds the refs of an MR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MrClone {
    /// The working tree where git runs.
    pub path: PathBuf,
    /// The remote that names the target project of the MR.
    pub remote: String,
}

/// The clones of one MR.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MrClones {
    /// The clone chosen.
    pub clone: Option<MrClone>,
    /// The clones that held its refs before. Each stays listed until its refs are deleted, so
    /// that the cleanup still knows the path when no clone replaces it. Sorted when read.
    pub retired: Vec<MrClone>,
}

fn text(path: &Path) -> Result<&str, StoreError> {
    path.to_str()
        .ok_or_else(|| StoreError::NonUtf8Path(path.to_path_buf()))
}

/// The clones of the MR `key` of task `id`.
pub(crate) fn get(conn: &Connection, id: &TaskId, key: &str) -> Result<MrClones, StoreError> {
    let mut statement = conn.prepare(
        "SELECT path, remote, retired FROM mr_clones \
         WHERE task_id = ?1 AND mr_key = ?2 ORDER BY path, remote",
    )?;
    let rows = statement.query_map(params![id.as_str(), key], |row| {
        Ok((
            MrClone {
                path: PathBuf::from(row.get::<_, String>(0)?),
                remote: row.get(1)?,
            },
            row.get::<_, bool>(2)?,
        ))
    })?;
    let mut clones = MrClones::default();
    for row in rows {
        let (clone, retired) = row?;
        if retired {
            clones.retired.push(clone);
        } else {
            clones.clone = Some(clone);
        }
    }
    Ok(clones)
}

/// Replaces the clones of the MR `key` of task `id`, in one transaction.
pub(crate) fn set(
    conn: &Connection,
    id: &TaskId,
    key: &str,
    clones: &MrClones,
) -> Result<(), StoreError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM mr_clones WHERE task_id = ?1 AND mr_key = ?2",
        params![id.as_str(), key],
    )?;
    let chosen = clones.clone.iter().map(|clone| (clone, false));
    let retired = clones.retired.iter().map(|clone| (clone, true));
    for (clone, is_retired) in chosen.chain(retired) {
        tx.execute(
            "INSERT OR REPLACE INTO mr_clones (task_id, mr_key, path, remote, retired) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.as_str(),
                key,
                text(&clone.path)?,
                clone.remote,
                is_retired
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}
