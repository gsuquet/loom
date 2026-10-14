//! Backup of the context stores: `GET /v1/tasks/backup`, `POST /v1/tasks/sync` and
//! `loom sync --json`.

use std::fmt;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ContextName, Timestamp};

/// Body of `POST /v1/tasks/sync`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BackupRequest {
    /// The context to sync; absent: every context held here with a backup remote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextName>,
}

/// The backup state of the context stores of this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BackupReport {
    /// This machine: the `id` of `machine.toml`.
    pub machine: String,
    /// Each store held here, sorted by context; after a sync, only the ones synced.
    pub contexts: Vec<ContextBackup>,
    /// Contexts whose `machines` does not name this machine: not loaded, never synced here.
    pub not_held: Vec<ContextName>,
}

/// The backup state of one context store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextBackup {
    /// The context.
    pub context: ContextName,
    /// Root of its store.
    pub path: PathBuf,
    /// `store.backup.remote`, a password in it replaced with `***`; `null` when the store is
    /// not pushed anywhere.
    pub remote: Option<String>,
    /// Why the last commit of the store failed; `null` when every change is committed.
    pub uncommitted: Option<String>,
    /// The last sync since loomd started; `null` before the first one or without a remote.
    pub last_sync: Option<BackupSync>,
}

impl ContextBackup {
    /// The last sync stopped before the store and its remote were the same, or a change is not
    /// committed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.uncommitted.is_some()
            || self
                .last_sync
                .as_ref()
                .is_some_and(|sync| sync.outcome.failed())
    }
}

/// One sync of a store with its backup remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BackupSync {
    /// When it ended.
    pub time: Timestamp,
    /// What happened.
    pub outcome: BackupOutcome,
}

/// What a sync did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackupOutcome {
    /// The store and its remote hold the same commits.
    UpToDate,
    /// Local commits were pushed.
    Pushed {
        /// Commits pushed.
        commits: u64,
    },
    /// The store was fast-forwarded to the remote.
    Pulled {
        /// Commits pulled.
        commits: u64,
    },
    /// The store and its remote both have commits the other lacks: nothing was merged, pushed
    /// or pulled. Fixed by hand.
    Diverged {
        /// Commits only in the store.
        local: u64,
        /// Commits only on the remote.
        remote: u64,
    },
    /// The remote refused the push: the commits stay local.
    PushRejected {
        /// The end of git's standard error, at most 2,000 bytes.
        stderr_tail: String,
    },
    /// A step failed or timed out.
    Failed {
        /// The step.
        step: BackupStep,
        /// What went wrong.
        error: String,
        /// The end of git's standard error, at most 2,000 bytes.
        stderr_tail: String,
    },
}

impl BackupOutcome {
    /// Whether the store and its remote may still differ.
    #[must_use]
    pub const fn failed(&self) -> bool {
        match self {
            Self::UpToDate | Self::Pushed { .. } | Self::Pulled { .. } => false,
            Self::Diverged { .. } | Self::PushRejected { .. } | Self::Failed { .. } => true,
        }
    }
}

impl fmt::Display for BackupOutcome {
    /// One line: `up to date`, `pushed 2 commits`, `diverged: 3 local, 1 remote commits`…
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let commits = |count: u64| if count == 1 { "commit" } else { "commits" };
        match self {
            Self::UpToDate => f.write_str("up to date"),
            Self::Pushed { commits: count } => write!(f, "pushed {count} {}", commits(*count)),
            Self::Pulled { commits: count } => write!(f, "pulled {count} {}", commits(*count)),
            Self::Diverged { local, remote } => write!(
                f,
                "diverged: {local} local and {remote} remote {} (nothing merged)",
                commits(*remote)
            ),
            Self::PushRejected { stderr_tail } => {
                write!(f, "push rejected, commits kept local")?;
                last_line(f, stderr_tail)
            }
            Self::Failed {
                step,
                error,
                stderr_tail,
            } => {
                write!(f, "{step} failed: {error}")?;
                last_line(f, stderr_tail)
            }
        }
    }
}

fn last_line(f: &mut fmt::Formatter<'_>, stderr: &str) -> fmt::Result {
    stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map_or(Ok(()), |line| write!(f, ": {}", line.trim()))
}

/// A step of a sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackupStep {
    /// Committing the changes not committed yet, or creating the repository.
    Commit,
    /// Setting the `backup` remote of the repository.
    Remote,
    /// `git fetch`.
    Fetch,
    /// Comparing the store with the remote.
    Compare,
    /// `git push`.
    Push,
    /// `git merge --ff-only`.
    FastForward,
}

impl fmt::Display for BackupStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Commit => "commit",
            Self::Remote => "remote",
            Self::Fetch => "fetch",
            Self::Compare => "compare",
            Self::Push => "push",
            Self::FastForward => "fast-forward",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{BackupOutcome, BackupStep};

    #[test]
    fn outcomes_are_tagged_by_kind() {
        assert_eq!(
            serde_json::to_value(BackupOutcome::Diverged {
                local: 2,
                remote: 1
            })
            .unwrap(),
            serde_json::json!({ "kind": "diverged", "local": 2, "remote": 1 })
        );
        assert_eq!(
            serde_json::to_value(BackupStep::FastForward).unwrap(),
            serde_json::json!("fast_forward")
        );
    }

    #[test]
    fn outcomes_print_on_one_line() {
        assert_eq!(
            BackupOutcome::Pushed { commits: 1 }.to_string(),
            "pushed 1 commit"
        );
        assert_eq!(
            BackupOutcome::Pulled { commits: 3 }.to_string(),
            "pulled 3 commits"
        );
        assert_eq!(
            BackupOutcome::Diverged {
                local: 3,
                remote: 1
            }
            .to_string(),
            "diverged: 3 local and 1 remote commit (nothing merged)"
        );
        assert_eq!(
            BackupOutcome::PushRejected {
                stderr_tail: "remote: no\n ! [remote rejected] main -> main (hook declined)\n"
                    .to_owned()
            }
            .to_string(),
            "push rejected, commits kept local: ! [remote rejected] main -> main (hook declined)"
        );
        assert!(!BackupOutcome::UpToDate.failed());
        assert!(
            BackupOutcome::Failed {
                step: BackupStep::Fetch,
                error: "exit status: 128".to_owned(),
                stderr_tail: String::new()
            }
            .failed()
        );
    }
}
