//! The provider-neutral model of an MR: snapshot, versions, threads, anchors, drafts and
//! verdict (design: `reviews.md`, ADR-0016 decision 2).

use std::fmt;

use loom_api::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use crate::key::{MrKey, MrKeyError};

/// A text that is not a commit SHA.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("`{0}` is not a commit SHA: expected 40 or 64 lowercase hexadecimal digits")]
pub struct ShaError(pub String);

/// The full SHA of a commit: 40 (SHA-1) or 64 (SHA-256) lowercase hexadecimal digits.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct Sha(String);

impl Sha {
    /// The SHA as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Sha {
    type Error = ShaError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let valid = matches!(text.len(), 40 | 64)
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if valid {
            Ok(Self(text))
        } else {
            Err(ShaError(text))
        }
    }
}

impl From<Sha> for String {
    fn from(sha: Sha) -> Self {
        sha.0
    }
}

impl fmt::Display for Sha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The state of an MR on its forge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MrState {
    /// The MR is open.
    Open,
    /// The MR is merged.
    Merged,
    /// The MR is closed and not merged.
    Closed,
}

/// The identifiers that the forge gives to an MR, other than its key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "forge", rename_all = "snake_case")]
pub enum MrForgeIds {
    /// A GitHub pull request.
    Github {
        /// The GraphQL node ID, such as `PR_kwDOAAATic4AA9-c`.
        node_id: String,
        /// The REST ID of the pull request.
        id: u64,
    },
    /// A GitLab merge request.
    Gitlab {
        /// The global ID of the merge request.
        id: u64,
        /// The ID of the target project.
        project_id: u64,
    },
}

/// A snapshot of an MR (`mr.json`, without the description, which is in `description.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Mr {
    /// The key of the MR.
    pub key: MrKey,
    /// The identifiers of the forge.
    pub forge_ids: MrForgeIds,
    /// The title.
    pub title: String,
    /// The description, in Markdown. Empty when the MR has no description.
    pub description: String,
    /// The login of the author.
    pub author: String,
    /// The state.
    pub state: MrState,
    /// True when the MR is a draft.
    pub draft: bool,
    /// The names of the labels.
    pub labels: Vec<String>,
    /// The logins of the users that are requested reviewers. Teams are not in this list.
    pub reviewers: Vec<String>,
    /// The target branch, such as `main`.
    pub base_ref: String,
    /// The source branch.
    pub head_ref: String,
    /// The current version.
    pub version: MrVersion,
    /// The page of the MR on the forge.
    pub web_url: String,
    /// The time when the MR was created.
    pub created_at: Timestamp,
    /// The time of the last change of the MR on the forge.
    pub updated_at: Timestamp,
}

/// An MR in a list of MRs to review: the fields that the forge search gives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MrSummary {
    /// The key of the MR.
    pub key: MrKey,
    /// The title.
    pub title: String,
    /// The login of the author.
    pub author: String,
    /// The state.
    pub state: MrState,
    /// True when the MR is a draft.
    pub draft: bool,
    /// The head SHA, when the forge gives it in the list (GitLab).
    pub head_sha: Option<Sha>,
    /// The page of the MR on the forge.
    pub web_url: String,
    /// The time of the last change of the MR on the forge.
    pub updated_at: Timestamp,
}

/// A version of an MR: one head SHA and the base it is compared with (`versions.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MrVersion {
    /// The head commit.
    pub head_sha: Sha,
    /// The tip of the target branch that the forge used for this version (GitHub `base.sha`,
    /// GitLab `start_sha`).
    pub base_sha: Sha,
    /// The merge base of the head and the target branch, when the forge gives it (GitLab
    /// `base_sha`). For GitHub, loom computes it with git.
    pub merge_base: Option<Sha>,
    /// The ID of the version on the forge (GitLab only).
    pub forge_version_id: Option<u64>,
    /// The time when the forge recorded the version, when it gives it (GitLab only).
    pub created_at: Option<Timestamp>,
}

/// The diff of one file of an MR, as the forge gives it: the patch of the forge, used to check
/// anchors and to show an MR without a local clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileDiff {
    /// The path in the head.
    pub path: String,
    /// The path in the base, when the file is renamed.
    pub old_path: Option<String>,
    /// What changed in the file.
    pub change: FileChange,
    /// The hunks of the patch, from the first `@@` line. `None` when the forge gives no patch:
    /// a binary or large file.
    pub patch: Option<String>,
    /// True when the forge cuts or leaves out the patch because the file is too large.
    pub too_large: bool,
    /// True when the forge marks the file as generated (GitLab only).
    pub generated: bool,
}

