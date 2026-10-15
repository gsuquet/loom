//! Automatic repository updates: the result of the last one, and `POST /v1/repos/sync`.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{RepoOperation, RepoUpdatePolicy, Timestamp};

/// The last update of a repository, by `loomd` on its schedule or by `POST /v1/repos/sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LastUpdate {
    /// When the update ended.
    pub time: Timestamp,
    /// The update policy it applied.
    pub policy: RepoUpdatePolicy,
    /// What happened, in order: the fetch or why there was none, the branch `HEAD` is on, then
    /// the default branch when it was moved or could not be. Never empty.
    pub outcomes: Vec<UpdateOutcome>,
}

impl LastUpdate {
    /// The update failed at some step: the fetch, the fast-forward or the default branch move.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.outcomes
            .iter()
            .any(|outcome| matches!(outcome, UpdateOutcome::Failed { .. }))
    }
}

/// One result of an update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateOutcome {
    /// The primary remote was fetched. No branch had to move.
    Fetched,
    /// The branch `HEAD` is on was fast-forwarded to its upstream.
    FastForwarded {
        /// Short branch name.
        branch: String,
        /// Commit id before (40 or 64 hexadecimal digits).
        from: String,
        /// Commit id after.
        to: String,
    },
    /// The local default branch, checked out nowhere, was fast-forwarded to its upstream.
    DefaultBranchMoved {
        /// Short branch name.
        branch: String,
        /// Commit id before.
        from: String,
        /// Commit id after.
        to: String,
    },
    /// A step was not done, on purpose.
    Skipped {
        /// Why.
        reason: SkipReason,
    },
    /// A git command failed or timed out.
    Failed {
        /// The step that failed.
        step: UpdateStep,
        /// What went wrong: exit status, timeout, or why git could not start.
        error: String,
        /// The end of git's standard error, at most 2,000 bytes.
        stderr_tail: String,
    },
}

impl fmt::Display for UpdateOutcome {
    /// One line: `fetched`, `fast-forwarded main 1a2b3c4..5d6e7f8`, `skipped: …`, `failed: …`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetched => f.write_str("fetched"),
            Self::FastForwarded { branch, from, to } => {
                write!(f, "fast-forwarded {branch} {}..{}", short(from), short(to))
            }
            Self::DefaultBranchMoved { branch, from, to } => write!(
                f,
                "moved default branch {branch} {}..{}",
                short(from),
                short(to)
            ),
            Self::Skipped { reason } => write!(f, "skipped: {reason}"),
            Self::Failed {
                step,
                error,
                stderr_tail,
            } => {
                write!(f, "failed: {step}: {error}")?;
                detail(
                    f,
                    stderr_tail
                        .lines()
                        .rev()
                        .find(|line| !line.trim().is_empty()),
                )
            }
        }
    }
}

/// `: <line>`, trimmed, when there is a line.
fn detail(f: &mut fmt::Formatter<'_>, line: Option<&str>) -> fmt::Result {
    line.map_or(Ok(()), |line| write!(f, ": {}", line.trim()))
}

/// The first 7 characters of a commit id.
fn short(id: &str) -> &str {
    id.get(..7).unwrap_or(id)
}

/// A step of an update that runs git.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStep {
    /// `git fetch` of the primary remote.
    Fetch,
    /// `git merge --ff-only` of the branch `HEAD` is on.
    FastForward,
    /// `git fetch <remote> <branch>:<branch>` of the default branch.
    MoveDefaultBranch,
}

impl UpdateStep {
    /// Name, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::FastForward => "fast_forward",
            Self::MoveDefaultBranch => "move_default_branch",
        }
    }
}

impl fmt::Display for UpdateStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A piece of repository state an update decision needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StateField {
    /// What `HEAD` points to.
    Head,
    /// Tracked changes.
    Dirty,
    /// Operation in progress.
    Operation,
    /// Upstream of the `HEAD` branch.
    Upstream,
    /// Commits of `HEAD` not in its upstream.
    Ahead,
    /// Commits of the upstream not in `HEAD`.
    Behind,
    /// The commits of `HEAD` and of its upstream.
    Commits,
    /// The default branch, its upstream and the worktrees that have it checked out.
    DefaultBranch,
}

impl StateField {
    /// Name, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Dirty => "dirty",
            Self::Operation => "operation",
            Self::Upstream => "upstream",
            Self::Ahead => "ahead",
            Self::Behind => "behind",
            Self::Commits => "commits",
            Self::DefaultBranch => "default_branch",
        }
    }
}

