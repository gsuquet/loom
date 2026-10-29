//! MR review sync: the body and the answer of `POST /v1/reviews/sync` (ADR-0016).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ContextName, ForgeKind, TaskId, Timestamp};

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
        /// The MRs polled: the MRs to review and the MRs of the Review tasks.
        mrs: usize,
        /// The Review tasks created.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        created: Vec<TaskId>,
        /// The Review tasks whose status or mirror changed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changed: Vec<TaskId>,
        /// The MRs to review that belong to another context: not written to this store.
        #[serde(default)]
        other_context: usize,
        /// The MRs whose sync failed: their last snapshot is kept.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        failed: Vec<ReviewSyncFailure>,
    },
    /// The host was not polled: it limits the rate of requests until `until`.
    RateLimited {
        /// When the next poll can run.
        until: Timestamp,
    },
    /// The poll failed: every MR of the host keeps its last snapshot.
    Failed {
        /// Why, for humans. Never contains the token.
        error: String,
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
