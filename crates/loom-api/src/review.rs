//! MR review sync and the queue of MRs that wait on me: the bodies and answers of
//! `POST /v1/reviews/sync`, `GET /v1/reviews/queue` and `POST /v1/reviews/queue/{key}/start`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ContextName, ForgeKind, TaskDetail, TaskId, Timestamp};

/// Body of `POST /v1/reviews/sync`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewSyncRequest {
    /// The context to poll. If absent: every context that this machine polls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextName>,
}

/// Answer of `POST /v1/reviews/sync`: one poll per context and forge host that has a
/// credential, sorted by context, then host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewSyncResult {
    /// The polls.
    pub forges: Vec<ReviewSync>,
}

/// The poll of one forge host for one context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewSync {
    /// The context.
    pub context: ContextName,
    /// The forge host.
    pub host: String,
    /// Its forge.
    pub kind: ForgeKind,
    /// The time of the last poll that succeeded, by this loomd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success: Option<Timestamp>,
    /// What the poll did.
    pub outcome: ReviewSyncOutcome,
}

/// What a poll of a forge host did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ReviewSyncOutcome {
    /// The host answered. An MR that failed is in `failed`: the others are synced.
    Synced {
        /// The MRs polled, each one once: the MRs to review, the MRs of the queue and the MRs of
        /// the Review tasks.
        mrs: usize,
        /// The keys of the MRs that this poll added to the queue.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        added: Vec<String>,
        /// The keys of the MRs that this poll removed from the queue.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        removed: Vec<String>,
        /// The Review tasks whose status or mirror changed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changed: Vec<TaskId>,
        /// The MRs to review that belong to another context: not written to this store.
        #[serde(default)]
        other_context: usize,
        /// The MRs whose sync failed: what the poll did not read keeps its last snapshot.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        failed: Vec<ReviewSyncFailure>,
        /// The lists that failed while the poll went on. The reason `mentioned` keeps its last
        /// value. Each MR that the poll read follows that read for the other reasons.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        failed_lists: Vec<ReviewListFailure>,
    },
    /// The host limits the rate of requests until `until`. The poll did not run, or it stopped:
    /// the MRs that it read before the limit are applied.
    RateLimited {
        /// When the next poll can run.
        until: Timestamp,
        /// The lists that failed before the limit.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        failed_lists: Vec<ReviewListFailure>,
    },
    /// The poll stopped: each MR that it did not read keeps its last snapshot.
    Failed {
        /// Why, for humans. Never contains the token.
        error: String,
        /// The lists that failed before the stop.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        failed_lists: Vec<ReviewListFailure>,
    },
}

/// An MR whose sync failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewSyncFailure {
    /// The key of the MR, such as `acme/shop#412`.
    pub key: String,
    /// Why, for humans. Never contains the token.
    pub error: String,
}

/// A list of a poll that can fail without stopping the poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewList {
    /// The MRs assigned to me.
    Assigned,
    /// The MRs that I wrote.
    Own,
    /// My to-dos that mention me.
    Mentioned,
}

/// How a list failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewListFailureKind {
    /// The forge refuses this list to the token: a permission is missing, and a retry does not
    /// help.
    Forbidden,
    /// Another failure.
    Other,
}

/// A list that failed while the poll went on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewListFailure {
    /// The list.
    pub list: ReviewList,
    /// How it failed.
    pub kind: ReviewListFailureKind,
    /// Why, for humans. Never contains the token.
    pub error: String,
}

/// Why an MR waits on me.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueueReason {
    /// I am a requested reviewer, and my verdict is not on the current head.
    ReviewRequested,
    /// I am an assignee, and I am not the author.
    Assigned,
    /// A pending to-do on the forge mentions me.
    Mentioned,
    /// My MR has a note from another person that I did not see.
    OwnNewComments,
    /// A pipeline failed on the current head of my MR.
    OwnPipelineFailed,
    /// My MR can be merged and has an approval.
    OwnReadyToMerge,
}

/// An MR that waits on me and has no Review task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewQueueEntry {
    /// The context of the MR.
    pub context: ContextName,
    /// The key of the MR, such as `acme/shop#412`.
    pub key: String,
    /// The title of the MR.
    pub title: String,
    /// The login of the author.
    pub author: String,
    /// The page of the MR on the forge.
    pub web_url: String,
    /// The head SHA at the last read of the MR.
    pub head_sha: String,
    /// The time of the last change of the MR on the forge.
    pub updated_at: Timestamp,
    /// Why the MR waits on me. Always at least one reason.
    pub reasons: Vec<QueueReason>,
    /// The time when I last opened the entry. If absent: I did not open it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<Timestamp>,
}

/// Answer of `GET /v1/reviews/queue`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewQueue {
    /// The entries, sorted by context, then key.
    pub entries: Vec<ReviewQueueEntry>,
}

/// Body of `POST /v1/reviews/queue/{key}/seen`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewSeenRequest {
    /// The context of the MR. Necessary only when two contexts have this key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextName>,
}

/// Answer of `POST /v1/reviews/queue/{key}/seen`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewSeenResult {
    /// The context of the MR.
    pub context: ContextName,
    /// The key of the MR, such as `company-a/checkout!88`.
    pub key: String,
    /// The time that this request wrote.
    pub seen_at: Timestamp,
    /// The reasons that are left. If empty: the MR left the queue.
    pub reasons: Vec<QueueReason>,
}

/// Body of `POST /v1/reviews/queue/{key}/start`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewStartRequest {
    /// The context of the MR. Necessary only when two contexts have this key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextName>,
}

/// Answer of `POST /v1/reviews/queue/{key}/start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewStartResult {
    /// True when this request created the Review task. False when the task existed before.
    pub created: bool,
    /// The Review task of the MR.
    pub task: TaskDetail,
}
