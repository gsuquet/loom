//! Guard: every test that starts `loom` or `loomd` gives it a temporary `HOME`, so that no
//! test inherits the real home (`loomd` scans `~/perso` and `~/work` at start).
//!
//! A source check over the integration tests of `loom` and `loomd`: a file that runs one of
//! the binaries must set `HOME` through `isolation::isolated_env` or `.env("HOME", …)`. The
//! runtime side is checked by `started_loomd_scans_only_the_test_home` (`status.rs`) and
//! `scan_roots_are_under_the_test_home` (`loomd/tests/daemon.rs`).

use std::fs;
use std::path::Path;

/// Ways a test names the `loom` or `loomd` binary.
const SPAWNS: [&str; 4] = [
    "cargo_bin!(\"loom",
    "CARGO_BIN_EXE_loom",
    ".join(\"loom\")",
    "fn loom_bin(",
];

/// Ways a test sets `HOME` for what it starts.
const ISOLATES: [&str; 3] = [
    "isolation::isolated_env(",
    ".env(\"HOME\"",
    "env!(\"CARGO_TARGET_TMPDIR\")",
];

#[test]
fn every_test_that_starts_a_binary_isolates_home() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    let mut checked = 0;
    for dir in [manifest.join("tests"), manifest.join("../loomd/tests")] {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            if !SPAWNS.iter().any(|spawn| text.contains(spawn)) {
                continue;
            }
            checked += 1;
            if !ISOLATES.iter().any(|isolate| text.contains(isolate)) {
                offenders.push(path.display().to_string());
            }
        }
    }
    assert!(
        checked >= 8,
        "only {checked} test files start a binary: is the check stale?"
    );
    assert!(
        offenders.is_empty(),
        "these tests start loom or loomd without a temporary HOME: {offenders:?}"
    );
}
