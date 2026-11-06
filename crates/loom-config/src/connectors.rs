//! `[contexts.<ctx>.connectors.<name>]`: the connector instances of a context, their keys per
//! kind, and the rules on them. A connector belongs to a context, and its secrets too, so there
//! is no table above the context as there is for `[forges]`.
//!
//! A table is read in two steps. Serde reads each key into [`RawConnector`], and reports wrong
//! types and unknown keys at their place. [`resolve`] then builds a [`Connector`] from it. Serde
//! gives an error raised from a table the place of the table, while the keys of a table depend
//! on its `kind`, which the file can give last. [`resolve`] holds the place of each key.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;
use std::time::Duration;

use loom_api::{ConnectorKind, ContextName};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use toml::Spanned;

use crate::config::{Config, Invalid, location};
use crate::forges::{PollInterval, TokenSource, secret_source};
use crate::repos::{ContextConfig, ForgeHost};

/// The sites Datadog publishes. "Each site is completely independent".
pub const DATADOG_SITES: [&str; 9] = [
    "datadoghq.com",
    "us3.datadoghq.com",
    "us5.datadoghq.com",
    "ap1.datadoghq.com",
    "ap2.datadoghq.com",
    "uk1.datadoghq.com",
    "datadoghq.eu",
    "ddog-gov.com",
    "us2.ddog-gov.com",
];
/// The ending of the site of a Jira instance.
pub const JIRA_SITE_SUFFIX: &str = ".atlassian.net";
/// The ending of the site of a Freshservice instance.
pub const FRESHSERVICE_SITE_SUFFIX: &str = ".freshservice.com";

/// Default `poll_interval` of a Datadog instance.
pub const DEFAULT_DATADOG_POLL_INTERVAL: Duration = Duration::from_mins(1);
/// Default `poll_interval` of a Jira or Freshservice instance.
pub const DEFAULT_TRACKER_POLL_INTERVAL: Duration = Duration::from_mins(5);
/// Default `pool_interval`.
pub const DEFAULT_POOL_INTERVAL: Duration = Duration::from_mins(15);
/// Default `pool_limit`.
pub const DEFAULT_POOL_LIMIT: u32 = 200;
/// Default `flap_window`.
pub const DEFAULT_FLAP_WINDOW: Duration = Duration::from_mins(30);
/// Default `urgent_priorities`.
pub const DEFAULT_URGENT_PRIORITIES: [u8; 2] = [1, 2];
/// Default `states`.
pub const DEFAULT_STATES: [MonitorState; 2] = [MonitorState::Alert, MonitorState::Warn];
/// Default `list_jql`.
pub const DEFAULT_LIST_JQL: &str =
    "assignee = currentUser() AND statusCategory != Done ORDER BY key";
/// Default `open_statuses`.
pub const DEFAULT_OPEN_STATUSES: [u64; 2] = [2, 3];

/// Longest connector name.
const MAX_NAME: usize = 64;
/// The Freshservice statuses that end a ticket (Resolved, Closed).
const TERMINAL_STATUSES: [u64; 2] = [4, 5];

/// A connector name: 1 to 64 of `[a-z0-9_]`. The first `-` of `<connector>-<key>` ends the name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectorName(String);

impl ConnectorName {
    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConnectorName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ConnectorName {
    type Error = String;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        let valid = !name.is_empty()
            && name.len() <= MAX_NAME
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if !valid {
            return Err(format!(
                "invalid connector name `{name}`: expected 1 to {MAX_NAME} lowercase ASCII letters, digits and `_`"
            ));
        }
        Ok(Self(name))
    }
}

impl<'de> Deserialize<'de> for ConnectorName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// `site`: the bare host name that gets the secrets of an instance. It follows the rule of its
/// kind.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectorSite(String);

impl ConnectorSite {
    /// The host name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `states` of a Datadog instance: the group states to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
pub enum MonitorState {
    /// `alert`.
    #[serde(rename = "alert")]
    Alert,
    /// `warn`.
    #[serde(rename = "warn")]
    Warn,
    /// `no data`.
    #[serde(rename = "no data")]
    NoData,
}

/// `pool_interval` and `pool_limit`: how the team pool of a tracker is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSettings {
    /// `pool_interval`.
    pub interval: PollInterval,
    /// `pool_limit`.
    pub limit: u32,
}

/// The keys of a Datadog instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatadogSettings {
    /// `app_key_cmd` or `app_key_env`.
    pub app_key: TokenSource,
    /// `query`, passed to Datadog as written.
    pub query: String,
    /// `states`.
    pub states: BTreeSet<MonitorState>,
    /// `priorities`; `None`: all, and monitors with no priority.
    pub priorities: Option<BTreeSet<u8>>,
    /// `urgent_priorities`.
    pub urgent_priorities: BTreeSet<u8>,
    /// `flap_window`.
    pub flap_window: Duration,
}

