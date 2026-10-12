//! Command-line definition of `loom` (ADR-0010).

use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use clap::{ArgAction, ColorChoice, Parser, Subcommand};
use loom_api::VersionInfo;

/// Build information, computed once for `--version` and `loom version`.
pub static VERSION_INFO: LazyLock<VersionInfo> = LazyLock::new(VersionInfo::current);
/// `--version` output: every build field, identical to `loomd --version`.
static LONG_VERSION: LazyLock<String> = LazyLock::new(|| VERSION_INFO.to_string());

/// Personal work engine: CLI and TUI.
#[derive(Debug, Parser)]
#[command(
    name = "loom",
    version = VERSION_INFO.version.as_str(),
    long_version = LONG_VERSION.as_str(),
    about
)]
pub struct Cli {
    /// Options accepted by every command.
    #[command(flatten)]
    pub global: GlobalArgs,
    /// Command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Options accepted by every command, before or after the command name.
#[derive(Debug, clap::Args)]
pub struct GlobalArgs {
    /// Context to work in (lowercase letters, digits and `-`, starting with a letter).
    #[arg(short = 'c', long, global = true, value_name = "NAME")]
    pub context: Option<ContextName>,
    /// Print errors only on stderr.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,
    /// Print more diagnostics on stderr; repeat for more (`-vv`).
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
    /// When to use colors: `auto` colors a terminal unless `NO_COLOR` is set.
    #[arg(long, global = true, value_name = "WHEN", default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,
    /// Answer yes to every confirmation (the only way to skip one).
    #[arg(short, long, global = true)]
    pub yes: bool,
}

/// `loom` commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print version, commit, target, channel and profile of this build.
    Version {
        /// Print the build information as JSON (`VersionInfo` schema).
        #[arg(long)]
        json: bool,
    },
    /// Show the state of loomd, starting it when it is not running.
    Status {
        /// Print the daemon state as JSON (`StatusResponse` schema).
        #[arg(long)]
        json: bool,
        /// Fail instead of starting loomd when it is not running.
        #[arg(long)]
        no_start: bool,
    },
}

/// Name of a context, as given to `-c`/`--context`.
///
/// 1 to 64 characters: lowercase ASCII letters, digits and `-`, starting with a
/// letter and not ending with `-`. Paths and `key=value` pairs are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContextName(String);

/// Maximum length of a context name.
const CONTEXT_NAME_MAX_LEN: usize = 64;

/// Why a context name is invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextNameError {
    /// The name is empty.
    #[error("a context name is expected, got an empty string")]
    Empty,
    /// The name is longer than 64 characters.
    #[error("a context name has at most {CONTEXT_NAME_MAX_LEN} characters")]
    TooLong,
    /// The name looks like a path.
    #[error("a context name is expected, not a path: `{0}`")]
    Path(String),
    /// The name looks like a `key=value` pair.
    #[error("a context name is expected, not a `key=value` pair: `{0}`")]
    KeyValue(String),
    /// The name has a character outside the allowed set, or a bad first or last character.
    #[error(
        "a context name is expected (lowercase letters, digits and `-`, starting with a letter, not ending with `-`): `{0}`"
    )]
    Invalid(String),
}

impl FromStr for ContextName {
    type Err = ContextNameError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        if name.is_empty() {
            return Err(ContextNameError::Empty);
        }
        if name.contains('/') || name.starts_with('.') || name.starts_with('~') {
            return Err(ContextNameError::Path(name.to_owned()));
        }
        if name.contains('=') {
            return Err(ContextNameError::KeyValue(name.to_owned()));
        }
        if name.len() > CONTEXT_NAME_MAX_LEN {
            return Err(ContextNameError::TooLong);
        }
        let starts_with_letter = name.starts_with(|c: char| c.is_ascii_lowercase());
        let allowed = name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !starts_with_letter || !allowed || name.ends_with('-') {
            return Err(ContextNameError::Invalid(name.to_owned()));
        }
        Ok(Self(name.to_owned()))
    }
}

impl ContextName {
    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContextName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, ContextName, ContextNameError};
    use clap::error::ErrorKind;
    use clap::{ColorChoice, CommandFactory, Parser};

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn valid_context_names() {
        for name in ["a", "personal", "company-a", "client-2", &"a".repeat(64)] {
            assert_eq!(name.parse::<ContextName>().unwrap().as_str(), name);
        }
    }

    #[test]
    fn invalid_context_names() {
        let cases: [(&str, ContextNameError); 11] = [
            ("", ContextNameError::Empty),
            (&"a".repeat(65), ContextNameError::TooLong),
            ("./foo", ContextNameError::Path("./foo".into())),
            ("~/src", ContextNameError::Path("~/src".into())),
            ("src/foo", ContextNameError::Path("src/foo".into())),
            ("..", ContextNameError::Path("..".into())),
            ("a=b", ContextNameError::KeyValue("a=b".into())),
            ("Personal", ContextNameError::Invalid("Personal".into())),
            ("2fa", ContextNameError::Invalid("2fa".into())),
            ("a_b", ContextNameError::Invalid("a_b".into())),
            ("a-", ContextNameError::Invalid("a-".into())),
        ];
        for (name, expected) in cases {
            assert_eq!(name.parse::<ContextName>(), Err(expected), "{name:?}");
        }
    }

    #[test]
    fn global_flags_parse_before_and_after_the_command() {
        for args in [
            ["loom", "-c", "personal", "-vv", "-y", "version"].as_slice(),
            ["loom", "version", "--context=personal", "-v", "-v", "--yes"].as_slice(),
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert_eq!(cli.global.context.unwrap().as_str(), "personal");
            assert_eq!(cli.global.verbose, 2);
            assert!(cli.global.yes);
            assert!(!cli.global.quiet);
            assert_eq!(cli.global.color, ColorChoice::Auto);
            assert!(matches!(cli.command, Command::Version { json: false }));
        }
    }

    #[test]
    fn status_flags() {
        let cli = Cli::try_parse_from(["loom", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Status {
                json: false,
                no_start: false
            }
        ));
        let cli = Cli::try_parse_from(["loom", "status", "--json", "--no-start"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Status {
                json: true,
                no_start: true
            }
        ));
    }

    #[test]
    fn grouped_short_flags() {
        let cli = Cli::try_parse_from(["loom", "-yv", "version"]).unwrap();
        assert!(cli.global.yes);
        assert_eq!(cli.global.verbose, 1);
    }

    #[test]
    fn quiet_conflicts_with_verbose() {
        let error = Cli::try_parse_from(["loom", "-q", "-v", "version"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn color_values() {
        for (value, expected) in [
            ("auto", ColorChoice::Auto),
            ("always", ColorChoice::Always),
            ("never", ColorChoice::Never),
        ] {
            let cli = Cli::try_parse_from(["loom", "--color", value, "version"]).unwrap();
            assert_eq!(cli.global.color, expected);
        }
        let error = Cli::try_parse_from(["loom", "--color", "sometimes", "version"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }
}