/// What changed in a file of an MR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileChange {
    /// A new file.
    Added,
    /// A changed file.
    Modified,
    /// A deleted file.
    Deleted,
    /// A file with a new path, maybe with changes.
    Renamed,
    /// A copy of another file.
    Copied,
    /// A file with a new mode or type and no change of its lines.
    Changed,
}

/// The side of a diff where a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The base: a removed line (GitHub `LEFT`, GitLab `old_line` only).
    Old,
    /// The head: an added or unchanged line (GitHub `RIGHT`, GitLab `new_line`).
    New,
}

/// The lines of a diff that a comment is on: lines `start_line` to `end_line` of one side of
/// `path`, in the version whose head is `sha`. The line numbers are file line numbers, never
/// positions in the diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Anchor {
    /// The path of the file, in the head when `side` is `new`, in the base when it is `old`.
    pub path: String,
    /// The side of the lines.
    pub side: Side,
    /// The first line, from 1.
    pub start_line: u32,
    /// The last line, from 1. Equal to `start_line` for one line.
    pub end_line: u32,
    /// The head SHA of the version that the line numbers apply to.
    pub sha: Sha,
}

/// What a thread is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThreadSubject {
    /// The MR as a whole: a general comment.
    General,
    /// A file, not a line.
    File {
        /// The path of the file in the head.
        path: String,
        /// The head SHA of the version.
        sha: Sha,
    },
    /// Lines of a file.
    Lines {
        /// The lines.
        anchor: Anchor,
    },
}

/// A comment of a thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Comment {
    /// The ID of the comment on the forge (GitHub node ID, GitLab note ID).
    pub id: String,
    /// The login of the author.
    pub author: String,
    /// The body, in Markdown.
    pub body: String,
    /// The time when the comment was written.
    pub created_at: Timestamp,
}

/// A thread of an MR, or a general comment (`threads.json`).
///
/// The subject of a line thread is the position of the thread in the newest version that it
/// applies to: the current version when the thread is not outdated, otherwise the version it
/// was written on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Thread {
    /// The ID of the thread on the forge (GitHub review thread or comment node ID, GitLab
    /// discussion ID).
    pub id: String,
    /// What the thread is on.
    pub subject: ThreadSubject,
    /// True when the lines of the thread are not in the current diff.
    pub outdated: bool,
    /// True when the thread is resolved. Always false for a general comment.
    pub resolved: bool,
    /// The comments, oldest first. Never empty.
    pub comments: Vec<Comment>,
}

/// A comment on lines that I write locally, published with the review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DraftComment {
    /// The lines of the comment.
    pub anchor: Anchor,
    /// The body, in Markdown, with an optional Conventional Comments label.
    pub body: String,
    /// The replacement text of the anchored lines, published as a suggestion. Only on the
    /// `new` side.
    pub suggestion: Option<String>,
}

/// A reply that I write locally to a thread of the forge, published with the review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DraftReply {
    /// The ID of the thread on the forge.
    pub thread_id: String,
    /// The body, in Markdown.
    pub body: String,
}

/// A thread that I resolve when the review is published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DraftResolve {
    /// The ID of the thread on the forge.
    pub thread_id: String,
}

/// The decision of a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    /// Approve the MR (GitHub `APPROVE`, GitLab `approve`).
    Approve,
    /// Ask for changes (GitHub `REQUEST_CHANGES`, GitLab reviewer state `requested_changes`).
    RequestChanges,
    /// Comment without a decision (GitHub `COMMENT`, GitLab reviewer state `reviewed`).
    Comment,
}

/// The verdict of a review, published last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Verdict {
    /// The decision.
    pub kind: VerdictKind,
    /// The summary of the review, in Markdown.
    pub body: Option<String>,
}

/// A user of a forge, as `GET /user` gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ForgeUser {
    /// The ID of the user on the forge.
    pub id: u64,
    /// The login (GitHub) or username (GitLab).
    pub login: String,
}

#[cfg(test)]
mod tests {
    use super::{Sha, ShaError};

    #[test]
    fn a_sha_is_40_or_64_lowercase_hex_digits() {
        for text in ["7ebf283bfee25ab48f95c0c55181a4ca66b4c990", &"a".repeat(64)] {
            assert_eq!(Sha::try_from(text.to_owned()).unwrap().as_str(), text);
        }
        for text in [
            "",
            "7ebf283",
            "7EBF283BFEE25AB48F95C0C55181A4CA66B4C990",
            "7ebf283bfee25ab48f95c0c55181a4ca66b4c99g",
            &"a".repeat(41),
        ] {
            assert_eq!(
                Sha::try_from(text.to_owned()),
                Err(ShaError(text.to_owned()))
            );
        }
    }
}
