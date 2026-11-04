//! MR refs and diffs (ADR-0016, decisions 3 and 4).
//!
//! Which clone and remote hold the refs of an MR, which refspecs fetch them, when a version needs
//! a diff, and how a diff is recorded in the mirror. Pure, except [`git`], which runs the git
//! commands. The worker of the daemon (`mr_worker`) reads the files and the index, and the task
//! thread writes the mirror.

pub mod git;

use std::path::{Path, PathBuf};

use loom_api::{ContextName, ForgeKind, RepoIdentity, RepoSummary, TaskId};
use loom_forge::model::{MrKey, MrState, Sha};
use loom_store::IndexedTask;
use loom_store::port::TaskLocation;
use loom_store::tasks::IndexedState;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::review_sync::{VersionRecord, review_key};

/// The directory of the patches in an MR mirror.
pub const DIFFS_DIR: &str = "diffs";
/// The field of `mr.json` that keeps the clone chosen.
const CLONE_FIELD: &str = "clone";

/// The clone that holds the refs of an MR (`clone` in `mr.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MrClone {
    /// The working tree where git runs.
    pub path: PathBuf,
    /// The remote that names the target project of the MR.
    pub remote: String,
}

/// Where the patch of a version comes from (`diff` in `versions.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    /// `git diff -M <merge base> <head>` in a clone.
    Local,
    /// The per-file patches of the forge: no context expansion, and large files can be missing.
    Reduced,
}

/// An indexed repository that can hold the refs of an MR.
#[derive(Debug, Clone)]
pub struct CloneCandidate<'a> {
    /// The repository.
    pub repo: &'a RepoSummary,
    /// Its common directory, resolved: the same for each worktree of one clone.
    pub common_dir: PathBuf,
    /// It is the main worktree of its clone.
    pub main: bool,
}

/// The remote of `identity` that names the project: the primary one first, then the order of
/// the configuration. Case ignored, as the forges do. Pure.
#[must_use]
pub fn project_remote<'a>(
    identity: &'a RepoIdentity,
    host: &str,
    project: &str,
) -> Option<&'a str> {
    let names_it = |remote: &&loom_api::Remote| {
        remote.location.as_ref().is_some_and(|location| {
            location.host.eq_ignore_ascii_case(host) && location.path.eq_ignore_ascii_case(project)
        })
    };
    let primary = identity.primary_remote.as_deref();
    identity
        .remotes
        .iter()
        .filter(names_it)
        .find(|remote| Some(remote.name.as_str()) == primary)
        .or_else(|| identity.remotes.iter().find(names_it))
        .map(|remote| remote.name.as_str())
}