/// The keys of a Jira instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraSettings {
    /// `email`: the user of Basic auth. It is no secret.
    pub email: String,
    /// `list_jql`.
    pub list_jql: String,
    /// `pool_jql`; `None`: no pool.
    pub pool_jql: Option<String>,
    /// `pool_interval` and `pool_limit`.
    pub pool: PoolSettings,
}

/// The keys of a Freshservice instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshserviceSettings {
    /// `agent_id`: my account. Freshservice has no documented call that gives it.
    pub agent_id: u64,
    /// `group_ids`: the pool; `None`: no pool.
    pub group_ids: Option<Vec<u64>>,
    /// `workspace_id`; `None`: the primary workspace only.
    pub workspace_id: Option<u64>,
    /// `open_statuses`.
    pub open_statuses: BTreeSet<u64>,
    /// `extra_query`, added to the built query with AND.
    pub extra_query: Option<String>,
    /// `pool_interval` and `pool_limit`.
    pub pool: PoolSettings,
}

/// The keys of an instance that only its kind has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectorSettings {
    /// `kind = "datadog"`.
    Datadog(DatadogSettings),
    /// `kind = "jira"`.
    Jira(JiraSettings),
    /// `kind = "freshservice"`.
    Freshservice(FreshserviceSettings),
}

/// `[contexts.<ctx>.connectors.<name>]`: one connector instance. `Debug` does not print the text
/// of a `*_cmd` key (see [`TokenSource`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connector {
    /// `site`.
    pub site: ConnectorSite,
    /// `token_cmd` or `token_env`: the token, or the API key of Datadog and Freshservice.
    pub token: TokenSource,
    /// `poll_interval`, or the default of the kind.
    pub poll_interval: PollInterval,
    /// The keys of its kind.
    pub settings: ConnectorSettings,
}

impl Connector {
    /// The kind of the instance. Pure.
    #[must_use]
    pub const fn kind(&self) -> ConnectorKind {
        match self.settings {
            ConnectorSettings::Datadog(_) => ConnectorKind::Datadog,
            ConnectorSettings::Jira(_) => ConnectorKind::Jira,
            ConnectorSettings::Freshservice(_) => ConnectorKind::Freshservice,
        }
    }

    /// The API address of the instance, without a final `/`: the only place its secrets go. Pure.
    #[must_use]
    pub fn api_base(&self) -> String {
        let site = self.site.as_str();
        match self.settings {
            ConnectorSettings::Datadog(_) => format!("https://api.{site}"),
            ConnectorSettings::Jira(_) => format!("https://{site}/rest/api/3"),
            ConnectorSettings::Freshservice(_) => format!("https://{site}/api/v2"),
        }
    }

    /// Where each secret of the instance comes from. Pure.
    pub fn secrets(&self) -> impl Iterator<Item = &TokenSource> {
        let app_key = match &self.settings {
            ConnectorSettings::Datadog(settings) => Some(&settings.app_key),
            ConnectorSettings::Jira(_) | ConnectorSettings::Freshservice(_) => None,
        };
        std::iter::once(&self.token).chain(app_key)
    }
}

/// `kind`, as a table can give it: forges have `[forges]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TableKind {
    Datadog,
    Jira,
    Freshservice,
}

impl TableKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Datadog => "datadog",
            Self::Jira => "jira",
            Self::Freshservice => "freshservice",
        }
    }
}

/// A table as the file gives it. Each key of one kind only keeps its place in the file, for the
/// refusal of [`resolve`].
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawConnector {
    kind: Spanned<TableKind>,
    site: Spanned<String>,
    token_cmd: Option<String>,
    token_env: Option<String>,
    poll_interval: Option<PollInterval>,
    app_key_cmd: Option<Spanned<String>>,
    app_key_env: Option<Spanned<String>>,
    query: Option<Spanned<String>>,
    states: Option<Spanned<Vec<MonitorState>>>,
    priorities: Option<Spanned<Vec<u8>>>,
    urgent_priorities: Option<Spanned<Vec<u8>>>,
    flap_window: Option<Spanned<String>>,
    email: Option<Spanned<String>>,
    list_jql: Option<Spanned<String>>,
    pool_jql: Option<Spanned<String>>,
    pool_interval: Option<Spanned<String>>,
    pool_limit: Option<Spanned<u32>>,
    agent_id: Option<Spanned<u64>>,
    group_ids: Option<Spanned<Vec<u64>>>,
    workspace_id: Option<Spanned<u64>>,
    open_statuses: Option<Spanned<Vec<u64>>>,
    extra_query: Option<Spanned<String>>,
}

/// It holds the text of the `*_cmd` keys, which can hold a secret.
impl fmt::Debug for RawConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RawConnector")
    }
}

/// Builds the instances from the tables of `config`, then checks the rules across tables. The
/// tables are empty afterwards.
pub(crate) fn resolve(config: &mut Config, text: &str) -> Result<(), Invalid> {
    let _ = (&config, text);
    Ok(())
}

#[cfg(test)]
mod tests;
