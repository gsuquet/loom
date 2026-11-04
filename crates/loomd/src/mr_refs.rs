//! MR refs and diffs (ADR-0016, decisions 3 and 4).
//!
//! Which clone and remote hold the refs of an MR, which refspecs fetch them, when a version needs
//! a diff, and how a diff is recorded in the mirror. Pure, except [`git`], which runs the git
//! commands. The worker of the daemon (`mr_worker`) reads the files and the index, and the task
//! thread writes the mirror.

pub mod git;

use std::path::{Path, PathBuf};

use loom_api::{ContextName, ForgeKind, RemoteProblem, RepoIdentity, RepoSummary, TaskId};
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
/// The field of `mr.json` that lists the clones whose refs may still be there.
const RETIRED_FIELD: &str = "retired_clones";
/// The largest patch that a mirror keeps. The store and its backup hold each patch, in a git
/// history that keeps it: over this, the version falls back to the patches of the forge.
pub const PATCH_CAP: usize = 5 * 1024 * 1024;
/// The largest total of the patches of one MR, for the same reason.
pub const MR_CAP: usize = 50 * 1024 * 1024;
/// The most polls that a permanent failure of one MR waits for between two tries, minus one: the
/// limit of the backoff of automatic updates, 8 times the interval.
const MAX_SKIPPED_POLLS: u32 = 7;

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
    /// The local diff is over [`PATCH_CAP`] or [`MR_CAP`]. The current head has the patches of
    /// the forge (or their list) after a note; an older version has the note alone.
    #[serde(rename = "reduced_too_large")]
    TooLarge,
}

/// The clones of an MR in `mr.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MrClones {
    /// The clone chosen.
    pub clone: Option<MrClone>,
    /// The clones that held its refs before. Each stays listed until its refs are deleted, so
    /// that the cleanup still knows the path when no clone replaces it.
    pub retired: Vec<MrClone>,
}

/// The clones after the choice of `chosen`. Pure.
#[must_use]
pub fn next_clones(kept: &MrClones, chosen: Option<MrClone>) -> MrClones {
    let mut retired = kept.retired.clone();
    if let Some(old) = &kept.clone
        && Some(old) != chosen.as_ref()
        && !retired.contains(old)
    {
        retired.push(old.clone());
    }
    retired.retain(|old| Some(old) != chosen.as_ref());
    MrClones {
        clone: chosen,
        retired,
    }
}

/// Whether the plan `Keep` still has to fetch: the refs are in the clone that the patch came
/// from, or the user deleted them. Pure.
#[must_use]
pub const fn needs_refs(plan: DiffPlan, same_clone: bool, head_ref_present: bool) -> bool {
    match plan {
        DiffPlan::Keep => !same_clone || !head_ref_present,
        DiffPlan::FromClone | DiffPlan::FromForge => false,
    }
}

/// Whether a failure of git with `problem` is one that the next MRs of its endpoint would meet
/// too, each after its own timeout. Pure.
#[must_use]
pub const fn stops_the_endpoint(problem: RemoteProblem) -> bool {
    match problem {
        RemoteProblem::SshAuth | RemoteProblem::HttpsAuth | RemoteProblem::Network => true,
        RemoteProblem::NotFound | RemoteProblem::Other => false,
    }
}

/// The backoff of one MR after a failure that is its own, counted in polls: no clock, and it
/// follows the interval of its host. It doubles, as for automatic updates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Retry {
    failures: u32,
    skip: u32,
}

impl Retry {
    /// Whether this poll tries; otherwise it counts as one poll waited. Pure.
    pub const fn due(&mut self) -> bool {
        if self.skip == 0 {
            true
        } else {
            self.skip -= 1;
            false
        }
    }

    /// A try failed. Pure.
    pub fn failed(&mut self) {
        self.failures = self.failures.saturating_add(1);
        let waits = 1_u32
            .checked_shl(self.failures - 1)
            .map_or(MAX_SKIPPED_POLLS, |factor| factor - 1);
        self.skip = waits.min(MAX_SKIPPED_POLLS);
    }
}

/// The bytes that the next patch of an MR can have, when its patches take `used` bytes. Pure.
#[must_use]
pub fn patch_budget(used: u64) -> usize {
    let left = (MR_CAP as u64).saturating_sub(used);
    usize::try_from(left).unwrap_or(usize::MAX).min(PATCH_CAP)
}