impl fmt::Display for StateField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a step of an update was not done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum SkipReason {
    /// Policy `off`: neither fetch nor fast-forward.
    PolicyOff,
    /// The repository has no remote to fetch.
    NoRemote,
    /// The repository cannot be opened.
    Unreadable {
        /// Why, in English.
        error: String,
    },
    /// A field the decision needs is unknown (time budget exceeded, unreadable).
    Unknown {
        /// The field.
        field: StateField,
    },
    /// An operation is in progress.
    Operation {
        /// The operation.
        operation: RepoOperation,
    },
    /// Tracked paths are modified, staged or conflicted.
    Dirty {
        /// How many.
        paths: u64,
    },
    /// `HEAD` is not on a branch.
    Detached,
    /// The branch `HEAD` is on has no commit.
    Unborn {
        /// Short branch name.
        branch: String,
    },
    /// The branch `HEAD` is on has no upstream, or its upstream ref does not exist.
    NoUpstream {
        /// Short branch name.
        branch: String,
    },
    /// The branch has commits its upstream does not have, and the reverse.
    Diverged {
        /// Short branch name.
        branch: String,
        /// Commits of the branch not in its upstream.
        ahead: u64,
        /// Commits of the upstream not in the branch.
        behind: u64,
    },
    /// `git merge --ff-only` refused, such as when an untracked file would be overwritten.
    MergeRefused {
        /// The end of git's standard error.
        stderr_tail: String,
    },
    /// The local default branch has commits its upstream does not have, and the reverse.
    DefaultBranchDiverged {
        /// Short branch name.
        branch: String,
        /// Commits of the branch not in its upstream.
        ahead: u64,
        /// Commits of the upstream not in the branch.
        behind: u64,
    },
    /// Another git process (an editor's automatic fetch, a command of the user) held a lock of
    /// the repository (`index.lock`, a ref lock) during this step. Retried at the next
    /// interval, without backoff. The lock is never removed.
    Busy {
        /// The step that met the lock.
        step: UpdateStep,
        /// The end of git's standard error.
        stderr_tail: String,
    },
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PolicyOff => f.write_str("update policy is off"),
            Self::NoRemote => f.write_str("no remote"),
            Self::Unreadable { error } => write!(f, "repository unreadable: {error}"),
            Self::Unknown { field } => write!(f, "{field} unknown"),
            Self::Operation { operation } => write!(f, "{operation} in progress"),
            Self::Dirty { paths: 1 } => f.write_str("1 changed tracked path"),
            Self::Dirty { paths } => write!(f, "{paths} changed tracked paths"),
            Self::Detached => f.write_str("HEAD detached"),
            Self::Unborn { branch } => write!(f, "branch {branch} has no commit"),
            Self::NoUpstream { branch } => write!(f, "branch {branch} has no upstream"),
            Self::Diverged {
                branch,
                ahead,
                behind,
            } => write!(
                f,
                "{branch} diverged from its upstream ({ahead} ahead, {behind} behind)"
            ),
            Self::MergeRefused { stderr_tail } => {
                f.write_str("git refused the fast-forward")?;
                detail(f, stderr_tail.lines().find(|line| !line.trim().is_empty()))
            }
            Self::DefaultBranchDiverged {
                branch,
                ahead,
                behind,
            } => write!(
                f,
                "default branch {branch} diverged from its upstream ({ahead} ahead, {behind} behind)"
            ),
            Self::Busy { step, stderr_tail } => {
                write!(
                    f,
                    "repository busy: another git process holds a lock ({step})"
                )?;
                detail(
                    f,
                    stderr_tail
                        .lines()
                        .find(|line| line.to_ascii_lowercase().contains("lock")),
                )
            }
        }
    }
}

/// Body of `POST /v1/repos/sync`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncRequest {
    /// Slugs of the repositories to update. If empty: every repository of the index.
    #[serde(default)]
    pub repos: Vec<String>,
    /// `true`: decide from the stored state, with no fetch and no merge.
    #[serde(default)]
    pub dry_run: bool,
}

/// Answer of `POST /v1/repos/sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncResult {
    /// One entry per repository, sorted by slug.
    pub repos: Vec<RepoSync>,
}

/// What `POST /v1/repos/sync` did, or would do, to one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepoSync {
    /// Slug of the repository.
    pub slug: String,
    /// What was done or would be done.
    pub result: SyncOutcome,
}

/// The result for one repository of `POST /v1/repos/sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncOutcome {
    /// The update ran. It is also stored as the repository's `last_update`.
    Updated {
        /// The update.
        update: LastUpdate,
    },
    /// Dry run: what the update would do, from the current state.
    Planned {
        /// The update policy that applies.
        policy: RepoUpdatePolicy,
        /// The actions, in order.
        actions: Vec<PlannedAction>,
    },
}

