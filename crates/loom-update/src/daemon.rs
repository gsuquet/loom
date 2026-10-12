//! Restart of a running loomd after its binary was replaced.

use std::fs::{File, TryLockError};
use std::io::{self, Read as _};
use std::path::Path;
use std::process::Command;

use loom_api::DaemonRestart;
use loom_config::layout::parse_lock_pid;

/// Sends `SIGTERM` to the loomd holding `lock`, through the `kill` command.
///
/// loomd runs when another process holds the lock file (`flock`); the PID it wrote in the
/// file is then signalled. A PID left in a lock file nobody holds is stale and is not
/// signalled, so a reused PID is never hit. Checking takes the lock for an instant: a loomd
/// starting at that very moment fails as if another one were running.
///
/// A service manager (launchd `KeepAlive`, systemd `Restart=always`) restarts loomd on the new
/// binary; otherwise the next `loom` command starts it. This function does not wait.
#[must_use]
pub fn restart(lock: &Path) -> DaemonRestart {
    let mut file = match File::open(lock) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return DaemonRestart::NotRunning,
        Err(error) => return failed(None, &format!("cannot open {}: {error}", lock.display())),
    };
    match file.try_lock() {
        // Nobody holds it: no loomd. Dropping the file releases the lock.
        Ok(()) => return DaemonRestart::NotRunning,
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(error)) => {
            return failed(None, &format!("cannot check {}: {error}", lock.display()));
        }
    }
    let mut text = String::new();
    let Some(pid) = file
        .read_to_string(&mut text)
        .ok()
        .and_then(|_| parse_lock_pid(&text))
    else {
        return failed(
            None,
            &format!("loomd holds {} but its PID is not in it", lock.display()),
        );
    };
    match Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .output()
    {
        Ok(output) if output.status.success() => DaemonRestart::Signalled { pid },
        Ok(output) => failed(Some(pid), String::from_utf8_lossy(&output.stderr).trim()),
        Err(error) => failed(Some(pid), &format!("cannot run kill: {error}")),
    }
}

/// Whether a process holds the single-instance lock at `lock`. The probe lock is released at
/// once. A missing file means nobody holds it; any other error counts as held.
#[must_use]
pub fn lock_held(lock: &Path) -> bool {
    let file = match File::open(lock) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(TryLockError::WouldBlock | TryLockError::Error(_)) => true,
    }
}

/// PID written in `lock` by the loomd holding it; `None` when nobody holds it or it has none.
#[must_use]
pub fn holder_pid(lock: &Path) -> Option<u32> {
    if !lock_held(lock) {
        return None;
    }
    std::fs::read_to_string(lock)
        .ok()
        .and_then(|text| parse_lock_pid(&text))
}

fn failed(pid: Option<u32>, message: &str) -> DaemonRestart {
    DaemonRestart::Failed {
        pid,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::process::ExitStatusExt as _;

    #[test]
    fn missing_lock_means_not_running() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            restart(&dir.path().join("loomd.lock")),
            DaemonRestart::NotRunning
        );
    }

    #[test]
    fn unheld_lock_with_a_pid_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("loomd.lock");
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        fs::write(&lock, format!("{}\n", child.id())).unwrap();
        assert_eq!(restart(&lock), DaemonRestart::NotRunning);
        // The process was not signalled.
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn held_lock_signals_the_pid() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("loomd.lock");
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        fs::write(&lock, format!("{}\n", child.id())).unwrap();
        let held = File::open(&lock).unwrap();
        // `flock` locks belong to the open file description: a second `open` in this process
        // conflicts like another process would.
        held.try_lock().unwrap();
        assert_eq!(restart(&lock), DaemonRestart::Signalled { pid: child.id() });
        assert_eq!(child.wait().unwrap().signal(), Some(15));
    }

    #[test]
    fn held_lock_without_pid_fails() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("loomd.lock");
        fs::write(&lock, "").unwrap();
        let held = File::open(&lock).unwrap();
        held.try_lock().unwrap();
        assert!(matches!(
            restart(&lock),
            DaemonRestart::Failed { pid: None, .. }
        ));
    }
}
