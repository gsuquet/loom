//! `LOOM_HOME/config.toml`: parsing and error locations.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use loom_api::{ContextName, UpdateChannel, UpdatePolicyMode};
use serde::{Deserialize, Deserializer};
pub use toml::Spanned;

use crate::duration::{format_duration, parse_duration};
use crate::machine::MachineName;
use crate::repos::{ContextConfig, ForgeConfig, ForgeHost, ReposConfig, reserved_message};

/// Content of `config.toml`. Unknown keys and tables are rejected at every level.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `[contexts.<name>]` tables: how repositories are matched to contexts.
    #[serde(default, deserialize_with = "crate::repos::contexts")]
    pub contexts: BTreeMap<ContextName, ContextConfig>,
    /// `[daemon]` table.
    #[serde(default)]
    pub daemon: Daemon,
    /// `default_context`: context of the repositories no rule matches. Names a defined context.
    #[serde(default)]
    pub default_context: Option<Spanned<ContextName>>,
    /// `[forges."<host>"]` tables: forge of self-hosted hosts.
    #[serde(default)]
    pub forges: BTreeMap<ForgeHost, ForgeConfig>,
    /// `[repos]` table: discovery, fetch schedule and per-repository settings.
    #[serde(default)]
    pub repos: ReposConfig,
    /// `[tasks]` table: the context stores.
    #[serde(default)]
    pub tasks: TasksConfig,
    /// `[telemetry]` table: export of traces for diagnosing loom.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// `[update]` table.
    #[serde(default)]
    pub update: Update,
}

impl Config {
    /// The root of each context store: `store.path` with `~` expanded to `user_home`, or
    /// `<loom_home>/contexts/<name>` when it is not set.
    #[must_use]
    pub fn store_paths(
        &self,
        loom_home: &Path,
        user_home: &Path,
    ) -> BTreeMap<ContextName, PathBuf> {
        self.contexts
            .iter()
            .map(|(name, context)| {
                let path = context.store.path.as_ref().map_or_else(
                    || loom_home.join("contexts").join(name.as_str()),
                    |path| path.get_ref().expand(user_home),
                );
                (name.clone(), path)
            })
            .collect()
    }
}

/// The context stores a machine holds, and the contexts it does not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stores {
    /// Each context the machine holds, with its store.
    pub held: BTreeMap<ContextName, StoreSettings>,
    /// Contexts whose `machines` does not name the machine, sorted.
    pub not_held: Vec<ContextName>,
}

/// Where a context store is and where it is backed up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreSettings {
    /// Root of the store (see [`Config::store_paths`]).
    pub path: PathBuf,
    /// `store.backup.remote`, `None` when the store is not pushed anywhere.
    pub remote: Option<String>,
}

impl Config {
    /// The stores of the contexts `machine` holds (`machines` absent or naming it), and the
    /// contexts it does not hold. Paths as [`Config::store_paths`] gives them.
    #[must_use]
    pub fn stores(&self, loom_home: &Path, user_home: &Path, machine: &MachineName) -> Stores {
        let mut paths = self.store_paths(loom_home, user_home);
        let mut stores = Stores::default();
        for (name, context) in &self.contexts {
            if !context.held_by(machine) {
                stores.not_held.push(name.clone());
                continue;
            }
            if let Some(path) = paths.remove(name) {
                let remote = context
                    .store
                    .backup
                    .as_ref()
                    .map(|backup| backup.remote.get_ref().as_str().to_owned());
                stores
                    .held
                    .insert(name.clone(), StoreSettings { path, remote });
            }
        }
        stores
    }
}

/// Shortest `[tasks] sync_interval` accepted.
pub const MIN_SYNC_INTERVAL: Duration = Duration::from_mins(1);
/// Default `[tasks] sync_interval`.
pub const DEFAULT_SYNC_INTERVAL: Duration = Duration::from_mins(15);

/// Shortest `[tasks] archive_after` accepted.
pub const MIN_ARCHIVE_AFTER: Duration = Duration::from_hours(24);
/// Default `[tasks] archive_after`.
pub const DEFAULT_ARCHIVE_AFTER: Duration = Duration::from_hours(7 * 24);

/// `[tasks]` table: the context stores.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TasksConfig {
    /// `sync_interval`: time between two pushes and pulls of each store with a backup remote.
    pub sync_interval: SyncInterval,
    /// `archive_after`: time after which a done or cancelled task moves to the archive.
    pub archive_after: ArchiveAfter,
}

/// `[tasks] archive_after`: a duration of at least [`MIN_ARCHIVE_AFTER`], written as text
/// (`"7d"`). Default [`DEFAULT_ARCHIVE_AFTER`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveAfter(Duration);

impl ArchiveAfter {
    /// The duration.
    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }
}

impl Default for ArchiveAfter {
    fn default() -> Self {
        Self(DEFAULT_ARCHIVE_AFTER)
    }
}

impl<'de> Deserialize<'de> for ArchiveAfter {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let text = String::deserialize(deserializer)?;
        let duration = parse_duration(&text)
            .map_err(|error| D::Error::custom(format!("archive_after: {error}")))?;
        if duration < MIN_ARCHIVE_AFTER {
            return Err(D::Error::custom(format!(
                "archive_after `{text}` is shorter than the minimum {}",
                format_duration(MIN_ARCHIVE_AFTER)
            )));
        }
        Ok(Self(duration))
    }
}

/// `[tasks] sync_interval`: a duration of at least [`MIN_SYNC_INTERVAL`], written as text
/// (`"15m"`). Default [`DEFAULT_SYNC_INTERVAL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncInterval(Duration);

impl SyncInterval {
    /// The duration.
    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }
}

impl Default for SyncInterval {
    fn default() -> Self {
        Self(DEFAULT_SYNC_INTERVAL)
    }
}

impl<'de> Deserialize<'de> for SyncInterval {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let text = String::deserialize(deserializer)?;
        let duration = parse_duration(&text)
            .map_err(|error| D::Error::custom(format!("sync_interval: {error}")))?;
        if duration < MIN_SYNC_INTERVAL {
            return Err(D::Error::custom(format!(
                "sync_interval `{text}` is shorter than the minimum {}",
                format_duration(MIN_SYNC_INTERVAL)
            )));
        }
        Ok(Self(duration))
    }
}