/// A step a dry run predicts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlannedAction {
    /// The step would not be done.
    Skip {
        /// Why.
        reason: SkipReason,
    },
    /// The primary remote would be fetched.
    Fetch,
    /// Whether branches move depends on what the fetch brings: decided after it.
    DecideAfterFetch,
}

impl fmt::Display for PlannedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Skip { reason } => write!(f, "skip: {reason}"),
            Self::Fetch => f.write_str("would fetch"),
            Self::DecideAfterFetch => f.write_str("then decide"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LastUpdate, PlannedAction, SkipReason, StateField, UpdateOutcome, UpdateStep};
    use crate::{RepoOperation, RepoUpdatePolicy, Timestamp};
    use serde_json::json;

    #[test]
    fn outcomes_serialize_tagged_by_kind() {
        let update = LastUpdate {
            time: Timestamp::from_unix_secs(1_791_792_000).unwrap(),
            policy: RepoUpdatePolicy::Ff,
            outcomes: vec![
                UpdateOutcome::Skipped {
                    reason: SkipReason::Dirty { paths: 2 },
                },
                UpdateOutcome::Failed {
                    step: UpdateStep::Fetch,
                    error: "exit status 128".to_owned(),
                    stderr_tail: "fatal: x\n".to_owned(),
                },
            ],
        };
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            json!({
                "time": "2026-10-12T08:00:00Z",
                "policy": "ff",
                "outcomes": [
                    { "kind": "skipped", "reason": { "code": "dirty", "paths": 2 } },
                    {
                        "kind": "failed",
                        "step": "fetch",
                        "error": "exit status 128",
                        "stderr_tail": "fatal: x\n"
                    }
                ]
            })
        );
        assert!(update.failed());
        for step in [
            UpdateStep::Fetch,
            UpdateStep::FastForward,
            UpdateStep::MoveDefaultBranch,
        ] {
            assert_eq!(serde_json::to_value(step).unwrap(), json!(step.to_string()));
        }
        assert_eq!(
            serde_json::to_value(StateField::DefaultBranch).unwrap(),
            json!(StateField::DefaultBranch.to_string())
        );
    }

    #[test]
    fn outcomes_display_on_one_line() {
        let id = |c: char| c.to_string().repeat(40);
        for (outcome, text) in [
            (UpdateOutcome::Fetched, "fetched"),
            (
                UpdateOutcome::FastForwarded {
                    branch: "main".to_owned(),
                    from: id('a'),
                    to: id('b'),
                },
                "fast-forwarded main aaaaaaa..bbbbbbb",
            ),
            (
                UpdateOutcome::DefaultBranchMoved {
                    branch: "main".to_owned(),
                    from: id('a'),
                    to: id('b'),
                },
                "moved default branch main aaaaaaa..bbbbbbb",
            ),
            (
                UpdateOutcome::Skipped {
                    reason: SkipReason::Operation {
                        operation: RepoOperation::Rebase,
                    },
                },
                "skipped: rebase in progress",
            ),
            (
                UpdateOutcome::Skipped {
                    reason: SkipReason::MergeRefused {
                        stderr_tail: "\nerror: untracked files would be overwritten\n\tf.txt\n"
                            .to_owned(),
                    },
                },
                "skipped: git refused the fast-forward: error: untracked files would be overwritten",
            ),
            (
                UpdateOutcome::Failed {
                    step: UpdateStep::Fetch,
                    error: "timed out after 120s".to_owned(),
                    stderr_tail: "a\nfatal: unreachable\n\n".to_owned(),
                },
                "failed: fetch: timed out after 120s: fatal: unreachable",
            ),
            (
                UpdateOutcome::Failed {
                    step: UpdateStep::FastForward,
                    error: "exit status 1".to_owned(),
                    stderr_tail: String::new(),
                },
                "failed: fast_forward: exit status 1",
            ),
        ] {
            assert_eq!(outcome.to_string(), text);
        }
        assert_eq!(
            SkipReason::Unknown {
                field: StateField::Dirty
            }
            .to_string(),
            "dirty unknown"
        );
        assert_eq!(
            SkipReason::Busy {
                step: UpdateStep::FastForward,
                stderr_tail: "fatal: Unable to create '/r/.git/index.lock': File exists.\n\nAnother git process seems to be running in this repository\n".to_owned(),
            }
            .to_string(),
            "repository busy: another git process holds a lock (fast_forward): fatal: Unable to create '/r/.git/index.lock': File exists."
        );
        assert_eq!(
            SkipReason::Dirty { paths: 1 }.to_string(),
            "1 changed tracked path"
        );
        assert_eq!(
            PlannedAction::Skip {
                reason: SkipReason::PolicyOff
            }
            .to_string(),
            "skip: update policy is off"
        );
    }
}
