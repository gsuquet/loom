//! The state of a repository (its card): `HEAD`, changes, operation in progress, upstream.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Timestamp;

/// A state field that may not have been computed: the time budget ran out, or it could not
/// be read (the reason is in [`RepoState::error`]).
///
/// Serialized as `{"known": <value>}` or `"unknown"`, so that a known `null` (no upstream, no
/// operation…) is never confused with an unknown value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(rename = "MaybeKnown_{T}")]
pub enum MaybeKnown<T> {
    /// The value.
    Known(T),
    /// Not computed.
    Unknown,
}

impl<T> MaybeKnown<T> {
    /// The value, `None` when unknown.
    #[must_use]
    pub fn known(self) -> Option<T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown => None,
        }
    }

    /// The value mapped with `f`, unknown when unknown.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> MaybeKnown<U> {
        match self {
            Self::Known(value) => MaybeKnown::Known(f(value)),
            Self::Unknown => MaybeKnown::Unknown,
        }
    }

    /// A reference to the value, `None` when unknown.
    #[must_use]
    pub const fn as_known(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown => None,
        }
    }
}

impl<T> From<Option<T>> for MaybeKnown<T> {
    /// `None` is [`MaybeKnown::Unknown`].
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Unknown, Self::Known)
    }
}

/// What `HEAD` points to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RepoHead {
    /// A branch with at least one commit.
    Branch {
        /// Short branch name, such as `main` or `feat/x`.
        name: String,
    },
    /// A branch without any commit yet (new repository, orphan branch).
    Unborn {
        /// Short branch name.
        name: String,
    },
    /// A commit, outside any branch.
    Detached {
        /// Abbreviated commit id (`core.abbrev`, at least 7 hexadecimal digits).
        commit: String,
    },
}

impl RepoHead {
    /// The branch name, `None` when detached.
    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        match self {
            Self::Branch { name } | Self::Unborn { name } => Some(name),
            Self::Detached { .. } => None,
        }
    }
}

/// A git operation left in progress in the working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RepoOperation {
    /// `git rebase`, including an interactive one.
    Rebase,
    /// `git merge`.
    Merge,
    /// `git cherry-pick`.
    CherryPick,
    /// `git revert`.
    Revert,
    /// `git bisect`.
    Bisect,
    /// `git am`: patches being applied from a mailbox.
    Am,
}

impl RepoOperation {
    /// Name, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rebase => "rebase",
            Self::Merge => "merge",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
            Self::Bisect => "bisect",
            Self::Am => "am",
        }
    }
}

impl fmt::Display for RepoOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Untracked files of the working tree, `.gitignore` applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Untracked {
    /// Number of untracked files (an untracked nested repository counts as one), at most the
    /// cap.
    pub count: u64,
    /// The count stopped at the cap: there are more.
    pub capped: bool,
}

/// The upstream of the branch `HEAD` is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Upstream {
    /// Short name of the upstream ref, such as `origin/main`.
    pub name: String,
    /// Commits of `HEAD` that are not in the upstream.
    pub ahead: MaybeKnown<u64>,
    /// Commits of the upstream that are not in `HEAD`.
    pub behind: MaybeKnown<u64>,
}

/// The state of a repository, read without starting git, at [`RepoState::refreshed_at`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepoState {
    /// When the state was read.
    pub refreshed_at: Timestamp,
    /// What `HEAD` points to.
    pub head: MaybeKnown<RepoHead>,
    /// Tracked paths with a change: modified, staged, deleted, renamed or conflicted
    /// (untracked files are counted in `untracked`).
    pub dirty: MaybeKnown<u64>,
    /// Untracked files.
    pub untracked: MaybeKnown<Untracked>,
    /// Operation in progress, `null` when none.
    pub operation: MaybeKnown<Option<RepoOperation>>,
    /// Upstream of the `HEAD` branch, `null` when detached, unconfigured, or when its ref is
    /// missing (never fetched, or deleted on the remote and pruned).
    pub upstream: MaybeKnown<Option<Upstream>>,
    /// Default branch: the target of `refs/remotes/<primary remote>/HEAD`, otherwise `main`,
    /// otherwise `master` when the branch exists; `null` when none of them.
    pub default_branch: MaybeKnown<Option<String>>,
    /// Committer date of `HEAD`'s commit, `null` for an unborn branch.
    pub last_commit: MaybeKnown<Option<Timestamp>>,
    /// Modification time of `FETCH_HEAD`, `null` when the repository was never fetched.
    pub last_fetch: MaybeKnown<Option<Timestamp>>,
    /// Why some fields are unknown, in English; `null` when every field is known.
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{MaybeKnown, RepoHead, RepoOperation, Untracked};
    use serde_json::json;

    #[test]
    fn maybe_known_keeps_a_known_null_apart_from_unknown() {
        let none: MaybeKnown<Option<String>> = MaybeKnown::Known(None);
        assert_eq!(
            serde_json::to_value(&none).unwrap(),
            json!({ "known": null })
        );
        let unknown: MaybeKnown<Option<String>> = MaybeKnown::Unknown;
        assert_eq!(serde_json::to_value(&unknown).unwrap(), json!("unknown"));
        assert_eq!(
            serde_json::from_value::<MaybeKnown<Option<String>>>(json!({ "known": "unknown" }))
                .unwrap(),
            MaybeKnown::Known(Some("unknown".to_owned()))
        );
        assert_eq!(MaybeKnown::from(Some(3)), MaybeKnown::Known(3));
        assert_eq!(MaybeKnown::<u8>::from(None), MaybeKnown::Unknown);
        assert_eq!(MaybeKnown::Known(3).known(), Some(3));
        assert_eq!(MaybeKnown::Known(3).map(|n| n + 1), MaybeKnown::Known(4));
        assert_eq!(
            MaybeKnown::<u8>::Unknown.map(|n| n + 1),
            MaybeKnown::Unknown
        );
        assert_eq!(MaybeKnown::<u8>::Unknown.as_known(), None);
    }

    #[test]
    fn head_and_operation_serialization() {
        let head = RepoHead::Unborn {
            name: "main".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&head).unwrap(),
            json!({ "kind": "unborn", "name": "main" })
        );
        assert_eq!(head.branch(), Some("main"));
        let detached = RepoHead::Detached {
            commit: "abc1234".to_owned(),
        };
        assert_eq!(detached.branch(), None);
        for operation in [
            RepoOperation::Rebase,
            RepoOperation::Merge,
            RepoOperation::CherryPick,
            RepoOperation::Revert,
            RepoOperation::Bisect,
            RepoOperation::Am,
        ] {
            assert_eq!(
                serde_json::to_value(operation).unwrap(),
                json!(operation.to_string())
            );
        }
        assert_eq!(
            serde_json::to_value(Untracked {
                count: 2,
                capped: false
            })
            .unwrap(),
            json!({ "count": 2, "capped": false })
        );
    }
}