/// `[daemon]` table: how `loomd` runs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Daemon {
    /// `service`: whether `loom service install` may install `loomd` as a user service.
    /// Default `true`.
    pub service: bool,
}

impl Default for Daemon {
    fn default() -> Self {
        Self { service: true }
    }
}

/// `[telemetry]` table: OTLP export of traces. Off by default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    /// Whether traces are exported. `OTEL_SDK_DISABLED=true` turns it off again.
    #[serde(default)]
    pub enabled: bool,
    /// OTLP/HTTP traces endpoint; overrides the `OTEL_EXPORTER_OTLP_*_ENDPOINT` variables.
    #[serde(default)]
    pub otlp_endpoint: Option<OtlpEndpoint>,
}

/// An `http` or `https` URL with a host, such as `http://localhost:4318/v1/traces`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct OtlpEndpoint(String);

impl OtlpEndpoint {
    /// The URL, as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for OtlpEndpoint {
    type Error = String;

    fn try_from(url: String) -> Result<Self, Self::Error> {
        let invalid = || {
            format!(
                "invalid otlp_endpoint `{url}`: expected an http:// or https:// URL with a host"
            )
        };
        let (scheme, rest) = url.split_once("://").ok_or_else(invalid)?;
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let valid_scheme =
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
        if !valid_scheme || host.is_empty() || url.chars().any(char::is_whitespace) {
            return Err(invalid());
        }
        Ok(Self(url))
    }
}

/// Shortest `[update] check_interval` accepted.
pub const MIN_CHECK_INTERVAL: Duration = Duration::from_mins(10);
/// Default `[update] check_interval`.
pub const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_hours(6);

/// `[update]` table: auto-upgrade of `loomd` (ADR-0011).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Update {
    /// `policy`: `auto`, `notify` or `off`. `None`: the default of the build channel.
    pub policy: Option<UpdatePolicyMode>,
    /// `channel`: `stable` or `dev`. `None`: the channel chosen with `loom upgrade --channel`,
    /// otherwise the build channel.
    pub channel: Option<UpdateChannel>,
    /// `check_interval`: time between two checks, before jitter.
    pub check_interval: CheckInterval,
}

/// `[update] check_interval`: a duration of at least [`MIN_CHECK_INTERVAL`], written as text
/// (`"6h"`). Default [`DEFAULT_CHECK_INTERVAL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckInterval(Duration);

impl CheckInterval {
    /// The duration.
    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }
}

impl Default for CheckInterval {
    fn default() -> Self {
        Self(DEFAULT_CHECK_INTERVAL)
    }
}

impl<'de> Deserialize<'de> for CheckInterval {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let text = String::deserialize(deserializer)?;
        let duration = parse_duration(&text).map_err(D::Error::custom)?;
        if duration < MIN_CHECK_INTERVAL {
            return Err(D::Error::custom(format!(
                "check_interval `{text}` is shorter than the minimum {}",
                format_duration(MIN_CHECK_INTERVAL)
            )));
        }
        Ok(Self(duration))
    }
}

/// Position in a text file, both 1-based. `column` counts characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    /// Line number.
    pub line: usize,
    /// Column number, in characters.
    pub column: usize,
}

/// An invalid configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// Path of the file.
    pub path: PathBuf,
    /// Where the error is, when the parser reports it.
    pub location: Option<Location>,
    /// What is wrong, such as ``unknown field `foo` ``.
    pub message: String,
}

