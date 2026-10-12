//! Command-line definition of `loom` (ADR-0010).

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::LazyLock;

use clap::{ArgAction, ArgGroup, ArgMatches, ColorChoice, Parser, Subcommand, ValueEnum};
use loom_api::{TEST_BUILD_VERSION_ENV, VersionInfo};
use semver::Version;

/// Build information, computed once for `--version` and `loom version`;
/// `LOOM_TEST_BUILD_VERSION` replaces the version in a debug build (test hook).
pub static VERSION_INFO: LazyLock<VersionInfo> = LazyLock::new(|| {
    VersionInfo::current().with_test_version(std::env::var(TEST_BUILD_VERSION_ENV).ok().as_deref())
});
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
    /// Install, remove or inspect loomd as a user service (launchd on macOS, systemd on Linux).
    Service {
        /// Service command to run.
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Upgrade `loom` and `loomd` to the latest release of a channel, to a given release, or to
    /// a local build.
    ///
    /// Both binaries are replaced in the directory of the running `loom`; the replaced version
    /// is kept in `<loom home>/versions/`. Releases are verified (minisign signature of
    /// `SHA256SUMS`, then the archive checksum) before anything is replaced.
    Upgrade(UpgradeArgs),
    /// Restore the previous version: the highest version kept in `<loom home>/versions/` other
    /// than the running one.
    Rollback(RollbackArgs),
    /// Internal: started by loomd from the saved previous version after an auto-upgrade;
    /// waits for the new loomd and rolls back when it does not answer (ADR-0011).
    #[command(name = "__upgrade-watchdog", hide = true)]
    UpgradeWatchdog(WatchdogArgs),
}

/// Options of the internal `__upgrade-watchdog` command.
#[derive(Debug, clap::Args)]
pub struct WatchdogArgs {
    /// Version replaced, restored on failure (`<loom home>/versions/<from>/`).
    #[arg(long, value_name = "VERSION")]
    pub from: Version,
    /// Version the new loomd must report.
    #[arg(long, value_name = "VERSION")]
    pub expect: Version,
    /// Seconds the new loomd has to answer.
    #[arg(long, value_name = "SECS")]
    pub deadline: u64,
    /// Directory holding the installed `loom` and `loomd`.
    #[arg(long, value_name = "DIR")]
    pub install_dir: PathBuf,
}

/// Options of `loom upgrade`.
#[derive(Debug, clap::Args)]
#[command(group(ArgGroup::new("source").args(["channel", "version", "from"])))]
pub struct UpgradeArgs {
    /// Release channel to upgrade on. Default: the channel of this build (required for a local
    /// build, unless `--version` or `--from` is given).
    #[arg(long, value_name = "CHANNEL", value_enum)]
    pub channel: Option<ReleaseChannel>,
    /// Release to install: `X.Y.Z` or `X.Y.Z-dev.N`. Never lower than the running version
    /// (use `loom rollback`).
    #[arg(long, value_name = "VERSION", value_parser = parse_release_version)]
    pub version: Option<Version>,
    /// Install a local build: a release archive (`.tar.gz`) or a directory holding `loom` and
    /// `loomd`. Not verified; installed even when its version is not newer.
    #[arg(long, value_name = "PATH")]
    pub from: Option<PathBuf>,
    /// Print the decision only: no download, nothing changed.
    #[arg(long, conflicts_with_all = ["from", "dry_run"])]
    pub check: bool,
    /// Download and verify, print what would be replaced, change nothing.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Print the result as JSON (`UpgradeReport` schema).
    #[arg(long)]
    pub json: bool,
}

/// Options of `loom rollback`.
#[derive(Debug, clap::Args)]
pub struct RollbackArgs {
    /// Print what would be restored, change nothing.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Print the result as JSON (`RollbackReport` schema).
    #[arg(long)]
    pub json: bool,
}

/// Release channel given to `loom upgrade --channel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReleaseChannel {
    /// Tagged releases.
    Stable,
    /// Prereleases built from every merge to `main`.
    Dev,
}

/// Parses a release version: `X.Y.Z` or `X.Y.Z-dev.N`.
fn parse_release_version(text: &str) -> Result<Version, String> {
    loom_update::release::tag_version(&format!("v{text}"))
        .ok_or_else(|| format!("expected X.Y.Z or X.Y.Z-dev.N, got `{text}`"))
}

