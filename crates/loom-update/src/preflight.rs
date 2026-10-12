//! Pre-flight check: the staged `loomd` must start in a throwaway loom home and report its
//! version on `/v1/status` before anything installed is replaced (ADR-0011).

use std::fs::{self, File};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use semver::Version;
use tempfile::TempDir;

use crate::health;

/// Prefix of the throwaway loom homes, under `/tmp`.
pub const HOME_PREFIX: &str = "loom-preflight-";
/// Variables the staged `loomd` does not inherit: it must not check for updates itself.
const REMOVED_ENV: [&str; 2] = ["LOOM_HOME", "LOOM_UPDATE_CHECK_INTERVAL"];
const POLL: Duration = Duration::from_millis(100);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Starts `<staged_dir>/loomd` with a throwaway `--loom-home`, waits up to `timeout` for its
/// status, checks it reports `expected`, then kills it and removes the home.
///
/// # Errors
///
/// Why the staged loomd is rejected: it cannot start, exits, does not answer in time, or
/// reports another version. Its last stderr lines are included.
pub fn check(staged_dir: &Path, expected: &Version, timeout: Duration) -> Result<(), String> {
    let home = throwaway_home().map_err(|error| format!("cannot create a loom home: {error}"))?;
    let log_path = home.path().join("loomd.stderr");
    let log = File::create(&log_path)
        .map_err(|error| format!("cannot create {}: {error}", log_path.display()))?;
    let loomd = staged_dir.join("loomd");
    let mut command = Command::new(&loomd);
    command
        .arg("--loom-home")
        .arg(home.path().join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    for name in REMOVED_ENV {
        command.env_remove(name);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start {}: {error}", loomd.display()))?;
    let result = wait(
        &mut child,
        &home.path().join("home/run/loomd.sock"),
        expected,
        timeout,
    );
    // SIGKILL: the home is thrown away, nothing to shut down gracefully.
    let _ = child.kill();
    let _ = child.wait();
    result.map_err(|reason| with_log(&reason, &log_path))
}

fn wait(
    child: &mut Child,
    socket: &Path,
    expected: &Version,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("the staged loomd exited ({status})"));
        }
        if let Ok(status) = health::status(socket, REQUEST_TIMEOUT) {
            return if status.daemon.version == expected.to_string() {
                Ok(())
            } else {
                Err(format!(
                    "the staged loomd reports version {}, expected {expected}",
                    status.daemon.version
                ))
            };
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the staged loomd did not answer within {} s",
                timeout.as_secs()
            ));
        }
        thread::sleep(POLL);
    }
}

/// A new directory under `/tmp`, short enough for a socket path; under the system temporary
/// directory when `/tmp` is missing.
fn throwaway_home() -> std::io::Result<TempDir> {
    let builder = {
        let mut builder = tempfile::Builder::new();
        builder.prefix(HOME_PREFIX);
        builder
    };
    builder.tempdir_in("/tmp").or_else(|_| builder.tempdir())
}

/// `reason`, then the last lines the staged loomd wrote on stderr.
fn with_log(reason: &str, log: &Path) -> String {
    let text = fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = &lines[lines.len().saturating_sub(5)..];
    if tail.is_empty() {
        reason.to_owned()
    } else {
        format!("{reason}: {}", tail.join(" | "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn loomd(dir: &Path, body: &str) {
        let path = dir.join("loomd");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn a_loomd_that_exits_is_rejected_with_its_stderr() {
        let dir = tempfile::tempdir().unwrap();
        loomd(dir.path(), "echo 'Error: boom' >&2; exit 1");
        let error = check(dir.path(), &Version::new(1, 0, 0), Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("exited"), "{error}");
        assert!(error.contains("Error: boom"), "{error}");
    }

    #[test]
    fn a_silent_loomd_times_out() {
        let dir = tempfile::tempdir().unwrap();
        loomd(dir.path(), "exec sleep 30");
        let started = Instant::now();
        let error = check(
            dir.path(),
            &Version::new(1, 0, 0),
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(error.contains("did not answer"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "killed, not awaited"
        );
    }

    #[test]
    fn a_missing_loomd_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let error = check(dir.path(), &Version::new(1, 0, 0), Duration::from_secs(1)).unwrap_err();
        assert!(error.contains("cannot start"), "{error}");
    }

    #[test]
    fn the_home_is_short_and_thrown_away() {
        let home = throwaway_home().unwrap();
        let path = home.path().to_path_buf();
        assert!(
            path.join("home/run/loomd.sock").as_os_str().len() < 100,
            "{}",
            path.display()
        );
        drop(home);
        assert!(!path.exists());
    }
}