/// `invalid configuration <path>:<line>:<column>: <message>`.
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid configuration {}", self.path.display())?;
        if let Some(Location { line, column }) = self.location {
            write!(f, ":{line}:{column}")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for ConfigError {}

/// Parses the content of the configuration file at `path`.
///
/// # Errors
///
/// [`ConfigError`] on a TOML syntax error or an unknown key, with its location.
pub fn parse_config(text: &str, path: &Path) -> Result<Config, ConfigError> {
    let config: Config = toml::from_str(text).map_err(|error| ConfigError {
        path: path.to_path_buf(),
        location: error.span().map(|span| location(text, span.start)),
        message: error.message().trim_end().to_owned(),
    })?;
    validate(&config, text).map_err(|Invalid { span, message }| ConfigError {
        path: path.to_path_buf(),
        location: Some(location(text, span.start)),
        message,
    })?;
    Ok(config)
}

/// A rule across keys that the configuration breaks, at `span`.
struct Invalid {
    span: Range<usize>,
    message: String,
}

/// Checks across keys: contexts named by `default_context` and `[repos."<path>"] context`
/// exist, no `match.paths`, `match.hosts`, `store.path` or `store.backup.remote` entry is in
/// two contexts, no repository path is given twice, no schedule ID twice in a context.
fn validate(config: &Config, text: &str) -> Result<(), Invalid> {
    let check_defined = |name: &Spanned<ContextName>, key: &str| {
        if name.get_ref().is_unassigned() {
            return Err(Invalid {
                span: name.span(),
                message: format!("{key}: {}", reserved_message()),
            });
        }
        if !config.contexts.contains_key(name.get_ref()) {
            return Err(Invalid {
                span: name.span(),
                message: format!(
                    "{key} `{}` is not a defined context: add a [contexts.{}] table",
                    name.get_ref(),
                    name.get_ref()
                ),
            });
        }
        Ok(())
    };
    if let Some(name) = &config.default_context {
        check_defined(name, "default_context")?;
    }
    let mut repo_paths = BTreeMap::new();
    for repo in &config.repos.overrides {
        if let Some(name) = &repo.context {
            check_defined(name, "context")?;
        }
        let normalized = repo.path.get_ref().normalized();
        if let Some(first) = repo_paths.insert(normalized, &repo.path) {
            return Err(Invalid {
                span: repo.path.span(),
                message: format!(
                    "[repos.\"{}\"] is the same repository as [repos.\"{}\"]",
                    repo.path.get_ref(),
                    first.get_ref()
                ),
            });
        }
    }

    // The same entry in two contexts would tie: neither is longer. Host paths ignore ASCII case,
    // as their matching does. Two contexts cannot share a store.
    let mut paths = BTreeMap::new();
    let mut hosts = BTreeMap::new();
    let mut stores = BTreeMap::new();
    let mut remotes = BTreeMap::new();
    for (name, context) in &config.contexts {
        // The ID is the dedup key of the schedule's tasks in the context.
        let mut ids = BTreeMap::new();
        for schedule in &context.schedules {
            if let Some(first) = ids.insert(schedule.id.get_ref(), &schedule.id) {
                let Location { line, column } = location(text, first.span().start);
                return Err(Invalid {
                    span: schedule.id.span(),
                    message: format!(
                        "schedule id `{}` of context `{name}` is already used (line {line}, column {column})",
                        schedule.id.get_ref()
                    ),
                });
            }
        }
        // Two stores pushed to one repository would diverge at once.
        if let Some(backup) = &context.store.backup
            && let Some((other, first)) =
                remotes.insert(backup.remote.get_ref().as_str(), (name, &backup.remote))
        {
            return Err(tie(
                text,
                "store.backup.remote",
                &backup.remote,
                name,
                first,
                other,
            ));
        }
        if let Some(path) = &context.store.path
            && let Some((other, first)) = stores.insert(path.get_ref().normalized(), (name, path))
        {
            return Err(tie(text, "store.path", path, name, first, other));
        }
        for path in &context.matching.paths {
            if let Some((other, first)) = paths.insert(path.get_ref().normalized(), (name, path))
                && other != name
            {
                return Err(tie(text, "match.paths", path, name, first, other));
            }
        }
        for host in &context.matching.hosts {
            let key = host.get_ref().to_string().to_ascii_lowercase();
            if let Some((other, first)) = hosts.insert(key, (name, host))
                && other != name
            {
                return Err(tie(text, "match.hosts", host, name, first, other));
            }
        }
    }
    Ok(())
}

/// `entry` of context `name` is also `first` of context `other`.
fn tie<T: fmt::Display>(
    text: &str,
    key: &str,
    entry: &Spanned<T>,
    name: &ContextName,
    first: &Spanned<T>,
    other: &ContextName,
) -> Invalid {
    let Location { line, column } = location(text, first.span().start);
    Invalid {
        span: entry.span(),
        message: format!(
            "{key} `{}` of context `{name}` is also `{}` of context `{other}` (line {line}, column {column}): an entry can belong to one context only",
            entry.get_ref(),
            first.get_ref(),
        ),
    }
}

/// Line and column of the byte `offset` in `text`. An offset inside a character or past the
/// end is clamped to the previous character boundary.
pub(crate) fn location(text: &str, offset: usize) -> Location {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    Location {
        line: before.matches('\n').count() + 1,
        column: before[line_start..].chars().count() + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CheckInterval, Config, DEFAULT_ARCHIVE_AFTER, DEFAULT_CHECK_INTERVAL,
        DEFAULT_SYNC_INTERVAL, Daemon, Location, OtlpEndpoint, Spanned, StoreSettings,
        TelemetryConfig, Update, location, parse_config,
    };
    use crate::repos::{
        ConfigPath, ContextConfig, DEFAULT_FETCH_CONCURRENCY, DEFAULT_FETCH_INTERVAL,
        DEFAULT_MAX_DEPTH, DEFAULT_ROOTS, ForgeHost, ReposConfig,
    };
    use crate::schedules::DueRule;
    use loom_api::{ContextName, ForgeKind, RepoUpdatePolicy, UpdateChannel, UpdatePolicyMode};
    use loom_core::{Importance, Kind, Policy};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const PATH: &str = "/home/me/.loom/config.toml";

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(parse_config("", Path::new(PATH)), Ok(Config::default()));
        assert_eq!(
            parse_config("# comment only\n\n", Path::new(PATH)),
            Ok(Config::default())
        );
    }

    #[test]
    fn unknown_key_names_file_line_column_and_key() {
        let error = parse_config("# loom\n\nfoo = 1\n", Path::new(PATH)).unwrap_err();
        assert_eq!(error.location, Some(Location { line: 3, column: 1 }));
        assert!(error.message.contains("foo"), "{}", error.message);
        let text = error.to_string();
        assert!(
            text.starts_with(&format!("invalid configuration {PATH}:3:1: ")),
            "{text}"
        );
        assert!(text.contains("unknown field `foo`"), "{text}");
    }

    #[test]
    fn unknown_table_is_rejected() {
        let error = parse_config("[server]\nport = 1\n", Path::new(PATH)).unwrap_err();
        assert_eq!(error.location.map(|at| at.line), Some(1));
        assert!(error.message.contains("server"), "{}", error.message);
    }

    #[test]
    fn daemon_service_defaults_to_true() {
        assert!(Config::default().daemon.service);
        let empty_table = parse_config("[daemon]\n", Path::new(PATH)).unwrap();
        assert_eq!(empty_table.daemon, Daemon { service: true });
    }

    #[test]
    fn daemon_service_can_be_disabled() {
        for (text, expected) in [
            ("[daemon]\nservice = false\n", false),
            ("[daemon]\nservice = true\n", true),
            ("daemon.service = false\n", false),
        ] {
            let config = parse_config(text, Path::new(PATH)).unwrap();
            assert_eq!(config.daemon.service, expected, "{text:?}");
        }
    }

    #[test]
    fn unknown_daemon_key_is_rejected() {
        let error = parse_config("[daemon]\nport = 1\n", Path::new(PATH)).unwrap_err();
        assert_eq!(error.location, Some(Location { line: 2, column: 1 }));
        assert!(
            error.message.contains("unknown field `port`"),
            "{}",
            error.message
        );
    }

    #[test]
    fn daemon_service_must_be_a_boolean() {
        let error = parse_config("[daemon]\nservice = \"no\"\n", Path::new(PATH)).unwrap_err();
        assert_eq!(error.location.map(|at| at.line), Some(2));
    }

    #[test]
    fn update_defaults() {
        let update = Config::default().update;
        assert_eq!(update.policy, None);
        assert_eq!(update.channel, None);
        assert_eq!(update.check_interval.get(), DEFAULT_CHECK_INTERVAL);
        assert_eq!(
            parse_config("[update]\n", Path::new(PATH)).unwrap().update,
            Update::default()
        );
    }

    #[test]
    fn update_keys_parse() {
        let text = "[update]\npolicy = \"notify\"\nchannel = \"dev\"\ncheck_interval = \"1h30m\"\n";
        let update = parse_config(text, Path::new(PATH)).unwrap().update;
        assert_eq!(
            update,
            Update {
                policy: Some(UpdatePolicyMode::Notify),
                channel: Some(UpdateChannel::Dev),
                check_interval: CheckInterval(Duration::from_mins(90)),
            }
        );
        for (value, mode) in [
            ("auto", UpdatePolicyMode::Auto),
            ("off", UpdatePolicyMode::Off),
        ] {
            let text = format!("update.policy = \"{value}\"\n");
            let config = parse_config(&text, Path::new(PATH)).unwrap();
            assert_eq!(config.update.policy, Some(mode));
        }
    }

    #[test]
    fn check_interval_minimum_is_ten_minutes() {
        let at_minimum = parse_config("[update]\ncheck_interval = \"10m\"\n", Path::new(PATH));
        assert_eq!(
            at_minimum.unwrap().update.check_interval.get(),
            Duration::from_secs(600)
        );
        let error =
            parse_config("[update]\ncheck_interval = \"9m59s\"\n", Path::new(PATH)).unwrap_err();
        assert_eq!(error.location.map(|at| at.line), Some(2));
        assert!(
            error.message.contains("shorter than the minimum 10m"),
            "{}",
            error.message
        );
    }

    #[test]
    fn invalid_update_values_are_rejected_with_a_location() {
        for (text, needle) in [
            (
                "[update]\ncheck_interval = \"6 hours\"\n",
                "invalid duration",
            ),
            ("[update]\ncheck_interval = 6\n", "string"),
            ("[update]\npolicy = \"sometimes\"\n", "sometimes"),
            ("[update]\nchannel = \"local\"\n", "local"),
            ("[update]\nfoo = 1\n", "unknown field `foo`"),
        ] {
            let error = parse_config(text, Path::new(PATH)).unwrap_err();
            assert_eq!(error.location.map(|at| at.line), Some(2), "{text}");
            assert!(error.message.contains(needle), "{text}: {}", error.message);
        }
    }

    #[test]
    fn syntax_error_has_a_location() {
        let error = parse_config("\nkey = \n", Path::new(PATH)).unwrap_err();
        assert_eq!(error.location.map(|at| at.line), Some(2));
        assert!(
            error
                .to_string()
                .starts_with(&format!("invalid configuration {PATH}:2:")),
            "{error}"
        );
    }

    #[test]
    fn telemetry_is_off_by_default() {
        let config = parse_config("", Path::new(PATH)).unwrap();
        assert_eq!(config.telemetry, TelemetryConfig::default());
        assert!(!config.telemetry.enabled);
        assert_eq!(config.telemetry.otlp_endpoint, None);
        let config = parse_config("[telemetry]\n", Path::new(PATH)).unwrap();
        assert_eq!(config.telemetry, TelemetryConfig::default());
    }

    #[test]
    fn telemetry_keys_parse() {
        let text = "[telemetry]\nenabled = true\notlp_endpoint = \"https://otel.example:4318/v1/traces\"\n";
        let config = parse_config(text, Path::new(PATH)).unwrap();
        assert!(config.telemetry.enabled);
        assert_eq!(
            config
                .telemetry
                .otlp_endpoint
                .as_ref()
                .map(OtlpEndpoint::as_str),
            Some("https://otel.example:4318/v1/traces")
        );
    }

    #[test]
    fn otlp_endpoint_must_be_an_http_url_with_a_host() {
        for url in [
            "http://localhost:4318/v1/traces",
            "https://otel.example",
            "HTTP://localhost:4318",
            "http://[::1]:4318/v1/traces",
        ] {
            assert!(OtlpEndpoint::try_from(url.to_owned()).is_ok(), "{url}");
        }
        for url in [
            "",
            "localhost:4318",
            "grpc://localhost:4317",
            "file:///tmp/traces",
            "http://",
            "http:///v1/traces",
            "http://local host:4318",
        ] {
            assert!(OtlpEndpoint::try_from(url.to_owned()).is_err(), "{url}");
        }
    }

    #[test]
    fn invalid_otlp_endpoint_names_file_line_column_and_value() {
        let text = "[telemetry]\nenabled = true\notlp_endpoint = \"localhost:4318\"\n";
        let error = parse_config(text, Path::new(PATH)).unwrap_err();
        assert_eq!(error.location.map(|at| at.line), Some(3));
        let message = error.to_string();
        assert!(
            message.starts_with(&format!("invalid configuration {PATH}:3:")),
            "{message}"
        );
        assert!(message.contains("`localhost:4318`"), "{message}");
        assert!(message.contains("http:// or https://"), "{message}");
    }

    #[test]
    fn unknown_telemetry_key_is_rejected() {
        let error =
            parse_config("[telemetry]\nendpoint = \"http://x\"\n", Path::new(PATH)).unwrap_err();
        assert_eq!(error.location.map(|at| at.line), Some(2));
        assert!(
            error.message.contains("unknown field `endpoint`"),
            "{}",
            error.message
        );
    }

    #[test]
    fn enabled_must_be_a_boolean() {
        let error = parse_config("[telemetry]\nenabled = \"yes\"\n", Path::new(PATH)).unwrap_err();
        assert_eq!(error.location.map(|at| at.line), Some(2));
    }

    /// Parses `text`, expecting an error at `line`:`column` whose message contains `needle`.
    fn assert_rejected(text: &str, line: usize, column: usize, needle: &str) {
        let error = parse_config(text, Path::new(PATH)).unwrap_err();
        assert_eq!(
            error.location,
            Some(Location { line, column }),
            "{text}\n{error}"
        );
        assert!(error.message.contains(needle), "{text}\n{error}");
        assert!(
            error
                .to_string()
                .starts_with(&format!("invalid configuration {PATH}:{line}:{column}: ")),
            "{error}"
        );
    }

    const REPOS_EXAMPLE: &str = r#"default_context = "personal"

[contexts.personal]
match.paths = ["~/perso"]
match.hosts = ["github.com/gsuquet"]

[contexts.company-a]
match.paths = ["~/work/company-a", "/srv/company-a"]
match.hosts = ["gitlab.com/company-a", "gitlab.company-a.example"]
update = "fetch"

[repos]
roots = ["~/perso", "~/work", "/srv"]
max_depth = 3
exclude = ["archive", "*-old"]
fetch_interval = "30m"
fetch_concurrency = 2

[repos."~/work/shared"]
context = "company-a"
update = "off"

[repos."/srv/mirror"]
update = "ff"

[forges."gitlab.company-a.example"]
kind = "gitlab"
"#;

    #[test]
    fn repo_index_keys_parse() {
        let config = parse_config(REPOS_EXAMPLE, Path::new(PATH)).unwrap();
        let name = |name: &str| name.parse::<ContextName>().unwrap();
        assert_eq!(
            config.default_context.as_ref().map(Spanned::get_ref),
            Some(&name("personal"))
        );
        assert_eq!(
            config.contexts.keys().cloned().collect::<Vec<_>>(),
            [name("company-a"), name("personal")]
        );
        let company = &config.contexts[&name("company-a")];
        assert_eq!(company.update, Some(RepoUpdatePolicy::Fetch));
        let paths: Vec<&str> = company
            .matching
            .paths
            .iter()
            .map(|path| path.get_ref().as_str())
            .collect();
        assert_eq!(paths, ["~/work/company-a", "/srv/company-a"]);
        let hosts: Vec<String> = company
            .matching
            .hosts
            .iter()
            .map(|host| host.get_ref().to_string())
            .collect();
        assert_eq!(hosts, ["gitlab.com/company-a", "gitlab.company-a.example"]);
        assert_eq!(config.contexts[&name("personal")].update, None);

        let repos = &config.repos;
        let roots: Vec<&str> = repos.roots.iter().map(ConfigPath::as_str).collect();
        assert_eq!(roots, ["~/perso", "~/work", "/srv"]);
        assert_eq!(repos.max_depth, 3);
        let exclude: Vec<&str> = repos
            .exclude
            .iter()
            .map(|pattern| pattern.glob().glob())
            .collect();
        assert_eq!(exclude, ["archive", "*-old"]);
        assert_eq!(repos.fetch_interval.get(), Duration::from_mins(30));
        assert_eq!(repos.fetch_concurrency, 2);
        assert_eq!(repos.overrides.len(), 2);
        let shared = &repos.overrides[1];
        assert_eq!(shared.path.get_ref().as_str(), "~/work/shared");
        assert_eq!(location(REPOS_EXAMPLE, shared.path.span().start).line, 19);
        assert_eq!(
            shared.context.as_ref().map(Spanned::get_ref),
            Some(&name("company-a"))
        );
        assert_eq!(shared.update, Some(RepoUpdatePolicy::Off));
        let mirror = &repos.overrides[0];
        assert_eq!(mirror.path.get_ref().as_str(), "/srv/mirror");
        assert_eq!(mirror.context, None);
        assert_eq!(mirror.update, Some(RepoUpdatePolicy::Ff));

        let forge_host = ForgeHost::try_from("gitlab.company-a.example".to_owned()).unwrap();
        assert_eq!(config.forges[&forge_host].kind, ForgeKind::Gitlab);
    }

    #[test]
    fn repo_index_defaults() {
        let config = parse_config("", Path::new(PATH)).unwrap();
        assert_eq!(config.default_context, None);
        assert!(config.contexts.is_empty());
        assert!(config.forges.is_empty());
        let repos = parse_config("[repos]\n", Path::new(PATH)).unwrap().repos;
        assert_eq!(repos, ReposConfig::default());
        let roots: Vec<&str> = repos.roots.iter().map(ConfigPath::as_str).collect();
        assert_eq!(roots, DEFAULT_ROOTS);
        assert_eq!(repos.max_depth, DEFAULT_MAX_DEPTH);
        assert_eq!(repos.exclude.len(), 0);
        assert_eq!(repos.fetch_interval.get(), DEFAULT_FETCH_INTERVAL);
        assert_eq!(repos.fetch_concurrency, DEFAULT_FETCH_CONCURRENCY);
        assert_eq!(repos.overrides.len(), 0);
        // A context without `match` is valid: overrides and `default_context` can name it.
        let config = parse_config("[contexts.personal]\n", Path::new(PATH)).unwrap();
        assert!(
            config
                .contexts
                .values()
                .all(|context| *context == ContextConfig::default())
        );
        let config = parse_config("[repos]\nroots = []\n", Path::new(PATH)).unwrap();
        assert_eq!(config.repos.roots.len(), 0);
    }

    #[test]
    fn invalid_context_values_are_rejected_with_a_location() {
        // (text, line, column, message excerpt)
        let cases: &[(&str, usize, usize, &str)] = &[
            // Context names, defined or referenced.
            ("[contexts.Personal]\n", 1, 11, "`Personal`"),
            ("[contexts.\"a_b\"]\n", 1, 11, "`a_b`"),
            ("[contexts.unassigned]\n", 1, 11, "reserved"),
            ("default_context = \"Perso\"\n", 1, 19, "`Perso`"),
            (
                "default_context = \"personal\"\n",
                1,
                19,
                "default_context `personal` is not a defined context",
            ),
            (
                "default_context = \"unassigned\"\n[contexts.personal]\n",
                1,
                19,
                "reserved",
            ),
            (
                "[contexts.personal]\n[repos.\"~/x\"]\ncontext = \"work\"\n",
                3,
                11,
                "context `work` is not a defined context",
            ),
            // `match` lists.
            (
                "[contexts.a]\nmatch.paths = []\n",
                2,
                15,
                "match.paths is empty",
            ),
            (
                "[contexts.a]\nmatch.hosts = []\n",
                2,
                15,
                "match.hosts is empty",
            ),
            (
                "[contexts.a]\nmatch.paths = [\"work\"]\n",
                2,
                15,
                "match.paths: invalid path `work`",
            ),
            (
                "[contexts.a]\nmatch.paths = \"~/work\"\n",
                2,
                15,
                "sequence",
            ),
            (
                "[contexts.a]\nmatch.hosts = [\"https://github.com/me\"]\n",
                2,
                15,
                "invalid match.hosts entry",
            ),
            (
                "[contexts.a]\nmatch.globs = [\"x\"]\n",
                2,
                7,
                "unknown field `globs`",
            ),
            ("[contexts.a]\nstore = 1\n", 2, 9, "invalid type"),
            ("[contexts.a]\nstore.backup = 1\n", 2, 16, "invalid type"),
            (
                "[contexts.a]\nstore.path = \"stores\"\n",
                2,
                14,
                "store.path: invalid path `stores`",
            ),
            ("[contexts.a]\nupdate = \"pull\"\n", 2, 10, "pull"),
        ];
        for (text, line, column, needle) in cases {
            assert_rejected(text, *line, *column, needle);
        }
    }

    #[test]
    fn invalid_repos_and_forges_values_are_rejected_with_a_location() {
        // (text, line, column, message excerpt)
        let cases: &[(&str, usize, usize, &str)] = &[
            (
                "[repos]\nroots = [\"src\"]\n",
                2,
                9,
                "roots: invalid path `src`",
            ),
            ("[repos]\nmax_depth = -1\n", 2, 13, "max_depth `-1`"),
            ("[repos]\nmax_depth = \"4\"\n", 2, 13, "expected i64"),
            (
                "[repos]\nexclude = [\"[a\"]\n",
                2,
                11,
                "invalid exclude pattern",
            ),
            (
                "[repos]\nexclude = [\"\"]\n",
                2,
                11,
                "invalid exclude pattern",
            ),
            (
                "[repos]\nfetch_interval = \"1 h\"\n",
                2,
                18,
                "invalid duration",
            ),
            (
                "[repos]\nfetch_interval = \"59s\"\n",
                2,
                18,
                "shorter than the minimum 1m",
            ),
            (
                "[repos]\nfetch_concurrency = 0\n",
                2,
                21,
                "fetch_concurrency `0`",
            ),
            ("[repos]\nroot = []\n", 2, 1, "unknown field `root`"),
            (
                "[repos.\"src\"]\nupdate = \"ff\"\n",
                1,
                8,
                "unknown field `src`",
            ),
            ("[repos.\"~/a/../b\"]\nupdate = \"ff\"\n", 1, 8, "`..`"),
            (
                "[repos.\"~/a\"]\nfetch = true\n",
                2,
                1,
                "unknown field `fetch`",
            ),
            ("[repos.\"~/a\"]\nupdate = \"rebase\"\n", 2, 10, "rebase"),
            (
                "[repos.\"~/a\"]\nupdate = \"ff\"\n[repos.\"~/a/\"]\nupdate = \"off\"\n",
                3,
                8,
                "same repository",
            ),
            // `[forges]`.
            (
                "[forges.\"GitLab.example\"]\nkind = \"gitlab\"\n",
                1,
                9,
                "[forges] host",
            ),
            (
                "[forges.\"gitlab.example\"]\nkind = \"gitea\"\n",
                2,
                8,
                "gitea",
            ),
            (
                "[forges.\"gitlab.example\"]\n",
                1,
                1,
                "missing field `kind`",
            ),
        ];
        for (text, line, column, needle) in cases {
            assert_rejected(text, *line, *column, needle);
        }
    }

    #[test]
    fn an_entry_in_two_contexts_is_rejected_with_both_locations() {
        let text = "[contexts.a]\nmatch.paths = [\"~/work\"]\n\n[contexts.b]\nmatch.paths = [\"/srv\", \"~//work/\"]\n";
        assert_rejected(
            text,
            5,
            24,
            "match.paths `~//work/` of context `b` is also `~/work` of context `a` (line 2, column 16)",
        );
        let text = "[contexts.a]\nmatch.hosts = [\"gitlab.com/x\"]\n[contexts.b]\nmatch.hosts = [\"gitlab.com/x/\"]\n";
        assert_rejected(
            text,
            4,
            16,
            "match.hosts `gitlab.com/x` of context `b` is also `gitlab.com/x` of context `a` (line 2, column 16)",
        );
        // Host paths ignore case, as their matching does.
        let text = "[contexts.a]\nmatch.hosts = [\"github.com/Me\"]\n[contexts.b]\nmatch.hosts = [\"github.com/me\"]\n";
        assert_rejected(text, 4, 16, "also `github.com/Me` of context `a`");
    }

    #[test]
    fn a_store_path_of_two_contexts_is_rejected() {
        let text = "[contexts.a]\nstore.path = \"~/s\"\n[contexts.b]\nstore.path = \"~/s/\"\n";
        assert_rejected(
            text,
            4,
            14,
            "store.path `~/s/` of context `b` is also `~/s` of context `a` (line 2, column 14)",
        );
    }

    #[test]
    fn store_paths_default_under_the_loom_home() {
        let text = "[contexts.personal]\n[contexts.work]\nstore.path = \"~/stores/work\"\n[contexts.client]\nstore.path = \"/srv/./client/\"\n";
        let config = parse_config(text, Path::new(PATH)).unwrap();
        let name = |name: &str| name.parse::<ContextName>().unwrap();
        assert_eq!(
            config.contexts[&name("work")]
                .store
                .path
                .as_ref()
                .map(|path| path.get_ref().as_str()),
            Some("~/stores/work")
        );
        let paths = config.store_paths(Path::new("/home/me/.loom"), Path::new("/home/me"));
        assert_eq!(
            paths,
            BTreeMap::from([
                (name("client"), PathBuf::from("/srv/client")),
                (
                    name("personal"),
                    PathBuf::from("/home/me/.loom/contexts/personal")
                ),
                (name("work"), PathBuf::from("/home/me/stores/work")),
            ])
        );
        assert!(
            Config::default()
                .store_paths(Path::new("/l"), Path::new("/h"))
                .is_empty()
        );
    }

    #[test]
    fn machines_are_read_and_checked() {
        let text = "[contexts.work]\nmachines = [\"laptop\", \"tower\"]\n[contexts.home]\n";
        let config = parse_config(text, Path::new(PATH)).unwrap();
        let work = &config.contexts[&"work".parse::<ContextName>().unwrap()];
        assert!(work.held_by(&"laptop".parse().unwrap()));
        assert!(!work.held_by(&"server".parse().unwrap()));
        let home = &config.contexts[&"home".parse::<ContextName>().unwrap()];
        assert!(home.held_by(&"server".parse().unwrap()));

        assert_rejected("[contexts.a]\nmachines = []\n", 2, 12, "machines is empty");
        assert_rejected(
            "[contexts.a]\nmachines = [\"my laptop\"]\n",
            2,
            12,
            "machines: invalid machine name `my laptop`",
        );
    }

    #[test]
    fn sync_interval_defaults_to_15_minutes_with_a_minimum_of_1() {
        assert_eq!(
            Config::default().tasks.sync_interval.get(),
            DEFAULT_SYNC_INTERVAL
        );
        assert_eq!(DEFAULT_SYNC_INTERVAL, Duration::from_mins(15));
        let config = parse_config("[tasks]\nsync_interval = \"5m\"\n", Path::new(PATH)).unwrap();
        assert_eq!(config.tasks.sync_interval.get(), Duration::from_mins(5));
        assert_rejected(
            "[tasks]\nsync_interval = \"30s\"\n",
            2,
            17,
            "sync_interval `30s` is shorter than the minimum 1m",
        );
        assert_rejected("[tasks]\nfoo = 1\n", 2, 1, "unknown field `foo`");
    }

    #[test]
    fn archive_after_defaults_to_7_days_with_a_minimum_of_1() {
        assert_eq!(
            Config::default().tasks.archive_after.get(),
            DEFAULT_ARCHIVE_AFTER
        );
        assert_eq!(DEFAULT_ARCHIVE_AFTER, Duration::from_hours(7 * 24));
        let config = parse_config("[tasks]\narchive_after = \"30d\"\n", Path::new(PATH)).unwrap();
        assert_eq!(
            config.tasks.archive_after.get(),
            Duration::from_hours(30 * 24)
        );
        assert_rejected(
            "[tasks]\narchive_after = \"23h\"\n",
            2,
            17,
            "archive_after `23h` is shorter than the minimum 1d",
        );
        assert_rejected(
            "[tasks]\narchive_after = \"a week\"\n",
            2,
            17,
            "archive_after: invalid duration `a week`",
        );
    }

    #[test]
    fn backup_remotes_and_machines_are_read_and_checked() {
        let text = "[contexts.work]\nmachines = [\"laptop\", \"tower\"]\nstore.backup.remote = \"git@example.com:me/work-store.git\"\n[contexts.home]\n";
        let config = parse_config(text, Path::new(PATH)).unwrap();
        let work = &config.contexts[&"work".parse::<ContextName>().unwrap()];
        assert_eq!(
            work.store
                .backup
                .as_ref()
                .map(|backup| backup.remote.get_ref().as_str()),
            Some("git@example.com:me/work-store.git")
        );
        assert!(work.held_by(&"laptop".parse().unwrap()));
        assert!(!work.held_by(&"server".parse().unwrap()));
        let home = &config.contexts[&"home".parse::<ContextName>().unwrap()];
        assert!(home.held_by(&"server".parse().unwrap()));

        assert_rejected("[contexts.a]\nmachines = []\n", 2, 12, "machines is empty");
        assert_rejected(
            "[contexts.a]\nmachines = [\"my laptop\"]\n",
            2,
            12,
            "machines: invalid machine name `my laptop`",
        );
        assert_rejected(
            "[contexts.a]\nstore.backup.remote = \"--upload-pack=x\"\n",
            2,
            23,
            "starts with `-`",
        );
        assert_rejected(
            "[contexts.a]\nstore.backup.remote = \"\"\n",
            2,
            23,
            "store.backup.remote: empty",
        );
        assert_rejected(
            "[contexts.a]\nstore.backup.branch = \"x\"\n",
            2,
            14,
            "unknown field `branch`",
        );
    }

    #[test]
    fn a_backup_remote_of_two_contexts_is_rejected() {
        let text = "[contexts.a]\nstore.backup.remote = \"/srv/s.git\"\n[contexts.b]\nstore.backup.remote = \"/srv/s.git\"\n";
        assert_rejected(
            text,
            4,
            23,
            "store.backup.remote `/srv/s.git` of context `b` is also `/srv/s.git` of context `a` (line 2, column 23)",
        );
    }

    #[test]
    fn stores_keep_the_contexts_this_machine_holds() {
        let text = "[contexts.personal]\n[contexts.work]\nmachines = [\"laptop\"]\nstore.backup.remote = \"/srv/work.git\"\n[contexts.lab]\nmachines = [\"tower\"]\n";
        let config = parse_config(text, Path::new(PATH)).unwrap();
        let name = |name: &str| name.parse::<ContextName>().unwrap();
        let stores = config.stores(Path::new("/l"), Path::new("/h"), &"laptop".parse().unwrap());
        assert_eq!(
            stores.held,
            BTreeMap::from([
                (
                    name("personal"),
                    StoreSettings {
                        path: PathBuf::from("/l/contexts/personal"),
                        remote: None,
                    }
                ),
                (
                    name("work"),
                    StoreSettings {
                        path: PathBuf::from("/l/contexts/work"),
                        remote: Some("/srv/work.git".to_owned()),
                    }
                ),
            ])
        );
        assert_eq!(stores.not_held, [name("lab")]);
    }

    #[test]
    fn schedules_are_read() {
        let text = r#"[contexts.company-a]

[[contexts.company-a.schedules]]
id = "timesheet"
title = "Fill in the timesheet"
kind = "admin"
importance = "p1"
rrule = "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1"
policy = "replace"
due = "same-day"

[[contexts.company-a.schedules]]
id = "review-1on1"
title = "Prepare the 1:1"
kind = "chore"
rrule = "freq=weekly;interval=2;byday=fr"
start = 2026-10-02
"#;
        let config = parse_config(text, Path::new(PATH)).unwrap();
        let context = &config.contexts[&"company-a".parse::<ContextName>().unwrap()];
        let [timesheet, review] = context.schedules.as_slice() else {
            panic!("two schedules expected: {:?}", context.schedules);
        };
        assert_eq!(timesheet.id.get_ref().as_str(), "timesheet");
        assert_eq!(timesheet.title, "Fill in the timesheet");
        assert_eq!(timesheet.kind, Kind::Admin);
        assert_eq!(timesheet.importance, Some(Importance::P1));
        assert_eq!(
            timesheet.recurrence.rule().to_string(),
            "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1"
        );
        assert_eq!(timesheet.recurrence.start(), None);
        assert_eq!(timesheet.policy, Policy::Replace);
        assert_eq!(timesheet.due, Some(DueRule::SameDay));
        // Defaults: importance of the kind, policy skip, no due date.
        assert_eq!(review.importance, None);
        assert_eq!(review.policy, Policy::Skip);
        assert_eq!(review.due, None);
        assert_eq!(
            review.recurrence.rule().to_string(),
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=FR"
        );
        assert_eq!(
            review.recurrence.start(),
            Some("2026-10-02".parse().unwrap())
        );
        // A start may also be a string.
        let quoted = text.replace("start = 2026-10-02", "start = \"2026-10-02\"");
        assert_eq!(parse_config(&quoted, Path::new(PATH)).unwrap(), config);
    }

    #[test]
    fn invalid_schedules_are_rejected_with_a_location() {
        const HEAD: &str = "[contexts.a]\n[[contexts.a.schedules]]\n";
        const VALID: &str = "id = \"s\"\ntitle = \"T\"\nkind = \"chore\"\nrrule = \"FREQ=DAILY\"\n";
        // (the line given, then line, column and message excerpt of the error)
        let cases: &[(&str, usize, usize, &str)] = &[
            (
                "rrule = \"FREQ=YEARLY\"",
                3,
                9,
                "`FREQ=YEARLY` is not supported",
            ),
            (
                "rrule = \"FREQ=DAILY;BYHOUR=9\"",
                3,
                9,
                "`BYHOUR` is not supported",
            ),
            (
                "rrule = \"FREQ=DAILY;COUNT=2\"",
                3,
                9,
                "`COUNT` is not supported",
            ),
            ("rrule = \"BYDAY=FR\"", 3, 9, "FREQ is missing"),
            ("kind = \"meeting\"", 3, 8, "unknown kind `meeting`"),
            ("importance = \"high\"", 3, 14, "high"),
            ("policy = \"merge\"", 3, 10, "merge"),
            ("due = \"next-day\"", 3, 7, "next-day"),
            ("start = 2026-10-02T09:00:00", 3, 9, "a date is expected"),
            ("start = \"tomorrow\"", 3, 9, "a date is expected"),
            ("title = \"\"", 3, 9, "not empty"),
            ("id = \"Weekly\"", 3, 6, "invalid schedule id `Weekly`"),
            ("id = \"a/b\"", 3, 6, "invalid schedule id"),
            ("cron = \"0 9 * * 5\"", 3, 1, "unknown field `cron`"),
            (
                "rrule = \"FREQ=WEEKLY\"",
                2,
                1,
                "schedule `s`: `FREQ=WEEKLY` needs `start`",
            ),
        ];
        for (line_text, line, column, needle) in cases {
            // The case first, then the valid keys it does not replace.
            let key = format!("{} ", line_text.split(' ').next().unwrap());
            let rest: Vec<&str> = VALID
                .lines()
                .filter(|valid| !valid.starts_with(&key))
                .collect();
            let rest = rest.join("\n");
            let text = format!("{HEAD}{line_text}\n{rest}\n");
            assert_rejected(&text, *line, *column, needle);
        }
        let missing = format!("{HEAD}id = \"s\"\ntitle = \"T\"\nkind = \"chore\"\n");
        assert_rejected(&missing, 2, 1, "missing field `rrule`");
    }

    #[test]
    fn a_schedule_id_twice_in_a_context_is_rejected() {
        let schedule = "id = \"s\"\ntitle = \"T\"\nkind = \"chore\"\nrrule = \"FREQ=DAILY\"\n";
        let text = format!(
            "[contexts.a]\n[[contexts.a.schedules]]\n{schedule}[[contexts.a.schedules]]\n{schedule}"
        );
        assert_rejected(
            &text,
            8,
            6,
            "schedule id `s` of context `a` is already used (line 3, column 6)",
        );
        // Two contexts may use the same ID.
        let text = format!(
            "[contexts.a]\n[[contexts.a.schedules]]\n{schedule}[contexts.b]\n[[contexts.b.schedules]]\n{schedule}"
        );
        assert!(parse_config(&text, Path::new(PATH)).is_ok());
    }

    #[test]
    fn nested_or_repeated_entries_are_not_ties() {
        // Different lengths: the longest prefix wins, no tie. A repeat inside one context is
        // harmless.
        let text = "[contexts.a]\nmatch.paths = [\"~/work\", \"~/work/\"]\nmatch.hosts = [\"gitlab.com\"]\n[contexts.b]\nmatch.paths = [\"~/work/b\"]\nmatch.hosts = [\"gitlab.com/b\"]\n";
        assert!(parse_config(text, Path::new(PATH)).is_ok());
    }

    #[test]
    fn location_counts_lines_and_characters() {
        assert_eq!(location("", 0), Location { line: 1, column: 1 });
        assert_eq!(location("ab\ncd", 4), Location { line: 2, column: 2 });
        assert_eq!(location("é = x", 2), Location { line: 1, column: 2 });
        // Inside `é`: clamped to its start. Past the end: clamped to the end.
        assert_eq!(location("é", 1), Location { line: 1, column: 1 });
        assert_eq!(location("a\n", 99), Location { line: 2, column: 1 });
    }
}
