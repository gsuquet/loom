//! `[contexts.<ctx>.connectors.<name>]`: the connector instances of a context, and their rules.
//!
//! A connector belongs to a context, and its secrets too, so there is no table above the context
//! as there is for `[forges]`.
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

/// A key that not all kinds have: its name, its place when the table gives it, and its kinds.
type KindKey = (&'static str, Option<Range<usize>>, &'static [TableKind]);

impl RawConnector {
    /// Each key that not all kinds have, with its place when the table gives it, and its kinds.
    fn kind_keys(&self) -> [KindKey; 17] {
        const DATADOG: &[TableKind] = &[TableKind::Datadog];
        const JIRA: &[TableKind] = &[TableKind::Jira];
        const FRESHSERVICE: &[TableKind] = &[TableKind::Freshservice];
        const POOL: &[TableKind] = &[TableKind::Jira, TableKind::Freshservice];
        [
            ("app_key_cmd", place(self.app_key_cmd.as_ref()), DATADOG),
            ("app_key_env", place(self.app_key_env.as_ref()), DATADOG),
            ("query", place(self.query.as_ref()), DATADOG),
            ("states", place(self.states.as_ref()), DATADOG),
            ("priorities", place(self.priorities.as_ref()), DATADOG),
            (
                "urgent_priorities",
                place(self.urgent_priorities.as_ref()),
                DATADOG,
            ),
            ("flap_window", place(self.flap_window.as_ref()), DATADOG),
            ("email", place(self.email.as_ref()), JIRA),
            ("list_jql", place(self.list_jql.as_ref()), JIRA),
            ("pool_jql", place(self.pool_jql.as_ref()), JIRA),
            ("pool_interval", place(self.pool_interval.as_ref()), POOL),
            ("pool_limit", place(self.pool_limit.as_ref()), POOL),
            ("agent_id", place(self.agent_id.as_ref()), FRESHSERVICE),
            ("group_ids", place(self.group_ids.as_ref()), FRESHSERVICE),
            (
                "workspace_id",
                place(self.workspace_id.as_ref()),
                FRESHSERVICE,
            ),
            (
                "open_statuses",
                place(self.open_statuses.as_ref()),
                FRESHSERVICE,
            ),
            (
                "extra_query",
                place(self.extra_query.as_ref()),
                FRESHSERVICE,
            ),
        ]
    }

    /// The instance of this table, at `table` in the file.
    fn build(self, table: &Range<usize>) -> Result<Connector, Invalid> {
        let kind = *self.kind.get_ref();
        for (key, at, kinds) in self.kind_keys() {
            if let Some(span) = at
                && !kinds.contains(&kind)
            {
                return Err(Invalid {
                    span,
                    message: format!("`{key}` is not a key of a `{}` connector", kind.name()),
                });
            }
        }
        let site = site(kind, self.site)?;
        let token = secret_source(
            ("token_cmd", "token_env", "token"),
            self.token_cmd,
            self.token_env,
        )
        .and_then(|token| token.ok_or_else(|| "expected token_cmd or token_env".to_owned()))
        .map_err(|message| Invalid {
            span: table.clone(),
            message,
        })?;
        let default_interval = match kind {
            TableKind::Datadog => DEFAULT_DATADOG_POLL_INTERVAL,
            TableKind::Jira | TableKind::Freshservice => DEFAULT_TRACKER_POLL_INTERVAL,
        };
        let poll_interval = self
            .poll_interval
            .unwrap_or_else(|| PollInterval::new(default_interval));
        let settings = match kind {
            TableKind::Datadog => ConnectorSettings::Datadog(DatadogSettings {
                app_key: secret_source(
                    ("app_key_cmd", "app_key_env", "application key"),
                    self.app_key_cmd.map(Spanned::into_inner),
                    self.app_key_env.map(Spanned::into_inner),
                )
                .and_then(|key| key.ok_or_else(|| "expected app_key_cmd or app_key_env".to_owned()))
                .map_err(|message| Invalid {
                    span: table.clone(),
                    message,
                })?,
                query: required(kind, "query", self.query, table)
                    .and_then(|query| not_blank("query", query))?,
                states: match self.states {
                    None => BTreeSet::from(DEFAULT_STATES),
                    Some(states) => not_empty("states", states)?
                        .into_inner()
                        .into_iter()
                        .collect(),
                },
                priorities: self
                    .priorities
                    .map(|list| {
                        not_empty("priorities", list)
                            .and_then(|list| priorities("priorities", list))
                    })
                    .transpose()?,
                urgent_priorities: match self.urgent_priorities {
                    None => BTreeSet::from(DEFAULT_URGENT_PRIORITIES),
                    Some(list) => priorities("urgent_priorities", list)?,
                },
                flap_window: match &self.flap_window {
                    None => DEFAULT_FLAP_WINDOW,
                    Some(text) => interval("flap_window", text)?.get(),
                },
            }),
            TableKind::Jira => ConnectorSettings::Jira(JiraSettings {
                email: required(kind, "email", self.email, table).and_then(email)?,
                list_jql: match self.list_jql {
                    None => DEFAULT_LIST_JQL.to_owned(),
                    Some(jql) => not_blank("list_jql", jql)?,
                },
                pool_jql: self
                    .pool_jql
                    .map(|jql| not_blank("pool_jql", jql))
                    .transpose()?,
                pool: pool(self.pool_interval.as_ref(), self.pool_limit)?,
            }),
            TableKind::Freshservice => ConnectorSettings::Freshservice(FreshserviceSettings {
                agent_id: required(kind, "agent_id", self.agent_id, table).and_then(agent_id)?,
                group_ids: self.group_ids.map(group_ids).transpose()?,
                workspace_id: self.workspace_id.map(Spanned::into_inner),
                open_statuses: match self.open_statuses {
                    None => BTreeSet::from(DEFAULT_OPEN_STATUSES),
                    Some(list) => open_statuses(list)?,
                },
                extra_query: self
                    .extra_query
                    .map(|query| not_blank("extra_query", query))
                    .transpose()?,
                pool: pool(self.pool_interval.as_ref(), self.pool_limit)?,
            }),
        };
        Ok(Connector {
            site,
            token,
            poll_interval,
            settings,
        })
    }
}

