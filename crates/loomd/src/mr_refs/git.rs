//! The git commands of MR refs and diffs. They write only under `refs/loom/`.
//!
//! Through the `git` CLI (ADR-0008) with the environment of automatic updates (`repo_update`):
//! `GIT_TERMINAL_PROMPT=0`, SSH `BatchMode` unless the user chose an SSH command, a timeout, and
//! no token.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::time::Duration;

use loom_api::{RemoteProblem, RepoSummary, RepoUpdatePolicy};
use loom_forge::model::{MrKey, MrVersion, Sha};
use loom_repo::update::UpdatePolicies;
use loom_repo::{classify_remote_failure, git_dirs, is_busy};
use tracing::debug;

use super::{
    CloneCandidate, MrClone, base_ref_name, fetch_refspecs, head_ref, is_missing_ref,
    project_remote, ref_prefix, reviewed_ref,
};
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
    /// It failed.
    Failed {
        /// Why.
        error: String,
        /// What git's output says of the remote.
        problem: RemoteProblem,
    },
}

fn step(result: Result<(), Failure>) -> Step {
    match result {
        Ok(()) => Step::Done,
        Err(Failure::Exit { stderr_tail, .. }) if is_busy(&stderr_tail) => Step::Busy(stderr_tail),
        Err(failure) => {
            let problem = match &failure {
                Failure::Exit { stderr_tail, .. } => classify_remote_failure(stderr_tail),
                Failure::Timeout { .. } => RemoteProblem::Network,
                Failure::Run(_) => RemoteProblem::Other,
            };
            Step::Failed {
                error: describe(failure),
                problem,
            }
        }
    }
}

