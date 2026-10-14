//! `loom service restart` end to end without a service: a temporary `HOME` (so no service file,
//! and no `launchctl` or `systemctl` command is ever run), a temporary `LOOM_HOME` and the
//! `loomd` built next to `loom`.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{DaemonRestartReport, RestartVia, VersionInfo};
use tempfile::TempDir;

mod isolation;

const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);

fn loom_bin() -> PathBuf {
    PathBuf::from(assert_cmd::cargo::cargo_bin!("loom"))
}

/// A loom home under `/tmp`: the macOS default `$TMPDIR` is too long for a socket path.
fn short_home() -> TempDir {
    let loomd = loom_bin().with_file_name("loomd");
    assert!(
        loomd.is_file(),
        "{} is missing: build the workspace (`cargo test --workspace`)",
        loomd.display()
    );
    tempfile::Builder::new()
        .prefix("loom-")
        .tempdir_in("/tmp")
        .unwrap()
}

fn lock(home: &Path) -> PathBuf {
    home.join("run/loomd.lock")
}

/// Runs `loom --color=never <args>` with `LOOM_HOME` and `HOME` set to `home`.
fn loom(home: &Path, args: &[&str]) -> Output {
    Command::new(loom_bin())
        .current_dir(home)
        .env("LOOM_HOME", home)
        .envs(isolation::isolated_env(home))
        .env_remove("NO_COLOR")
        .arg("--color=never")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn restart_json(home: &Path) -> DaemonRestartReport {
    let output = loom(home, &["service", "restart", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Whether a process holds the lock of `home`.
fn lock_held(home: &Path) -> bool {
    File::open(lock(home)).is_ok_and(|file| file.try_lock().is_err())
}

/// PID written in the lock by the running daemon.
fn lock_pid(home: &Path) -> u32 {
    fs::read_to_string(lock(home))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Whether the process `pid` exists.
fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// Stops, when dropped, the daemon a test started in `home`.
struct DaemonGuard<'a> {
    home: &'a Path,
}

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        if !lock_held(self.home) {
            return;
        }
        let Some(pid) = fs::read_to_string(lock(self.home))
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        else {
            return;
        };
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
        // Waits for the lock to be released, so the temporary home is removed after loomd.
        let deadline = Instant::now() + TIMEOUT;
        while lock_held(self.home) && Instant::now() < deadline {
            thread::sleep(POLL);
        }
    }
}

#[test]
fn restart_starts_a_stopped_loomd_then_replaces_a_running_one() {
    let home = short_home();
    let _guard = DaemonGuard { home: home.path() };

    let started = restart_json(home.path());
    assert!(!started.was_running);
    assert_eq!(started.previous_pid, None);
    assert_eq!(started.via, RestartVia::Start);
    assert_eq!(started.version, VersionInfo::current().version);
    assert_eq!(started.pid, lock_pid(home.path()));

    let restarted = restart_json(home.path());
    assert!(restarted.was_running);
    assert_eq!(restarted.previous_pid, Some(started.pid));
    assert_eq!(restarted.via, RestartVia::Signal);
    assert_ne!(restarted.pid, started.pid);
    assert_eq!(restarted.pid, lock_pid(home.path()));
    assert!(!alive(started.pid), "the old loomd still runs");

    let text = loom(home.path(), &["service", "restart"]);
    assert_eq!(text.status.code(), Some(0), "{}", stderr(&text));
    let line = String::from_utf8(text.stdout).unwrap();
    assert!(
        line.starts_with(&format!("loomd restarted (pid {} -> ", restarted.pid)),
        "{line}"
    );
    assert!(line.contains(", via signal) in "), "{line}");
}

#[test]
fn an_invalid_configuration_leaves_loomd_running() {
    let home = short_home();
    let _guard = DaemonGuard { home: home.path() };
    let pid = restart_json(home.path()).pid;

    fs::write(home.path().join("config.toml"), "[repos\n").unwrap();
    let output = loom(home.path(), &["service", "restart"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("config.toml"),
        "{}",
        stderr(&output)
    );
    assert!(lock_held(home.path()));
    assert_eq!(lock_pid(home.path()), pid);
    assert!(alive(pid));
}

#[test]
fn a_loomd_that_ignores_sigterm_fails_the_restart_without_sigkill() {
    let home = short_home();
    fs::create_dir_all(home.path().join("run")).unwrap();
    // Stands for a loomd that ignores SIGTERM: the test holds the lock, the PID in it is a
    // shell that ignores the signal (and so does its `sleep`).
    let mut stubborn = Command::new("sh")
        .args(["-c", "trap '' TERM; sleep 30"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let held = File::create(lock(home.path())).unwrap();
    held.lock().unwrap();
    fs::write(lock(home.path()), format!("{}\n", stubborn.id())).unwrap();

    let started = Instant::now();
    let output = loom(home.path(), &["service", "restart"]);
    let waited = started.elapsed();
    let alive_after = alive(stubborn.id());
    let _ = stubborn.kill();
    let _ = stubborn.wait();

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(
        message.contains(&format!(
            "loomd (pid {}) did not exit within 10 s",
            stubborn.id()
        )),
        "{message}"
    );
    assert!(message.contains("loomd.log"), "{message}");
    assert!(waited >= Duration::from_secs(10), "{waited:?}");
    assert!(alive_after, "SIGKILL was sent");
    drop(held);
}
