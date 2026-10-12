//! Command-line definition of `loomd` (ADR-0010).

use std::path::PathBuf;
use std::sync::LazyLock;

use clap::{ArgAction, ColorChoice, Parser};
use loom_api::VersionInfo;

/// Build information, computed once for `--version`.
static VERSION_INFO: LazyLock<VersionInfo> = LazyLock::new(VersionInfo::current);
/// `--version` output: every build field, identical to `loom version`.
static LONG_VERSION: LazyLock<String> = LazyLock::new(|| VERSION_INFO.to_string());

/// Loom daemon: API server, scheduler, updater.
#[derive(Debug, Parser)]
#[command(
    name = "loomd",
    version = VERSION_INFO.version.as_str(),
    long_version = LONG_VERSION.as_str(),
    about
)]
pub struct Cli {
    /// Loom home directory. Overrides `LOOM_HOME`; default `~/.loom`.
    #[arg(long, value_name = "DIR")]
    pub loom_home: Option<PathBuf>,
    /// Print errors only on stderr.
    #[arg(short, long, conflicts_with = "verbose")]
    pub quiet: bool,
    /// Print more diagnostics on stderr; repeat for more (`-vv`).
    #[arg(short, long, action = ArgAction::Count)]
    pub verbose: u8,
    /// When to use colors: `auto` colors a terminal unless `NO_COLOR` is set.
    #[arg(long, value_name = "WHEN", default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::error::ErrorKind;
    use clap::{ColorChoice, CommandFactory, Parser};

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn flags_parse() {
        let cli = Cli::try_parse_from(["loomd", "-vv", "--color=never"]).unwrap();
        assert_eq!(cli.verbose, 2);
        assert_eq!(cli.color, ColorChoice::Never);
        assert!(!cli.quiet);
        assert_eq!(cli.loom_home, None);
    }

    #[test]
    fn loom_home_takes_a_directory() {
        let cli = Cli::try_parse_from(["loomd", "--loom-home", "/tmp/l"]).unwrap();
        assert_eq!(
            cli.loom_home.as_deref(),
            Some(std::path::Path::new("/tmp/l"))
        );
    }

    #[test]
    fn quiet_conflicts_with_verbose() {
        let error = Cli::try_parse_from(["loomd", "-q", "-v"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }
}