/// `loom service` commands.
#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Install loomd as a user service and start it; no-op when already installed and loaded.
    /// Refused when `[daemon] service = false`.
    Install {
        /// Print the file that would be written and the commands that would run; change nothing.
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    /// Stop loomd, unload the user service and remove its file; no-op when not installed.
    Uninstall {
        /// Print the commands that would run and the file that would be removed; change nothing.
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    /// Report whether the user service is installed, loaded and running.
    Status {
        /// Print the state as JSON (`ServiceStatus` schema).
        #[arg(long)]
        json: bool,
    },
}

/// The command line's command path, such as `loom status`: `loom` followed by each
/// subcommand name. Names the root span of the command.
#[must_use]
pub fn command_path(matches: &ArgMatches) -> String {
    let mut path = String::from("loom");
    let mut current = matches;
    while let Some((name, sub)) = current.subcommand() {
        path.push(' ');
        path.push_str(name);
        current = sub;
    }
    path
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
    use super::{
        Cli, Command, ContextName, ContextNameError, ReleaseChannel, ServiceCommand, command_path,
    };
    use clap::error::ErrorKind;
    use clap::{ColorChoice, CommandFactory, Parser};

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn command_path_names_every_subcommand() {
        let path =
            |args: &[&str]| command_path(&Cli::command().try_get_matches_from(args).unwrap());
        assert_eq!(path(&["loom", "version"]), "loom version");
        assert_eq!(
            path(&["loom", "-c", "personal", "status", "--json"]),
            "loom status"
        );
        // Nested subcommands, as `loom service install` will have.
        let nested = clap::Command::new("loom")
            .subcommand(clap::Command::new("service").subcommand(clap::Command::new("install")));
        let matches = nested
            .try_get_matches_from(["loom", "service", "install"])
            .unwrap();
        assert_eq!(command_path(&matches), "loom service install");
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
    fn service_commands_parse() {
        let parse = |args: &[&str]| match Cli::try_parse_from(args).unwrap().command {
            Command::Service { command } => command,
            Command::Version { .. }
            | Command::Status { .. }
            | Command::Upgrade(_)
            | Command::Rollback(_)
            | Command::UpgradeWatchdog(_) => {
                panic!("not a service command: {args:?}")
            }
        };
        assert!(matches!(
            parse(&["loom", "service", "install"]),
            ServiceCommand::Install { dry_run: false }
        ));
        assert!(matches!(
            parse(&["loom", "service", "install", "-n"]),
            ServiceCommand::Install { dry_run: true }
        ));
        assert!(matches!(
            parse(&["loom", "service", "uninstall", "--dry-run"]),
            ServiceCommand::Uninstall { dry_run: true }
        ));
        assert!(matches!(
            parse(&["loom", "service", "status", "--json"]),
            ServiceCommand::Status { json: true }
        ));
    }

    #[test]
    fn service_needs_a_subcommand_and_status_has_no_dry_run() {
        let missing = Cli::try_parse_from(["loom", "service"]).unwrap_err();
        assert_eq!(
            missing.kind(),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        let error = Cli::try_parse_from(["loom", "service", "status", "-n"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UnknownArgument);
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
    fn upgrade_sources_are_mutually_exclusive() {
        for args in [
            &["loom", "upgrade", "--channel", "dev", "--version", "1.0.0"][..],
            &["loom", "upgrade", "--channel", "dev", "--from", "x.tar.gz"],
            &["loom", "upgrade", "--version", "1.0.0", "--from", "dir"],
            &["loom", "upgrade", "--check", "--from", "dir"],
            &[
                "loom",
                "upgrade",
                "--check",
                "--dry-run",
                "--channel",
                "dev",
            ],
        ] {
            let error = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{args:?}");
        }
    }

    #[test]
    fn upgrade_parses_channel_version_and_flags() {
        let cli =
            Cli::try_parse_from(["loom", "upgrade", "--channel", "dev", "-n", "--json"]).unwrap();
        let Command::Upgrade(args) = cli.command else {
            panic!("not upgrade");
        };
        assert_eq!(args.channel, Some(ReleaseChannel::Dev));
        assert!(args.dry_run && args.json && !args.check);

        let cli = Cli::try_parse_from(["loom", "upgrade", "--version", "1.2.3-dev.4"]).unwrap();
        let Command::Upgrade(args) = cli.command else {
            panic!("not upgrade");
        };
        assert_eq!(args.version.unwrap().to_string(), "1.2.3-dev.4");
    }

    #[test]
    fn upgrade_rejects_other_versions() {
        for version in ["v1.2.3", "1.2", "1.2.3-rc.1", "latest"] {
            let error = Cli::try_parse_from(["loom", "upgrade", "--version", version]).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ValueValidation, "{version}");
        }
    }

    #[test]
    fn rollback_flags() {
        let cli = Cli::try_parse_from(["loom", "rollback", "--dry-run"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Rollback(super::RollbackArgs {
                dry_run: true,
                json: false
            })
        ));
    }

    #[test]
    fn watchdog_is_hidden_and_parses() {
        let cli = Cli::try_parse_from([
            "loom",
            "__upgrade-watchdog",
            "--from",
            "0.1.0",
            "--expect",
            "0.2.0-dev.3",
            "--deadline",
            "60",
            "--install-dir",
            "/bin",
        ])
        .unwrap();
        let Command::UpgradeWatchdog(args) = cli.command else {
            panic!("{:?}", cli.command);
        };
        assert_eq!(args.expect.to_string(), "0.2.0-dev.3");
        assert_eq!(args.deadline, 60);
        let help = Cli::command().render_long_help().to_string();
        assert!(!help.contains("watchdog"), "{help}");
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
