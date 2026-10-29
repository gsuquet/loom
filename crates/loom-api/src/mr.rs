//! `loom pr`: the request that opens or links the MR of a branch, and its result.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ExternalRef, ForgeKind, TaskId};

/// Body of `POST /v1/tasks/{prefix}/mr`: the branch that `loom pr` pushed, in the project of
/// its remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MrOpenRequest {
    /// The forge host of the remote, such as `github.com`.
    pub host: String,
    /// The project path on the host, such as `acme/shop`.
    pub project: String,
    /// The source branch.
    pub head: String,
    /// The target branch.
    pub base: String,
    /// When true, loomd sends nothing to the forge and changes no task: the result gives the
    /// requests it would send.
    #[serde(default)]
    pub dry_run: bool,
}

/// What loomd did for a [`MrOpenRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MrOutcome {
    /// It opened a draft MR and linked it to the task.
    Opened,
    /// It found an open MR of the branch and linked it to the task.
    Linked,
    /// Dry run: it sent nothing and changed nothing.
    Planned,
}

/// A request to the forge API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ForgeRequestPlan {
    /// The HTTP method.
    pub method: String,
    /// The URL.
    pub url: String,
    /// The JSON body. Absent for a `GET`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

/// Result of `POST /v1/tasks/{prefix}/mr`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MrOpenResult {
    /// The task.
    pub task: TaskId,
    /// The forge of the host.
    pub forge: ForgeKind,
    /// What loomd did.
    pub outcome: MrOutcome,
    /// The MR, now a `delivery` link of the task. Absent for a dry run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mr: Option<ExternalRef>,
    /// The requests to the forge: sent, or planned for a dry run. A dry run lists the creation
    /// request, which loomd sends only when the branch has no open MR.
    pub requests: Vec<ForgeRequestPlan>,
}

/// The `git push` of `loom pr`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PushPlan {
    /// The remote.
    pub remote: String,
    /// The branch.
    pub branch: String,
    /// The command, one argument per item.
    pub command: Vec<String>,
    /// True when the push ran and succeeded; false for a dry run.
    pub pushed: bool,
}

/// Output of `loom pr --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PrReport {
    /// True for `loom pr -n`: nothing was pushed or sent.
    pub dry_run: bool,
    /// The push.
    pub push: PushPlan,
    /// The MR.
    pub result: MrOpenResult,
}
