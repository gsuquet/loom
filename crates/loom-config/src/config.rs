//! `LOOM_HOME/config.toml`: parsing and error locations.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Content of `config.toml`. Unknown keys and tables are rejected at every level.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `[daemon]` table.
    #[serde(default)]
    pub daemon: Daemon,
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
    use super::{Config, Daemon, Location, location, parse_config};
    use std::path::Path;

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
    fn location_counts_lines_and_characters() {
        assert_eq!(location("", 0), Location { line: 1, column: 1 });
        assert_eq!(location("ab\ncd", 4), Location { line: 2, column: 2 });
        assert_eq!(location("é = x", 2), Location { line: 1, column: 2 });
        // Inside `é`: clamped to its start. Past the end: clamped to the end.
        assert_eq!(location("é", 1), Location { line: 1, column: 1 });
        assert_eq!(location("a\n", 99), Location { line: 2, column: 1 });
    }
}