const fn refused(error: String) -> Step {
    Step::Failed {
        error,
        problem: RemoteProblem::Other,
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

/// The repositories of `repos` that can hold the refs of the project.
///
/// Each comes with its common directory. A repository with the policy `off` gets no git command,
/// so it is no candidate. A repository that cannot be read (gone from the disk) is none either.
#[must_use]
pub fn clone_candidates<'a>(
    repos: &'a [RepoSummary],
    host: &str,
    project: &str,
    policies: &UpdatePolicies,
) -> Vec<CloneCandidate<'a>> {
    repos
        .iter()
        .filter(|repo| project_remote(&repo.identity, host, project).is_some())
        .filter(|repo| !is_off(repo, policies))
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

/// Whether the repository has the update policy `off`: "no git command".
#[must_use]
pub fn is_off(repo: &RepoSummary, policies: &UpdatePolicies) -> bool {
    match policies.policy(&repo.identity.path, &repo.identity.context) {
        RepoUpdatePolicy::Off => true,
        RepoUpdatePolicy::Ff | RepoUpdatePolicy::Fetch => false,
    }
}

/// Fetches the head of the MR `key` and its base branch into `refs/loom/mr/<remote>/<n>/`.
///
/// When the remote no longer has the base branch, the head comes alone: the diff of a version
/// needs the base tip of that version, which the clone can have all the same.
///
/// `--refmap=` matters: without it, git also moves the configured remote-tracking ref of the
/// base branch (`refs/remotes/<remote>/<base>`). `--no-prune`, `--no-prune-tags`, `--no-tags` and
/// `--no-recurse-submodules` hold against the user's configuration, as in automatic updates. So
/// no ref outside `refs/loom/`, no `FETCH_HEAD`, no working tree and no index changes.
#[must_use]
pub fn fetch_mr(runner: &GitRunner, clone: &MrClone, key: &MrKey, base_ref: &str) -> Step {
    let refspecs = match fetch_refspecs(key, &clone.remote, base_ref) {
        Ok(refspecs) => refspecs,
        Err(error) => return refused(error.to_string()),
    };
    let git_dir = match git_dirs(&clone.path) {
        Ok((git_dir, _)) => git_dir,
        Err(error) => return refused(error.to_string()),
    };
    let git = Git::remote(runner, &clone.path, &git_dir);
    let [head, base] = &refspecs;
    let fetch = |specs: &[&String]| {
        let mut args: Vec<&OsStr> = vec![
            "fetch".as_ref(),
            "--no-tags".as_ref(),
            "--no-write-fetch-head".as_ref(),
            "--no-prune".as_ref(),
            "--no-prune-tags".as_ref(),
            "--no-recurse-submodules".as_ref(),
            "--refmap=".as_ref(),
            "--".as_ref(),
            clone.remote.as_ref(),
        ];
        args.extend(specs.iter().map(|spec| -> &OsStr { spec.as_ref() }));
        git.output(&args, runner.fetch_timeout).map(drop)
    };
    match fetch(&[head, base]) {
        Err(Failure::Exit { stderr_tail, .. })
            if is_missing_ref(&stderr_tail, &base_ref_name(base_ref)) =>
        {
            debug!(%key, base_ref, "the remote no longer has the base branch of the MR");
            step(fetch(&[head]))
        }
        other => step(other),
    }
}

/// `runner` with variables that no command of this module wants, on any version of git: no lazy
/// fetch of an object for a read that only asks if it is here (`GIT_NO_LAZY_FETCH`, the variable
/// of `--no-lazy-fetch`, which older versions ignore and do not refuse), and no attributes file
/// of the system.
fn with_variables(runner: &GitRunner, variables: &[(&str, &str)]) -> GitRunner {
    let mut runner = runner.clone();
    runner.env.extend(
        variables
            .iter()
            .map(|(name, value)| (OsString::from(name), Some(OsString::from(value)))),
    );
    runner
}

/// Whether the commit `sha` is in the clone at `workdir`. It never asks a promisor remote: a
/// partial clone would fetch the commit, and tell that it was here. The settings of a remote
/// command stay for a version of git that ignores the variable.
fn has_commit(runner: &GitRunner, workdir: &Path, sha: &Sha) -> Result<bool, Failure> {
    let runner = with_variables(runner, &[("GIT_NO_LAZY_FETCH", "1")]);
    let git_dir = workdir.join(".git");
    let git = Git::remote(&runner, workdir, &git_dir);
    let object = format!("{}^{{commit}}", sha.as_str());
    let args: [&OsStr; 3] = ["cat-file".as_ref(), "-e".as_ref(), object.as_ref()];
    match git.output(&args, LOCAL_TIMEOUT) {
        Ok(_) => Ok(true),
        Err(Failure::Exit { status, .. }) if status.code() == Some(1) => Ok(false),
        Err(Failure::Exit {
            status,
            stderr_tail,
        }) if status.code() == Some(128) && stderr_tail.contains("Not a valid object name") => {
            Ok(false)
        }
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
/// `color.ui`, `diff.context`, `diff.submodule`…), or the hashes of [`loom_forge::diff`] would
/// differ. The attributes are those of the commit, not of the working tree (`--attr-source`, git
/// 2.41). Older versions of git refuse the option: then the attributes of the working tree count.
fn diff(git: &Git<'_>, from: &Sha, to: &Sha, limit: usize) -> Result<Vec<u8>, String> {
    let attr_source = format!("--attr-source={}", to.as_str());
    let options: [&OsStr; 19] = [
        "-c".as_ref(),
        "core.quotePath=false".as_ref(),
        "-c".as_ref(),
        "core.attributesFile=/dev/null".as_ref(),
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
        "--submodule=short".as_ref(),
        "--ignore-submodules=none".as_ref(),
        "--src-prefix=a/".as_ref(),
    ];
    let ends: [&OsStr; 5] = [
        "--dst-prefix=b/".as_ref(),
        "-O/dev/null".as_ref(),
        from.as_str().as_ref(),
        to.as_str().as_ref(),
        "--".as_ref(),
    ];
    let mut args: Vec<&OsStr> = vec![attr_source.as_ref()];
    args.extend(options);
    args.extend(ends);
    // One byte more than the limit: it tells a patch over the limit from one at the limit.
    let keep = Some(limit.saturating_add(1));
    match git.output_bytes(&args, LOCAL_TIMEOUT, keep) {
        Err(Failure::Exit { stderr_tail, .. }) if stderr_tail.contains("--attr-source") => {
            debug!("this git has no --attr-source: the attributes of the working tree count");
            git.output_bytes(&args[1..], LOCAL_TIMEOUT, keep)
                .map_err(describe)
        }
        other => other.map_err(describe),
    }
}

/// The diff of a version in a clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDiff {
    /// The merge base used.
    pub merge_base: Sha,
    /// The patch; empty when it is over the limit.
    pub patch: Vec<u8>,
    /// The patch is over the limit.
    pub over_limit: bool,
}

/// The diff of `version` in the clone at `workdir`: `git diff -M <merge base> <head>`. `Ok(None)`
/// when the clone does not have the head. A patch of more than `limit` bytes is not kept.
///
/// The merge base is the forge's (GitLab). Otherwise it is `git merge-base <base.sha> <head>`
/// with the base tip that the forge used for this version (GitHub), not the fetched base ref,
/// which may have moved since and would shift the hunks from the forge's.
///
/// `git diff` has the settings of a remote command: in a partial clone, it fetches the blobs
/// that it reads.
///
/// # Errors
///
/// The reason when git fails, for example when the clone does not have the base.
pub fn version_diff(
    runner: &GitRunner,
    workdir: &Path,
    version: &MrVersion,
    limit: usize,
) -> Result<Option<LocalDiff>, String> {
    let head = &version.head_sha;
    if !has_commit(runner, workdir, head).map_err(describe)? {
        return Ok(None);
    }
    let runner = with_variables(runner, &[("GIT_ATTR_NOSYSTEM", "1")]);
    let git_dir = workdir.join(".git");
    let git = Git::remote(&runner, workdir, &git_dir);
    let merge_base = match &version.merge_base {
        Some(merge_base) => merge_base.clone(),
        None => merge_base(&git, &version.base_sha, head)?,
    };
    let patch = diff(&git, &merge_base, head, limit)?;
    let over_limit = patch.len() > limit;
    Ok(Some(LocalDiff {
        merge_base,
        patch: if over_limit { Vec::new() } else { patch },
        over_limit,
    }))
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
    match has_commit(runner, &clone.path, sha) {
        Ok(true) => {}
        Ok(false) => return Reviewed::NoCommit,
        Err(failure) => return Reviewed::Not(refused(describe(failure))),
    }
    let git = Git::local(runner, &clone.path);
    let name = reviewed_ref(&clone.remote, number);
    let args: [&OsStr; 3] = ["update-ref".as_ref(), name.as_ref(), sha.as_str().as_ref()];
    match step(git.output(&args, LOCAL_TIMEOUT).map(drop)) {
        Step::Done => Reviewed::Set,
        other => Reviewed::Not(other),
    }
}

/// Whether the clone has the head ref of the MR. A command that fails says yes: only a sure "no"
/// asks for a fetch.
#[must_use]
pub fn has_head_ref(runner: &GitRunner, clone: &MrClone, number: u64) -> bool {
    let git = Git::local(runner, &clone.path);
    let name = head_ref(&clone.remote, number);
    let args: [&OsStr; 4] = [
        "rev-parse".as_ref(),
        "-q".as_ref(),
        "--verify".as_ref(),
        name.as_ref(),
    ];
    !matches!(
        git.output(&args, LOCAL_TIMEOUT),
        Err(Failure::Exit { status, .. }) if status.code() == Some(1)
    )
}

/// Deletes the refs of one MR, one `git update-ref -d` each. Only their exact names: the remote
/// `foo/12` has refs under the prefix of MR 12 of the remote `foo`.
#[must_use]
pub fn delete_refs(runner: &GitRunner, clone: &MrClone, number: u64) -> Step {
    let git = Git::local(runner, &clone.path);
    let prefix = ref_prefix(&clone.remote, number);
    let names = ["head", "base", "reviewed"].map(|leaf| format!("{prefix}{leaf}"));
    let mut args: Vec<&OsStr> = vec!["for-each-ref".as_ref(), "--format=%(refname)".as_ref()];
    args.extend(names.iter().map(|name| -> &OsStr { name.as_ref() }));
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
