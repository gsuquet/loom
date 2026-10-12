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
    /// `[telemetry]` table: export of traces for diagnosing loom.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// `[update]` table.
    #[serde(default)]
    pub update: Update,
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
    /// `channel`: `stable` or `dev`. `None`: the build channel.
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
/// exist, no `match.paths` or `match.hosts` entry is in two contexts, no repository path is
/// given twice.
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
    // as their matching does.
    let mut paths = BTreeMap::new();
    let mut hosts = BTreeMap::new();
    for (name, context) in &config.contexts {
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
fn location(text: &str, offset: usize) -> Location {
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
        CheckInterval, Config, DEFAULT_CHECK_INTERVAL, Daemon, Location, OtlpEndpoint, Spanned,
        TelemetryConfig, Update, location, parse_config,
    };
    use crate::repos::{
        ConfigPath, ContextConfig, DEFAULT_FETCH_CONCURRENCY, DEFAULT_FETCH_INTERVAL,
        DEFAULT_MAX_DEPTH, DEFAULT_ROOTS, ForgeHost, ReposConfig,
    };
    use loom_api::{ContextName, ForgeKind, RepoUpdatePolicy, UpdateChannel, UpdatePolicyMode};
    use std::path::Path;
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
            ("[contexts.a]\nstore = 1\n", 2, 1, "unknown field `store`"),
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
