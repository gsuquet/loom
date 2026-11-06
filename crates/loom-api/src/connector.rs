//! Connector status: the answer of `GET /v1/connectors` and of `loom connector status --json`.
//! Each connector instance and each forge credential reports this shape.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ContextName, ForgeKind, Timestamp};

/// The kinds of connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorKind {
    /// A GitLab forge.
    Gitlab,
    /// A GitHub forge.
    Github,
    /// Datadog.
    Datadog,
    /// Jira Cloud.
    Jira,
    /// Freshservice.
    Freshservice,
}

impl ConnectorKind {
    /// What this kind can do. The value is constant for each kind.
    #[must_use]
    pub const fn capabilities(self) -> Capabilities {
        match self {
            // The MR mirror of a forge is not a tracker mirror.
            Self::Gitlab | Self::Github | Self::Datadog => Capabilities {
                pool: false,
                mirror: false,
            },
            Self::Jira | Self::Freshservice => Capabilities {
                pool: true,
                mirror: true,
            },
        }
    }
}

impl From<ForgeKind> for ConnectorKind {
    fn from(kind: ForgeKind) -> Self {
        match kind {
            ForgeKind::Github => Self::Github,
            ForgeKind::Gitlab => Self::Gitlab,
        }
    }
}

/// What a kind of connector can do. `loom` and the app use this to know what to offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Capabilities {
    /// The connector has a team pool.
    pub pool: bool,
    /// The connector has a mirror of each issue.
    pub mirror: bool,
}

/// A secret of a connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Secret {
    /// The token or the API key.
    Token,
    /// The application key of Datadog.
    AppKey,
}

/// The machine that polls a connector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum PolledBy {
    /// This machine: it is the first machine of the context.
    ThisMachine,
    /// Another machine. This machine does not know the health of the connector.
    Other {
        /// The first machine of the context.
        machine: String,
    },
    /// Each machine that holds the context: the context has no `machines` key.
    EveryMachine,
}

/// An item or a list that failed in a poll that went on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConnectorFailure {
    /// The key of the item, or `list:<name>` for a list.
    pub key: String,
    /// Why, for humans. Never contains a secret.
    pub reason: String,
}

/// The result of the last poll of a connector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectorHealth {
    /// No poll ran since loomd started, or another machine polls this connector.
    NotPolledYet,
    /// The last poll reported no failure.
    Ok,
    /// Some items or lists failed. The others are applied.
    Partial {
        /// What failed.
        failed: Vec<ConnectorFailure>,
    },
    /// The host limits the rate of requests.
    RateLimited {
        /// When the next poll can run.
        until: Timestamp,
    },
    /// The host refuses a secret, or the secret does not resolve.
    Credential {
        /// The secret.
        which: Secret,
        /// Why, for humans. Never contains the token that loomd has. When `token_cmd` fails,
        /// contains the end of the standard error of the command.
        error: String,
    },
    /// The secrets are good, but a permission is missing.
    Forbidden {
        /// Why, for humans. Never contains a secret.
        error: String,
        /// What else failed, when the poll went on.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        failed: Vec<ConnectorFailure>,
    },
    /// The host refuses the filter of the configuration. A retry does not help.
    InvalidFilter {
        /// Why, for humans.
        error: String,
    },
    /// The poll stopped for another reason.
    Unreachable {
        /// Why, for humans. Never contains a secret.
        error: String,
        /// The failed polls in a row.
        failures: u32,
    },
}

/// The counts of the last poll that was not stopped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConnectorCounts {
    /// The items of the last poll. For a forge: the updates and the failures of the poll. This
    /// is not the number of MRs that wait on me.
    pub listed: usize,
    /// The items of the team pool.
    pub pool: usize,
    /// The items and lists that failed.
    pub failed: usize,
    /// The pool has more items than `pool_limit`.
    pub truncated: bool,
    /// The mirrors with local changes.
    pub local_changes: usize,
}

/// The status of one connector instance or forge credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConnectorStatus {
    /// The context.
    pub context: ContextName,
    /// The name of the connector. For a forge: its host.
    pub name: String,
    /// Its kind.
    pub kind: ConnectorKind,
    /// The host of the credential. The API of a forge can be on another host.
    pub host: String,
    /// The machine that polls it.
    pub polled_by: PolledBy,
    /// The result of the last poll.
    pub health: ConnectorHealth,
    /// No poll succeeded in the last 3 poll intervals, or no poll ever succeeded. Always false
    /// when another machine polls the connector.
    pub stale: bool,
    /// The start of the last poll on this machine that called the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt: Option<Timestamp>,
    /// The start of the last poll on this machine that succeeded: the host answered its list and
    /// the poll went on, also when each read of an item failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success: Option<Timestamp>,
    /// The time of the next poll.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_poll: Option<Timestamp>,
    /// The counts of the last poll that was not stopped.
    pub items: ConnectorCounts,
    /// What its kind can do.
    pub capabilities: Capabilities,
}

/// Answer of `GET /v1/connectors`: sorted by context, then name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConnectorList {
    /// The connectors.
    pub connectors: Vec<ConnectorStatus>,
}