/// The line at the start of a patch that says why it is not the local diff: the diff is over
/// the room of `limit` bytes that it had. No file: a patch reader skips it. Pure.
#[must_use]
pub fn too_large_note(limit: usize) -> Vec<u8> {
    format!("# loom: the local diff is over the limit of {limit} bytes for this patch\n")
        .into_bytes()
}

/// Whether git's standard error says that the remote has no ref `name`. Pure.
#[must_use]
pub fn is_missing_ref(stderr: &str, name: &str) -> bool {
    stderr.lines().any(|line| {
        line.trim()
            .strip_suffix(name)
            .is_some_and(|start| start.ends_with("couldn't find remote ref "))
    })
}

/// Whether the remote of `clone` now names another project than the one of the MR. Pure.
///
/// Its refs may then be of another MR with the number, and stay. A remote renamed or removed, or
/// a repository out of the index, tells nothing.
#[must_use]
pub fn remote_names_other_project(
    repos: &[RepoSummary],
    clone: &MrClone,
    host: &str,
    project: &str,
) -> bool {
    repos
        .iter()
        .find(|repo| repo.identity.path == clone.path)
        .and_then(|repo| {
            repo.identity
                .remotes
                .iter()
                .find(|remote| remote.name == clone.remote)
        })
        .and_then(|remote| remote.location.as_ref())
        .is_some_and(|location| {
            !(location.host.eq_ignore_ascii_case(host)
                && location.path.eq_ignore_ascii_case(project))
        })
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

/// The full name of the base branch `base_ref` on the remote.
#[must_use]
pub fn base_ref_name(base_ref: &str) -> String {
    format!("refs/heads/{base_ref}")
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
        format!("+{}:{prefix}base", base_ref_name(base_ref)),
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
        (Some(DiffKind::Local | DiffKind::TooLarge), true, _)
        | (Some(DiffKind::Reduced), true, false) => DiffPlan::Keep,
        (Some(DiffKind::Reduced) | None, true, true)
        | (Some(DiffKind::Local | DiffKind::Reduced | DiffKind::TooLarge) | None, false, true) => {
            DiffPlan::FromClone
        }
        (None, true, false)
        | (Some(DiffKind::Local | DiffKind::Reduced | DiffKind::TooLarge) | None, false, false) => {
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
    /// The clones after the choice.
    pub clones: MrClones,
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

/// The clones in the bytes of an `mr.json`. Pure.
#[must_use]
pub fn read_clones(mr_json: &[u8]) -> MrClones {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(mr_json) else {
        return MrClones::default();
    };
    MrClones {
        clone: value
            .get(CLONE_FIELD)
            .and_then(|clone| serde_json::from_value(clone.clone()).ok()),
        retired: value
            .get(RETIRED_FIELD)
            .and_then(|retired| serde_json::from_value(retired.clone()).ok())
            .unwrap_or_default(),
    }
}

/// `snapshot`, a JSON object, with `clones` set: a field without a value is removed.
///
/// # Errors
///
/// The error of `serde_json`, which the model types never give.
pub fn set_clones(
    snapshot: &mut serde_json::Value,
    clones: &MrClones,
) -> Result<(), serde_json::Error> {
    if let Some(object) = snapshot.as_object_mut() {
        match &clones.clone {
            Some(clone) => {
                object.insert(CLONE_FIELD.to_owned(), serde_json::to_value(clone)?);
            }
            None => {
                object.remove(CLONE_FIELD);
            }
        }
        if clones.retired.is_empty() {
            object.remove(RETIRED_FIELD);
        } else {
            object.insert(
                RETIRED_FIELD.to_owned(),
                serde_json::to_value(&clones.retired)?,
            );
        }
    }
    Ok(())
}

/// The bytes of an `mr.json` with `clones` set. Pure.
///
/// # Errors
///
/// The error of `serde_json` when the bytes are not a JSON object.
pub fn with_clones(mr_json: &[u8], clones: &MrClones) -> Result<Vec<u8>, serde_json::Error> {
    let mut snapshot: serde_json::Value = serde_json::from_slice(mr_json)?;
    set_clones(&mut snapshot, clones)?;
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
