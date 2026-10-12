//! `LOOM_HOME/config.toml`: parsing and error locations.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use loom_api::{UpdateChannel, UpdatePolicyMode};
use serde::{Deserialize, Deserializer};

use crate::duration::{format_duration, parse_duration};

/// Content of `config.toml`. Unknown keys and tables are rejected at every level.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `[daemon]` table.
    #[serde(default)]
    pub daemon: Daemon,
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
    toml::from_str(text).map_err(|error| ConfigError {
        path: path.to_path_buf(),
        location: error.span().map(|span| location(text, span.start)),
        message: error.message().trim_end().to_owned(),
    })
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
        CheckInterval, Config, DEFAULT_CHECK_INTERVAL, Daemon, Location, OtlpEndpoint,
        TelemetryConfig, Update, location, parse_config,
    };
    use loom_api::{UpdateChannel, UpdatePolicyMode};
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