fn place<T>(value: Option<&Spanned<T>>) -> Option<Range<usize>> {
    value.map(Spanned::span)
}

/// Whether `host` is a bare lowercase host name: labels of 1 to 63 ASCII letters, digits and `-`,
/// without a `-` at either end, split by `.`. So no scheme, user, port or path can be in it.
fn is_bare_host(host: &str) -> bool {
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

/// `site`, by the rule of `kind`. It decides where the secrets go.
fn site(kind: TableKind, site: Spanned<String>) -> Result<ConnectorSite, Invalid> {
    let span = site.span();
    let site = site.into_inner();
    let refuse = |why: String| Invalid {
        span: span.clone(),
        message: format!("site `{site}`: {why}"),
    };
    if !is_bare_host(&site) {
        return Err(refuse(
            "expected a lowercase host name without scheme, user, port or path".to_owned(),
        ));
    }
    match kind {
        TableKind::Datadog if !DATADOG_SITES.contains(&site.as_str()) => Err(refuse(format!(
            "not a Datadog site: expected one of {}",
            DATADOG_SITES.join(", ")
        ))),
        TableKind::Jira if !site.ends_with(JIRA_SITE_SUFFIX) => Err(refuse(format!(
            "a `jira` connector needs a host that ends with `{JIRA_SITE_SUFFIX}`"
        ))),
        TableKind::Freshservice if !site.ends_with(FRESHSERVICE_SITE_SUFFIX) => {
            Err(refuse(format!(
                "a `freshservice` connector needs a host that ends with `{FRESHSERVICE_SITE_SUFFIX}`"
            )))
        }
        TableKind::Datadog | TableKind::Jira | TableKind::Freshservice => Ok(ConnectorSite(site)),
    }
}

fn required<T>(
    kind: TableKind,
    key: &str,
    value: Option<Spanned<T>>,
    table: &Range<usize>,
) -> Result<Spanned<T>, Invalid> {
    value.ok_or_else(|| Invalid {
        span: table.clone(),
        message: format!("a `{}` connector needs `{key}`", kind.name()),
    })
}

fn not_blank(key: &str, value: Spanned<String>) -> Result<String, Invalid> {
    if value.get_ref().trim().is_empty() {
        return Err(Invalid {
            span: value.span(),
            message: format!("{key} is blank"),
        });
    }
    Ok(value.into_inner())
}

fn not_empty<T>(key: &str, list: Spanned<Vec<T>>) -> Result<Spanned<Vec<T>>, Invalid> {
    if list.get_ref().is_empty() {
        return Err(Invalid {
            span: list.span(),
            message: format!("{key} is empty"),
        });
    }
    Ok(list)
}

fn interval(key: &str, text: &Spanned<String>) -> Result<PollInterval, Invalid> {
    PollInterval::parse(key, text.get_ref()).map_err(|message| Invalid {
        span: text.span(),
        message,
    })
}

/// Datadog's priorities go from 1 (high) to 5 (low).
fn priorities(key: &str, list: Spanned<Vec<u8>>) -> Result<BTreeSet<u8>, Invalid> {
    if let Some(wrong) = list.get_ref().iter().find(|p| !(1..=5).contains(*p)) {
        return Err(Invalid {
            span: list.span(),
            message: format!("{key}: {wrong} is not from 1 to 5"),
        });
    }
    Ok(list.into_inner().into_iter().collect())
}

/// One address. Basic auth splits `email:token` on the first `:`, so a `:` would change the user.
fn email(value: Spanned<String>) -> Result<String, Invalid> {
    let valid = value.get_ref().split_once('@').is_some_and(|(user, host)| {
        !user.is_empty()
            && !host.is_empty()
            && !host.contains('@')
            && !value
                .get_ref()
                .chars()
                .any(|c| c == ':' || c.is_whitespace() || c.is_control())
    });
    if !valid {
        return Err(Invalid {
            span: value.span(),
            message: format!(
                "invalid email `{}`: expected one address, without a space or `:`",
                value.get_ref()
            ),
        });
    }
    Ok(value.into_inner())
}

/// An adapter compares it to the assignee: one that matches nobody would answer `NotMine` to
/// every ticket.
fn agent_id(value: Spanned<u64>) -> Result<u64, Invalid> {
    if *value.get_ref() == 0 {
        return Err(Invalid {
            span: value.span(),
            message: "agent_id is 0: expected the ID of my agent".to_owned(),
        });
    }
    Ok(value.into_inner())
}

fn group_ids(list: Spanned<Vec<u64>>) -> Result<Vec<u64>, Invalid> {
    let list = not_empty("group_ids", list)?;
    if list.get_ref().contains(&0) {
        return Err(Invalid {
            span: list.span(),
            message: "group_ids: 0 is not an ID".to_owned(),
        });
    }
    Ok(list.into_inner())
}

/// A terminal status in the list would make an item both open and closed in one poll.
fn open_statuses(list: Spanned<Vec<u64>>) -> Result<BTreeSet<u64>, Invalid> {
    let list = not_empty("open_statuses", list)?;
    let span = list.span();
    let statuses = list.into_inner();
    let wrong = |message: String| Invalid {
        span: span.clone(),
        message,
    };
    if statuses.contains(&0) {
        return Err(wrong("open_statuses: 0 is not a status".to_owned()));
    }
    if let Some(terminal) = statuses
        .iter()
        .find(|status| TERMINAL_STATUSES.contains(*status))
    {
        return Err(wrong(format!("open_statuses: {terminal} is terminal")));
    }
    Ok(statuses.into_iter().collect())
}

fn pool(
    pool_interval: Option<&Spanned<String>>,
    pool_limit: Option<Spanned<u32>>,
) -> Result<PoolSettings, Invalid> {
    let limit = pool_limit.map_or(Ok(DEFAULT_POOL_LIMIT), |limit| {
        if *limit.get_ref() == 0 {
            return Err(Invalid {
                span: limit.span(),
                message: "pool_limit is 0: expected at least 1".to_owned(),
            });
        }
        Ok(limit.into_inner())
    })?;
    Ok(PoolSettings {
        interval: match pool_interval {
            None => PollInterval::new(DEFAULT_POOL_INTERVAL),
            Some(text) => interval("pool_interval", text)?,
        },
        limit,
    })
}

/// What an instance on a shared site says is its own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SiteFilter<'a> {
    /// The default: it selects the same items in every context.
    Default(&'static str),
    Jql(&'a str),
    Workspace(Option<u64>, Option<&'a str>),
}

impl Connector {
    /// `None` for Datadog, whose site says nothing of the organization.
    fn site_filter(&self) -> Option<SiteFilter<'_>> {
        match &self.settings {
            ConnectorSettings::Datadog(_) => None,
            ConnectorSettings::Jira(settings) => Some(if settings.list_jql == DEFAULT_LIST_JQL {
                SiteFilter::Default("write list_jql")
            } else {
                SiteFilter::Jql(&settings.list_jql)
            }),
            ConnectorSettings::Freshservice(settings) => {
                Some(match (settings.workspace_id, &settings.extra_query) {
                    (None, None) => SiteFilter::Default("write workspace_id or extra_query"),
                    (workspace, query) => SiteFilter::Workspace(workspace, query.as_deref()),
                })
            }
        }
    }
}

/// One instance on a shared site.
struct Seat<'a> {
    context: &'a ContextName,
    name: &'a ConnectorName,
    span: Range<usize>,
    filter: SiteFilter<'a>,
}

/// Builds the instances from the tables of `config`, then checks the rules across tables. The
/// tables are empty afterwards.
pub(crate) fn resolve(config: &mut Config, text: &str) -> Result<(), Invalid> {
    for context in config.contexts.values_mut() {
        for (name, table) in std::mem::take(&mut context.connector_tables) {
            let span = table.span();
            let connector = table.into_inner().build(&span)?;
            context
                .connectors
                .insert(name, Spanned::new(span, connector));
        }
    }
    validate(config, text)
}

/// The rules across tables. Pure.
fn validate(config: &Config, text: &str) -> Result<(), Invalid> {
    let mut sites: BTreeMap<&str, Vec<Seat<'_>>> = BTreeMap::new();
    for (context_name, context) in &config.contexts {
        for (name, connector) in &context.connectors {
            check_forge_host(config, context_name, name, connector)?;
            check_backup(context_name, name, context, connector)?;
            let Some(filter) = connector.get_ref().site_filter() else {
                continue;
            };
            let seats = sites.entry(connector.get_ref().site.as_str()).or_default();
            if let Some(first) = seats.iter().find(|seat| seat.context == context_name) {
                return Err(Invalid {
                    span: connector.span(),
                    message: format!(
                        "connector `{name}` of context `{context_name}`: `{}` of the same context has the same site (line {}): widen the filter of one instance",
                        first.name,
                        location(text, first.span.start).line
                    ),
                });
            }
            seats.push(Seat {
                context: context_name,
                name,
                span: connector.span(),
                filter,
            });
        }
    }
    for (site, seats) in &sites {
        check_shared_site(site, seats, text)?;
    }
    Ok(())
}

/// Status keys on context and name, and a forge credential shows under its host.
fn check_forge_host(
    config: &Config,
    context: &ContextName,
    name: &ConnectorName,
    connector: &Spanned<Connector>,
) -> Result<(), Invalid> {
    let Ok(host) = ForgeHost::try_from(name.as_str().to_owned()) else {
        return Ok(());
    };
    if config.forge_credential(context, &host).is_some() {
        return Err(Invalid {
            span: connector.span(),
            message: format!(
                "connector name `{name}` is a forge host of context `{context}`: status could not tell them apart, choose another name"
            ),
        });
    }
    Ok(())
}

/// The whole store goes to the remote in plain text.
fn check_backup(
    context_name: &ContextName,
    name: &ConnectorName,
    context: &ContextConfig,
    connector: &Spanned<Connector>,
) -> Result<(), Invalid> {
    match &context.store.backup {
        Some(backup) if !backup.allows_client_data => Err(Invalid {
            span: connector.span(),
            message: format!(
                "connector `{name}` of context `{context_name}`: the store with its client data is pushed in plain text to store.backup.remote: check that the client allows this remote, then set store.backup.allows_client_data = true"
            ),
        }),
        Some(_) | None => Ok(()),
    }
}

/// A token sees the same items in two contexts of one site, so each must say what is its own.
fn check_shared_site(site: &str, seats: &[Seat<'_>], text: &str) -> Result<(), Invalid> {
    if seats.len() < 2 {
        return Ok(());
    }
    for (index, seat) in seats.iter().enumerate() {
        let shared = |other: &Seat<'_>, why: &str| Invalid {
            span: seat.span.clone(),
            message: format!(
                "connector `{}` of context `{}`: site `{site}` is also used by connector `{}` of context `{}` (line {}): {why}",
                seat.name,
                seat.context,
                other.name,
                other.context,
                location(text, other.span.start).line,
            ),
        };
        if let SiteFilter::Default(how) = seat.filter {
            let other = &seats[usize::from(index == 0)];
            return Err(shared(
                other,
                &format!("{how}, so that each context says what is its own"),
            ));
        }
        if let Some(first) = seats[..index]
            .iter()
            .find(|first| first.filter == seat.filter)
        {
            return Err(shared(
                first,
                "they have the same filter, so each item would land in both",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
