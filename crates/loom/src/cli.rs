//! Command-line definition of `loom` (ADR-0010).

use std::path::PathBuf;
use std::sync::LazyLock;

use clap::{ArgAction, ArgGroup, ArgMatches, ColorChoice, Parser, Subcommand, ValueEnum};
use loom_api::{ContextName, TEST_BUILD_VERSION_ENV, VersionInfo};
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
    /// List, show, scan, register and check the repositories loomd indexes.
    ///
    /// Starts loomd when it is not running (except `doctor`). Run inside a repository the index
    /// does not know, any `loom` command registers it.
    Repo {
        /// Repository command to run.
        #[command(subcommand)]
        command: RepoCommand,
    },
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
    /// Release channel to upgrade on, remembered in `state/update.json` for loomd and the next
    /// `loom upgrade` (not with `--dry-run`). Default: `[update] channel`, otherwise the channel
    /// last given here, otherwise the channel of this build (required for a local build, unless
    /// `--version` or `--from` is given).
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
    /// Print the decision only: no download, nothing installed (a `--channel` is still
    /// remembered).
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

/// `loom repo` commands.
///
/// A `<REPO>` argument is a path (absolute, relative starting with `.`, or `~`), walked up to
/// the repository root; a slug `<context>/<name>`; or a name, which `-c` or the context of the
/// current directory narrows when several repositories have it.
#[derive(Debug, Subcommand)]
pub enum RepoCommand {
    /// List the indexed repositories, sorted by context then name (`-c` keeps one context).
    Ls(RepoLsArgs),
    /// Show a repository: identity, context and the rule that chose it, state.
    Show(RepoShowArgs),
    /// Scan the roots and the registered repositories now.
    Scan(RepoScanArgs),
    /// Add a repository outside the roots to the index; scans keep it while it exists.
    Register(RepoRegisterArgs),
    /// Remove a repository from the index (registering it again undoes it).
    Forget(RepoForgetArgs),
    /// Check roots, contexts, unassigned repositories, loomd's SSH agent and SSH access to
    /// each host; print the fixes. Exits with 1 when a check fails.
    Doctor(RepoDoctorArgs),
}

/// Output format of `loom repo ls`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RepoListFormat {
    /// Aligned columns: unassigned flag (`!`), slug, HEAD, state, path.
    Table,
    /// One absolute path per line, nothing else (for `fzf`).
    Paths,
}

/// Options of `loom repo ls`.
#[derive(Debug, clap::Args)]
pub struct RepoLsArgs {
    /// Print the list as JSON (`RepoList` schema).
    #[arg(long, conflicts_with = "format")]
    pub json: bool,
    /// Output format.
    #[arg(long, value_name = "FORMAT", value_enum, default_value_t = RepoListFormat::Table)]
    pub format: RepoListFormat,
}

/// Options of `loom repo show`.
#[derive(Debug, clap::Args)]
pub struct RepoShowArgs {
    /// Repository: path, `<context>/<name>` or name. Default: the repository of the current
    /// directory. A path the index does not know is registered.
    #[arg(value_name = "REPO")]
    pub repo: Option<String>,
    /// Read the repository's state now instead of showing the stored one.
    #[arg(long)]
    pub refresh: bool,
    /// Print the repository as JSON (`RepoSummary` schema).
    #[arg(long)]
    pub json: bool,
}

/// Options of `loom repo scan`.
#[derive(Debug, clap::Args)]
pub struct RepoScanArgs {
    /// Drop the whole index and rebuild it from the roots: registered repositories outside the
    /// roots, `last_used` and states are lost. Asks for confirmation.
    #[arg(long)]
    pub rebuild: bool,
    /// Print what would change, change nothing.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Print the result as JSON (`ScanResult` schema).
    #[arg(long)]
    pub json: bool,
}

/// Options of `loom repo register`.
#[derive(Debug, clap::Args)]
pub struct RepoRegisterArgs {
    /// A directory inside the repository. Default: the current directory.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Print the repository that would be registered, change nothing.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Print the registered repository as JSON (`RepoSummary` schema).
    #[arg(long)]
    pub json: bool,
}

/// Options of `loom repo forget`.
#[derive(Debug, clap::Args)]
pub struct RepoForgetArgs {
    /// Repository: path, `<context>/<name>` or name.
    #[arg(value_name = "REPO")]
    pub repo: String,
    /// Print the repository that would be removed, change nothing.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Print the removed repository as JSON (`RepoSummary` schema).
    #[arg(long)]
    pub json: bool,
}

/// Options of `loom repo doctor`.
#[derive(Debug, clap::Args)]
pub struct RepoDoctorArgs {
    /// Print the report as JSON (`DoctorReport` schema).
    #[arg(long)]
    pub json: bool,
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

#[cfg(test)]
mod tests {
    use super::{Cli, Command, ReleaseChannel, ServiceCommand, command_path};
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
            | Command::Repo { .. }
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

    fn repo(args: &[&str]) -> super::RepoCommand {
        let mut full = vec!["loom", "repo"];
        full.extend_from_slice(args);
        match Cli::try_parse_from(full).unwrap().command {
            Command::Repo { command } => command,
            other => panic!("not a repo command: {other:?}"),
        }
    }

    #[test]
    fn repo_commands_parse() {
        use super::{RepoCommand, RepoListFormat};
        let RepoCommand::Ls(ls) = repo(&["ls"]) else {
            panic!("ls")
        };
        assert_eq!(ls.format, RepoListFormat::Table);
        assert!(!ls.json);
        let RepoCommand::Ls(ls) = repo(&["ls", "--format", "paths"]) else {
            panic!("ls")
        };
        assert_eq!(ls.format, RepoListFormat::Paths);
        let RepoCommand::Show(show) = repo(&["show", "--refresh", "--json"]) else {
            panic!("show")
        };
        assert_eq!(show.repo, None);
        assert!(show.refresh && show.json);
        let RepoCommand::Scan(scan) = repo(&["scan", "--rebuild", "-n"]) else {
            panic!("scan")
        };
        assert!(scan.rebuild && scan.dry_run && !scan.json);
        let RepoCommand::Register(register) = repo(&["register"]) else {
            panic!("register")
        };
        assert_eq!(register.path, None);
        let RepoCommand::Forget(forget) = repo(&["forget", "work/x", "-n"]) else {
            panic!("forget")
        };
        assert_eq!(forget.repo, "work/x");
        assert!(forget.dry_run);
        assert!(matches!(repo(&["doctor", "--json"]), RepoCommand::Doctor(args) if args.json));
        // `-c` reaches the repo commands.
        let cli = Cli::try_parse_from(["loom", "repo", "ls", "-c", "work"]).unwrap();
        assert_eq!(cli.global.context.unwrap().as_str(), "work");
    }

    #[test]
    fn repo_usage_errors() {
        for args in [
            &["loom", "repo"][..],
            &["loom", "repo", "ls", "--json", "--format", "paths"],
            &["loom", "repo", "ls", "--format", "csv"],
            &["loom", "repo", "forget"],
            &["loom", "repo", "doctor", "-n"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
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
