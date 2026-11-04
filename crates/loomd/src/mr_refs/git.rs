//! The git commands of MR refs and diffs. They write only under `refs/loom/`.
//!
//! Through the `git` CLI (ADR-0008) with the environment of automatic updates (`repo_update`):
//! `GIT_TERMINAL_PROMPT=0`, SSH `BatchMode` unless the user chose an SSH command, a timeout, and
//! no token.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::time::Duration;

use loom_api::RepoSummary;
use loom_forge::model::{MrKey, MrVersion, Sha};
use loom_repo::{git_dirs, is_busy};
use tracing::debug;

use super::{CloneCandidate, MrClone, fetch_refspecs, project_remote, ref_prefix, reviewed_ref};
use crate::repo_update::{Failure, Git, GitRunner};

/// Longest wait for a local command: `git diff`, `git merge-base`, `git update-ref`.
const LOCAL_TIMEOUT: Duration = Duration::from_mins(2);

/// How a git step ended. A lock of another git process is not a failure: the next poll does the
/// step again, and no lock file is ever removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// It succeeded.
    Done,
    /// Another git process holds a lock.
    Busy(String),
    /// It failed: why.
    Failed(String),
}

fn step(result: Result<(), Failure>) -> Step {
    match result {
        Ok(()) => Step::Done,
        Err(Failure::Exit { stderr_tail, .. }) if is_busy(&stderr_tail) => Step::Busy(stderr_tail),
        Err(failure) => Step::Failed(describe(failure)),
    }
}

fn describe(failure: Failure) -> String {
    match failure {
        Failure::Run(error) => format!("cannot run git: {error}"),
        Failure::Timeout { after, stderr_tail } => {
            format!("timed out after {}s: {stderr_tail}", after.as_secs())
        }
        Failure::Exit {
            status,
            stderr_tail,
        } => format!("{status}: {stderr_tail}"),
    }
}

/// The repositories of `repos` that have a remote of the project, each with its common
/// directory. A repository that cannot be read (gone from the disk) is not a candidate.
#[must_use]
pub fn clone_candidates<'a>(
    repos: &'a [RepoSummary],
    host: &str,
    project: &str,
) -> Vec<CloneCandidate<'a>> {
    repos
        .iter()
        .filter(|repo| project_remote(&repo.identity, host, project).is_some())
        .filter_map(|repo| {
            let (git_dir, common_dir) = git_dirs(&repo.identity.path)
                .inspect_err(|error| debug!(%error, "not a clone of the MR"))
                .ok()?;
            Some(CloneCandidate {
                repo,
                main: git_dir == common_dir,
                // `<git dir>/../..` for a linked worktree.
                common_dir: fs::canonicalize(&common_dir).unwrap_or(common_dir),
            })
        })
        .collect()
}

/// Fetches the head of the MR `key` and its base branch into `refs/loom/mr/<remote>/<n>/`.
///
/// `--refmap=` matters: without it, git also moves the configured remote-tracking ref of the
/// base branch (`refs/remotes/<remote>/<base>`). `--no-prune`, `--no-prune-tags`, `--no-tags` and
/// `--no-recurse-submodules` hold against the user's configuration, as in automatic updates. So
/// no ref outside `refs/loom/`, no `FETCH_HEAD`, no working tree and no index changes.
#[must_use]
pub fn fetch_mr(runner: &GitRunner, clone: &MrClone, key: &MrKey, base_ref: &str) -> Step {
    let refspecs = match fetch_refspecs(key, &clone.remote, base_ref) {
        Ok(refspecs) => refspecs,
        Err(error) => return Step::Failed(error.to_string()),
    };
    let git_dir = match git_dirs(&clone.path) {
        Ok((git_dir, _)) => git_dir,
        Err(error) => return Step::Failed(error.to_string()),
    };
    let git = Git::remote(runner, &clone.path, &git_dir);
    let [head, base] = &refspecs;
    let args: [&OsStr; 11] = [
        "fetch".as_ref(),
        "--no-tags".as_ref(),
        "--no-write-fetch-head".as_ref(),
        "--no-prune".as_ref(),
        "--no-prune-tags".as_ref(),
        "--no-recurse-submodules".as_ref(),
        "--refmap=".as_ref(),
        "--".as_ref(),
        clone.remote.as_ref(),
        head.as_ref(),
        base.as_ref(),
    ];
    step(git.output(&args, runner.fetch_timeout).map(drop))
}

/// Whether the commit `sha` is in the clone.
fn has_commit(git: &Git<'_>, sha: &Sha) -> Result<bool, Failure> {
    let object = format!("{}^{{commit}}", sha.as_str());
    let args: [&OsStr; 3] = ["cat-file".as_ref(), "-e".as_ref(), object.as_ref()];
    match git.output(&args, LOCAL_TIMEOUT) {
        Ok(_) => Ok(true),
        Err(Failure::Exit { status, .. }) if status.code() == Some(1) => Ok(false),
        // `fatal: Not a valid object name` when the object is missing.
        Err(Failure::Exit { status, .. }) if status.code() == Some(128) => Ok(false),
        Err(failure) => Err(failure),
    }
}

