//! Disk usage of the loom home end to end: a temporary loom home of known content, the
//! `disk` object of `GET /v1/status`.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use loom_api::{ContextDiskUsage, DiskUsage, StatusResponse};
use tempfile::TempDir;

mod common;

use common::{Daemon, ok, request};

/// A loom home under `/tmp`: the macOS default `$TMPDIR` is too long for a socket path.
fn short_home() -> TempDir {
    tempfile::Builder::new()
        .prefix("loomd-disk-")
        .tempdir_in("/tmp")
        .unwrap()
}

/// `loomd` on `home`, with `HOME` set to it too, so that the repository scan never walks the
/// real home directory.
fn loomd(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
    command
        .env("LOOM_HOME", home)
        .env("HOME", home)
        .arg("--color=never")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn disk(home: &Path) -> DiskUsage {
    let status: StatusResponse = ok(&request(
        &home.join("run/loomd.sock"),
        "GET",
        "/v1/status",
        None,
    ));
    status.disk.unwrap()
}

/// Allocated bytes of each path, as `lstat` reports them.
fn allocated(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .map(|path| fs::symlink_metadata(path).unwrap().blocks() * 512)
        .sum()
}

fn write(path: &Path, len: usize) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![b'x'; len]).unwrap();
    path.to_path_buf()
}

#[test]
fn status_reports_the_allocated_bytes_of_each_part() {
    let home = short_home();
    let root = home.path();
    let outside = TempDir::new().unwrap();
    let big = write(&outside.path().join("big"), 5_000_000);

    let versions = root.join("versions");
    let v1 = versions.join("0.1.0");
    let v2 = versions.join("0.2.0");
    let v1_loom = write(&v1.join("loom"), 100_000);
    let v1_loomd = write(&v1.join("loomd"), 50_000);
    let v2_loom = write(&v2.join("loom"), 120_000);
    // A second link to a file already counted: counted once.
    fs::hard_link(&v1_loomd, v2.join("loomd")).unwrap();
    // A symbolic link out of the home: the link is counted, never its target.
    let link = v1.join("big");
    symlink(&big, &link).unwrap();

    let contexts = root.join("contexts");
    let personal = contexts.join("personal");
    let task = write(&personal.join("tasks/k3q7zm2a/task.md"), 3_000);
    let work = contexts.join("work");
    let notes = write(&work.join("notes.md"), 10_000);
    let secret = work.join("secret");
    write(&secret.join("hidden"), 70_000);
    // A file directly in `contexts/` is not a store.
    write(&contexts.join("README"), 10);

    let expected_versions = allocated(&[versions, v1, v2, v1_loom, v1_loomd, v2_loom, link]);
    let expected_personal = allocated(&[
        personal.clone(),
        personal.join("tasks"),
        personal.join("tasks/k3q7zm2a"),
        task,
    ]);
    // The content of the unreadable directory counts as 0.
    let expected_work = allocated(&[work, notes, secret.clone()]);

    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();
    // True when running as root: the directory is then measured like any other.
    let readable = fs::read_dir(&secret).is_ok();
    let daemon = Daemon::start(&mut loomd(root), &root.join("run/loomd.sock"));
    let usage = disk(root);
    daemon.stop();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(usage.versions_bytes, expected_versions);
    assert!(usage.versions_bytes < 5_000_000, "{usage:?}");
    assert_eq!(usage.kept_versions, ["0.1.0", "0.2.0"]);
    assert_eq!(
        usage.contexts[0],
        ContextDiskUsage {
            name: "personal".to_owned(),
            bytes: expected_personal
        }
    );
    assert_eq!(usage.contexts[1].name, "work");
    assert_eq!(usage.contexts.len(), 2);
    if !readable {
        assert_eq!(usage.contexts[1].bytes, expected_work);
        assert!(
            usage
                .warnings
                .iter()
                .any(|warning| warning.starts_with(&format!("cannot read {}", secret.display()))),
            "{:?}",
            usage.warnings
        );
    }
    // loomd wrote its own files (log, run, index) before answering: present, not exact.
    assert!(usage.index_bytes >= usage.index_wal_bytes);
    assert_eq!(
        usage.total_bytes,
        usage.versions_bytes
            + usage.index_bytes
            + usage.logs_bytes
            + usage.state_bytes
            + usage.other_bytes
            + usage.contexts_bytes()
    );
    assert!(usage.other_bytes >= allocated(&[root.to_path_buf(), contexts.join("README")]));
    assert!(
        usage.available_bytes.is_some_and(|bytes| bytes > 0),
        "{usage:?}"
    );
}

#[test]
fn a_home_without_versions_or_contexts_has_no_warning() {
    let home = short_home();
    let root = home.path();
    let daemon = Daemon::start(&mut loomd(root), &root.join("run/loomd.sock"));
    let usage = disk(root);
    daemon.stop();
    assert_eq!(usage.versions_bytes, 0);
    assert_eq!(usage.kept_versions, Vec::<String>::new());
    assert_eq!(usage.contexts, []);
    assert_eq!(usage.warnings, Vec::<String>::new());
    assert!(usage.total_bytes > 0, "run/ and the home exist");
}

#[test]
fn the_measure_is_reused_until_the_kept_versions_change() {
    let home = short_home();
    let root = home.path();
    write(&root.join("versions/0.1.0/loom"), 1_000);
    let daemon = Daemon::start(&mut loomd(root), &root.join("run/loomd.sock"));
    let first = disk(root);

    // Within the cache lifetime, a new context store is not seen yet.
    write(&root.join("contexts/personal/notes.md"), 50_000);
    assert_eq!(disk(root), first);

    // An upgrade saves a version: measured again.
    write(&root.join("versions/0.2.0/loom"), 1_000);
    let second = disk(root);
    daemon.stop();
    assert_eq!(second.kept_versions, ["0.1.0", "0.2.0"]);
    assert_eq!(second.contexts.len(), 1);
    assert!(second.versions_bytes > first.versions_bytes);
}
