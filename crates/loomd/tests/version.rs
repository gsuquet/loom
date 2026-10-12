//! `loomd --version` end to end.

use assert_cmd::Command;
use loom_api::VersionInfo;

fn loomd() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("loomd"))
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
