//! `loomd --version` end to end.

use assert_cmd::Command;
use loom_api::VersionInfo;

/// `HOME` under the integration tests' directory: no test inherits the real home.
fn loomd() -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("loomd"));
    command.env("HOME", env!("CARGO_TARGET_TMPDIR"));
    command
}

#[test]
fn long_version_lists_every_field() {
    let expected = format!("loomd {}\n", VersionInfo::current());
    loomd().arg("--version").assert().success().stdout(expected);
}

#[test]
fn short_version_prints_the_version_only() {
    let expected = format!("loomd {}\n", VersionInfo::current().version);
    loomd().arg("-V").assert().success().stdout(expected);
}