/// `git merge-base <a> <b>`.
fn merge_base(git: &Git<'_>, a: &Sha, b: &Sha) -> Result<Sha, String> {
    let args: [&OsStr; 3] = [
        "merge-base".as_ref(),
        a.as_str().as_ref(),
        b.as_str().as_ref(),
    ];
    let output = git.output(&args, LOCAL_TIMEOUT).map_err(describe)?;
    Sha::try_from(output.trim().to_owned()).map_err(|error| error.to_string())
}

/// The patch of `git diff -M <from> <to>`, the same for each user: each option here holds
/// against a configuration that changes the output (`diff.noprefix`, `diff.external`,
/// `color.ui`, `diff.context`…), or the hashes of [`loom_forge::diff`] would differ.
fn diff(git: &Git<'_>, from: &Sha, to: &Sha) -> Result<Vec<u8>, String> {
    let args: [&OsStr; 20] = [
        "-c".as_ref(),
        "core.quotePath=false".as_ref(),
        "diff".as_ref(),
        "-M".as_ref(),
        "--patch".as_ref(),
        "-U3".as_ref(),
        "--inter-hunk-context=0".as_ref(),
        "--diff-algorithm=myers".as_ref(),
        "--indent-heuristic".as_ref(),
        "--full-index".as_ref(),
        "--no-color".as_ref(),
        "--no-ext-diff".as_ref(),
        "--no-textconv".as_ref(),
        "--no-relative".as_ref(),
        "--src-prefix=a/".as_ref(),
        "--dst-prefix=b/".as_ref(),
        "-O/dev/null".as_ref(),
        from.as_str().as_ref(),
        to.as_str().as_ref(),
        "--".as_ref(),
    ];
    git.output_bytes(&args, LOCAL_TIMEOUT).map_err(describe)
}

/// The diff of a version in a clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDiff {
    /// The merge base used.
    pub merge_base: Sha,
    /// The patch.
    pub patch: Vec<u8>,
}

/// The diff of `version` in the clone at `workdir`: `git diff -M <merge base> <head>`. `Ok(None)`
/// when the clone does not have the head.
///
/// The merge base is the forge's (GitLab). Otherwise it is `git merge-base <base.sha> <head>`
/// with the base tip that the forge used for this version (GitHub), not the fetched base ref,
/// which may have moved since and would shift the hunks from the forge's.
///
/// # Errors
///
/// The reason when git fails, for example when the clone does not have the base.
pub fn version_diff(
    runner: &GitRunner,
    workdir: &Path,
    version: &MrVersion,
) -> Result<Option<LocalDiff>, String> {
    let git = Git::local(runner, workdir);
    let head = &version.head_sha;
    if !has_commit(&git, head).map_err(describe)? {
        return Ok(None);
    }
    let merge_base = match &version.merge_base {
        Some(merge_base) => merge_base.clone(),
        None => merge_base(&git, &version.base_sha, head)?,
    };
    let patch = diff(&git, &merge_base, head)?;
    Ok(Some(LocalDiff { merge_base, patch }))
}

/// What became of the `reviewed` ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reviewed {
    /// The ref points at the commit.
    Set,
    /// The clone does not have the commit: a fetch may bring it.
    NoCommit,
    /// The step did not end well.
    Not(Step),
}

/// Points `refs/loom/mr/<remote>/<n>/reviewed` at `sha`, so that git keeps it through force
/// pushes and `git gc`.
#[must_use]
pub fn set_reviewed(runner: &GitRunner, clone: &MrClone, number: u64, sha: &Sha) -> Reviewed {
    let git = Git::local(runner, &clone.path);
    match has_commit(&git, sha) {
        Ok(true) => {}
        Ok(false) => return Reviewed::NoCommit,
        Err(failure) => return Reviewed::Not(Step::Failed(describe(failure))),
    }
    let name = reviewed_ref(&clone.remote, number);
    let args: [&OsStr; 3] = ["update-ref".as_ref(), name.as_ref(), sha.as_str().as_ref()];
    match step(git.output(&args, LOCAL_TIMEOUT).map(drop)) {
        Step::Done => Reviewed::Set,
        other => Reviewed::Not(other),
    }
}

/// Deletes every ref of `refs/loom/mr/<remote>/<n>/`, one `git update-ref -d` each.
#[must_use]
pub fn delete_refs(runner: &GitRunner, clone: &MrClone, number: u64) -> Step {
    let git = Git::local(runner, &clone.path);
    let prefix = ref_prefix(&clone.remote, number);
    let args: [&OsStr; 3] = [
        "for-each-ref".as_ref(),
        "--format=%(refname)".as_ref(),
        prefix.as_ref(),
    ];
    let listed = match git.output(&args, LOCAL_TIMEOUT) {
        Ok(listed) => listed,
        Err(failure) => return step(Err(failure)),
    };
    for name in listed.lines() {
        let args: [&OsStr; 3] = ["update-ref".as_ref(), "-d".as_ref(), name.as_ref()];
        match step(git.output(&args, LOCAL_TIMEOUT).map(drop)) {
            Step::Done => {}
            other => return other,
        }
    }
    Step::Done
}
