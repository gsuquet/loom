//! `loomd`: the daemon. Placeholder until v0.1 commands land.

use std::sync::LazyLock;

use clap::Parser;
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
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
