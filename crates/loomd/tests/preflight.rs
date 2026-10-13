//! The pre-flight check (ADR-0011) on the real `loomd`: its repository scan stays in the
//! throwaway directory.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::time::Duration;

use loom_api::VersionInfo;
use loom_update::preflight::{HOME_PREFIX, check};
use semver::Version;

#[test]
fn preflight_loomd_scans_only_the_throwaway_directory() {
    let staged = tempfile::tempdir().unwrap();
    let capture = staged.path().join("stderr");
    // `exec` keeps one process, killed by the pre-flight; stderr goes to `capture`, which
    // outlives the throwaway directory.
    let wrapper = staged.path().join("loomd");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec '{}' \"$@\" 2>'{}'\n",
            env!("CARGO_BIN_EXE_loomd"),
            capture.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();

    let version = Version::parse(&VersionInfo::current().version).unwrap();
    check(staged.path(), &version, Duration::from_secs(60)).unwrap();

    let log = fs::read_to_string(&capture).unwrap();
    let line = log
        .lines()
        .find(|line| line.contains("repository index started"))
        .unwrap_or_else(|| panic!("{log}"));
    let (_, rest) = line.split_once("roots=[").unwrap();
    let (list, _) = rest.split_once(']').unwrap();
    let roots: Vec<PathBuf> = list
        .split(", ")
        .map(|item| PathBuf::from(item.trim_matches('"')))
        .collect();
    assert!(!roots.is_empty(), "{line}");
    for root in &roots {
        assert!(
            root.to_string_lossy().contains(HOME_PREFIX),
            "scan root {} is outside the throwaway directory",
            root.display()
        );
    }
}