/// The clone of an MR: `kept` until it is gone, otherwise the main worktree of the most recently
/// used clone, then the shortest path. Pure.
#[must_use]
pub fn choose_clone(
    kept: Option<&MrClone>,
    candidates: &[CloneCandidate<'_>],
    host: &str,
    project: &str,
) -> Option<MrClone> {
    fn path<'a>(candidate: &'a CloneCandidate<'_>) -> &'a Path {
        candidate.repo.identity.path.as_path()
    }
    let of_project: Vec<(&CloneCandidate<'_>, &str)> = candidates
        .iter()
        .filter_map(|candidate| {
            project_remote(&candidate.repo.identity, host, project)
                .map(|remote| (candidate, remote))
        })
        .collect();
    // The last use of a clone is the last use of any of its worktrees.
    let clone_used = |candidate: &CloneCandidate<'_>| {
        of_project
            .iter()
            .filter(|(other, _)| other.common_dir == candidate.common_dir)
            .filter_map(|(other, _)| other.repo.last_used)
            .max()
    };
    let chosen = kept
        .and_then(|kept| {
            of_project
                .iter()
                .find(|(candidate, _)| path(candidate) == kept.path)
        })
        .or_else(|| {
            of_project.iter().min_by(|(a, _), (b, _)| {
                clone_used(b)
                    .cmp(&clone_used(a))
                    .then_with(|| b.main.cmp(&a.main))
                    .then_with(|| path(a).as_os_str().len().cmp(&path(b).as_os_str().len()))
                    .then_with(|| path(a).cmp(path(b)))
            })
        })?;
    Some(MrClone {
        path: path(chosen.0).to_path_buf(),
        remote: chosen.1.to_owned(),
    })
}

/// A name that cannot be a part of a ref.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("`{0}` cannot be a part of a git ref")]
pub struct RefNameError(pub String);

/// The prefix of the refs of an MR.
#[must_use]
pub fn ref_prefix(remote: &str, number: u64) -> String {
    format!("refs/loom/mr/{remote}/{number}/")
}

/// The ref that follows the head of an MR.
#[must_use]
pub fn head_ref(remote: &str, number: u64) -> String {
    format!("{}head", ref_prefix(remote, number))
}

/// The ref that keeps the last reviewed head of an MR.
#[must_use]
pub fn reviewed_ref(remote: &str, number: u64) -> String {
    format!("{}reviewed", ref_prefix(remote, number))
}

/// Whether `name` can be a part of a ref, by the rules of `git check-ref-format`. The base
/// branch comes from the forge, and a `:` in it would write another ref than the one asked for.
fn is_ref_part(name: &str) -> bool {
    let bad_byte = name.bytes().any(|byte| {
        byte.is_ascii_control()
            || matches!(byte, b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
    });
    let bad_component = name.split('/').any(|component| {
        component.is_empty()
            || component.starts_with('.')
            // Case-sensitive, as git is.
            || component.strip_suffix(".lock").is_some()
    });
    !bad_byte
        && !bad_component
        && !name.starts_with('-')
        && !name.ends_with('.')
        && !name.contains("..")
        && !name.contains("@{")
}

/// The refspecs of the head of the MR `key` and of its base branch. Pure.
///
/// # Errors
///
/// [`RefNameError`] when the remote or the base branch cannot be a part of a ref.
pub fn fetch_refspecs(
    key: &MrKey,
    remote: &str,
    base_ref: &str,
) -> Result<[String; 2], RefNameError> {
    for name in [remote, base_ref] {
        if !is_ref_part(name) {
            return Err(RefNameError(name.to_owned()));
        }
    }
    let number = key.number();
    let source = match key.forge() {
        ForgeKind::Github => format!("refs/pull/{number}/head"),
        ForgeKind::Gitlab => format!("refs/merge-requests/{number}/head"),
    };
    let prefix = ref_prefix(remote, number);
    Ok([
        format!("+{source}:{prefix}head"),
        format!("+refs/heads/{base_ref}:{prefix}base"),
    ])
}

/// How the current head of an MR gets its patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffPlan {
    /// It has the patch that it can have.
    Keep,
    /// Fetch the refs, then `git diff`.
    FromClone,
    /// The per-file patches of the forge.
    FromForge,
}

/// The plan for the current head of an MR: `recorded` is the mark of its version in
/// `versions.json`, `patch_exists` tells whether its file is in this mirror. Pure.
///
/// A merged or closed MR gets no diff. A head with its patch keeps it, except a reduced patch
/// when a clone can give the full one. The plan asks for the same work again until the record
/// is written, so a fetch that failed or met a lock is done again at the next poll.
#[must_use]
pub const fn diff_plan(
    state: MrState,
    recorded: Option<DiffKind>,
    patch_exists: bool,
    has_clone: bool,
) -> DiffPlan {
    match state {
        MrState::Merged | MrState::Closed => return DiffPlan::Keep,
        MrState::Open => {}
    }
    match (recorded, patch_exists, has_clone) {
        (Some(DiffKind::Local), true, _) | (Some(DiffKind::Reduced), true, false) => DiffPlan::Keep,
        (Some(DiffKind::Reduced) | None, true, true)
        | (Some(DiffKind::Local | DiffKind::Reduced) | None, false, true) => DiffPlan::FromClone,
        (None, true, false) | (Some(DiffKind::Local | DiffKind::Reduced) | None, false, false) => {
            DiffPlan::FromForge
        }
    }
}

/// The diff of one version, for the task thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionDiff {
    /// The head of the version.
    pub head: Sha,
    /// The merge base that git computed; `None` for a patch of the forge.
    pub merge_base: Option<Sha>,
    /// Where the patch comes from.
    pub kind: DiffKind,
    /// The patch: bytes, since a file need not be UTF-8.
    pub patch: Vec<u8>,
}

/// What the worker hands to the task thread for the mirror of one MR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRecord {
    /// The context of the store.
    pub context: ContextName,
    /// The Review task.
    pub id: TaskId,
    /// The MR.
    pub key: MrKey,
    /// The clone chosen.
    pub clone: Option<MrClone>,
    /// The new diffs.
    pub diffs: Vec<VersionDiff>,
}

