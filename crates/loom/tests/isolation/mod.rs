//! Every `loom` or `loomd` a test starts gets a temporary `HOME` and `XDG_CONFIG_HOME`, never
//! the real ones: `loomd` scans `~/perso`, `~/work` and the context paths at start, and `loom`
//! resolves the loom home and the service files from `HOME`.

#![allow(dead_code, reason = "each test crate uses part of the helpers")]

use std::path::{Path, PathBuf};

/// `HOME` and `XDG_CONFIG_HOME` under `dir`, for `Command::envs`.
pub fn isolated_env(dir: &Path) -> [(&'static str, PathBuf); 2] {
    [
        ("HOME", dir.to_path_buf()),
        ("XDG_CONFIG_HOME", dir.join(".config")),
    ]
}

/// A user home for the tests that need no directory of their own: under the target directory
/// of the integration tests, outside the real home.
pub fn shared_home() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("isolated-home")
}

/// The scan roots `loomd` logged at start (`repository index started roots=[...]`) in `log`.
///
/// # Panics
///
/// When `log` has no such line.
pub fn logged_scan_roots(log: &str) -> Vec<PathBuf> {
    let line = log
        .lines()
        .find(|line| line.contains("repository index started"))
        .unwrap_or_else(|| panic!("no `repository index started` line in:\n{log}"));
    let list = line
        .split_once("roots=[")
        .and_then(|(_, rest)| rest.split_once(']'))
        .map_or_else(|| panic!("no roots in {line}"), |(list, _)| list);
    list.split(", ")
        .filter(|item| !item.is_empty())
        .map(|item| PathBuf::from(item.trim_matches('"')))
        .collect()
}

/// Fails when a scan root is under `real_home` (the `HOME` of the test process) without being
/// under `sandbox`.
pub fn assert_isolated(roots: &[PathBuf], sandbox: &Path) {
    assert!(!roots.is_empty(), "no scan roots");
    let real_home = std::env::var_os("HOME").map(PathBuf::from);
    for root in roots {
        assert!(
            root.starts_with(sandbox),
            "{} is outside {}",
            root.display(),
            sandbox.display()
        );
        if let Some(real_home) = &real_home {
            assert!(
                !root.starts_with(real_home) || sandbox.starts_with(real_home),
                "scan root {} is under the real home {}",
                root.display(),
                real_home.display()
            );
        }
    }
}