/// `versions` with each of `diffs` recorded on its version. Pure.
#[must_use]
pub fn record_diffs(versions: &[VersionRecord], diffs: &[VersionDiff]) -> Vec<VersionRecord> {
    let mut versions = versions.to_vec();
    for diff in diffs {
        if let Some(record) = versions
            .iter_mut()
            .find(|record| record.version.head_sha == diff.head)
        {
            record.diff = Some(diff.kind);
            // The merge base of the forge stays when there is one.
            if record.version.merge_base.is_none() {
                record.version.merge_base.clone_from(&diff.merge_base);
            }
        }
    }
    versions
}

/// The clone in the bytes of an `mr.json`. Pure.
#[must_use]
pub fn read_clone(mr_json: &[u8]) -> Option<MrClone> {
    let value: serde_json::Value = serde_json::from_slice(mr_json).ok()?;
    serde_json::from_value(value.get(CLONE_FIELD)?.clone()).ok()
}

/// `snapshot`, a JSON object, with `clone` set or removed.
///
/// # Errors
///
/// The error of `serde_json`, which the model types never give.
pub fn set_clone(
    snapshot: &mut serde_json::Value,
    clone: Option<&MrClone>,
) -> Result<(), serde_json::Error> {
    if let Some(object) = snapshot.as_object_mut() {
        match clone {
            Some(clone) => {
                object.insert(CLONE_FIELD.to_owned(), serde_json::to_value(clone)?);
            }
            None => {
                object.remove(CLONE_FIELD);
            }
        }
    }
    Ok(())
}

/// The bytes of an `mr.json` with `clone` set or removed. Pure.
///
/// # Errors
///
/// The error of `serde_json` when the bytes are not a JSON object.
pub fn with_clone(mr_json: &[u8], clone: Option<&MrClone>) -> Result<Vec<u8>, serde_json::Error> {
    let mut snapshot: serde_json::Value = serde_json::from_slice(mr_json)?;
    set_clone(&mut snapshot, clone)?;
    let mut bytes = serde_json::to_vec_pretty(&snapshot)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// The name of the patch of the version whose head is `head`.
#[must_use]
pub fn patch_name(head: &Sha) -> String {
    format!("{}.patch", head.as_str())
}

/// The path of the patch of `head` in the mirror `dir`.
#[must_use]
pub fn patch_path(dir: &Path, head: &Sha) -> PathBuf {
    dir.join(DIFFS_DIR).join(patch_name(head))
}

/// A Review task of the index, reduced to what the cleanup of its refs needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRow {
    /// The task.
    pub id: TaskId,
    /// Its context.
    pub context: ContextName,
    /// Its MR.
    pub key: MrKey,
    /// The host of its link.
    pub host: Option<String>,
    /// Active or archived.
    pub location: TaskLocation,
}

/// The Review tasks of `rows`, a list of the index. Pure.
#[must_use]
pub fn review_rows(rows: &[IndexedTask]) -> Vec<ReviewRow> {
    rows.iter()
        .filter_map(|row| match &row.state {
            IndexedState::Valid { task, .. } => {
                let (key, host) = review_key(&task.kind, &task.links)?;
                Some(ReviewRow {
                    id: row.id.clone(),
                    context: row.context.clone(),
                    key,
                    host,
                    location: row.location,
                })
            }
            IndexedState::Invalid { .. } => None,
        })
        .collect()
}

/// The archived tasks of `context` on `host` whose refs go. Pure.
///
/// A clone has one set of refs per MR, so they stay while an active task of any context has the
/// MR (a new task after the archive, or the task of another account).
#[must_use]
pub fn refs_to_clean<'a>(
    rows: &'a [ReviewRow],
    context: &ContextName,
    host: &str,
) -> Vec<&'a ReviewRow> {
    let on_host = |row: &ReviewRow| row.host.as_deref() == Some(host);
    rows.iter()
        .filter(|row| row.context == *context && on_host(row))
        .filter(|row| matches!(row.location, TaskLocation::Archived(_)))
        .filter(|row| {
            !rows.iter().any(|other| {
                other.location == TaskLocation::Active && on_host(other) && other.key == row.key
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
