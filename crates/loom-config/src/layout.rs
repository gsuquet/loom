//! Files of the daemon under the loom home directory, and the checks made before using them.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Mode of `run/`: owner only.
pub const RUN_DIR_MODE: u32 = 0o700;
/// Mode of `run/loomd.lock` and `run/loomd.sock`: owner read and write.
pub const RUN_FILE_MODE: u32 = 0o600;
/// Mode of `log/`: owner only.
pub const LOG_DIR_MODE: u32 = 0o700;
/// Mode of `log/loomd.log`: owner read and write.
pub const LOG_FILE_MODE: u32 = 0o600;

/// Longest unix socket path, in bytes, that can be bound: `sun_path` holds 108 bytes on Linux
/// and 104 on macOS and the BSDs, including the terminating NUL.
pub const MAX_SOCKET_PATH_BYTES: usize = if cfg!(any(target_os = "linux", target_os = "android")) {
    107
} else {
    103
};

/// Paths the daemon uses, derived from the loom home directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Loom home directory.
    pub home: PathBuf,
    /// `config.toml`.
    pub config: PathBuf,
    /// `run/`: runtime files of the daemon.
    pub run_dir: PathBuf,
    /// `run/loomd.lock`: single-instance lock, holds the PID of the running daemon.
    pub lock: PathBuf,
    /// `run/loomd.sock`: unix socket of the API.
    pub socket: PathBuf,
    /// `log/`: logs of the daemon.
    pub log_dir: PathBuf,
    /// `log/loomd.log`: stderr of a `loomd` started by `loom`, appended across starts.
    pub daemon_log: PathBuf,
    /// `versions/`: previous versions of `loom` and `loomd` kept by `loom upgrade` and
    /// `loom rollback`, one `versions/<version>/` directory each.
    pub versions_dir: PathBuf,
    /// `staging/`: downloads being verified and unpacked by `loom upgrade`.
    pub staging_dir: PathBuf,
}

impl Layout {
    /// Paths under `home`.
    #[must_use]
    pub fn new(home: PathBuf) -> Self {
        let run_dir = home.join("run");
        let log_dir = home.join("log");
        Self {
            daemon_log: log_dir.join("loomd.log"),
            log_dir,
            config: home.join("config.toml"),
            lock: run_dir.join("loomd.lock"),
            socket: run_dir.join("loomd.sock"),
            versions_dir: home.join("versions"),
            staging_dir: home.join("staging"),
            run_dir,
            home,
        }
    }
}

/// A socket path too long to bind.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "socket path {} is {len} bytes long, the limit on this system is {max}: use a shorter loom home directory (--loom-home or LOOM_HOME)",
    path.display()
)]
pub struct SocketPathTooLong {
    /// The socket path.
    pub path: PathBuf,
    /// Its length in bytes.
    pub len: usize,
    /// The limit.
    pub max: usize,
}

/// Checks that `path` is at most `max` bytes long.
///
/// # Errors
///
/// [`SocketPathTooLong`] when it is longer.
pub fn check_socket_path(path: &Path, max: usize) -> Result<(), SocketPathTooLong> {
    let len = path.as_os_str().len();
    if len > max {
        return Err(SocketPathTooLong {
            path: path.to_path_buf(),
            len,
            max,
        });
    }
    Ok(())
}

/// PID written in the lock file by the daemon holding it.
#[must_use]
pub fn parse_lock_pid(text: &str) -> Option<u32> {
    text.trim().parse().ok().filter(|pid| *pid != 0)
}

/// Message of the error raised when another daemon holds the lock.
#[must_use]
pub fn lock_taken_message(lock: &Path, holder: Option<u32>) -> String {
    let holder = holder.map_or_else(String::new, |pid| format!(" (PID {pid})"));
    format!(
        "another loomd is running{holder}: {} is locked",
        lock.display()
    )
}

#[cfg(test)]
mod tests {
    use super::{
        Layout, MAX_SOCKET_PATH_BYTES, check_socket_path, lock_taken_message, parse_lock_pid,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn layout_lives_under_home() {
        let layout = Layout::new(PathBuf::from("/home/me/.loom"));
        assert_eq!(layout.home, Path::new("/home/me/.loom"));
        assert_eq!(layout.config, Path::new("/home/me/.loom/config.toml"));
        assert_eq!(layout.run_dir, Path::new("/home/me/.loom/run"));
        assert_eq!(layout.lock, Path::new("/home/me/.loom/run/loomd.lock"));
        assert_eq!(layout.socket, Path::new("/home/me/.loom/run/loomd.sock"));
        assert_eq!(layout.log_dir, Path::new("/home/me/.loom/log"));
        assert_eq!(layout.daemon_log, Path::new("/home/me/.loom/log/loomd.log"));
        assert_eq!(layout.versions_dir, Path::new("/home/me/.loom/versions"));
        assert_eq!(layout.staging_dir, Path::new("/home/me/.loom/staging"));
    }

    #[test]
    fn socket_path_limit_is_inclusive() {
        let at_limit = PathBuf::from(format!("/{}", "a".repeat(9)));
        assert_eq!(check_socket_path(&at_limit, 10), Ok(()));

        let too_long = PathBuf::from(format!("/{}", "a".repeat(10)));
        let error = check_socket_path(&too_long, 10).unwrap_err();
        assert_eq!((error.len, error.max), (11, 10));
        let message = error.to_string();
        assert!(message.contains(&*too_long.to_string_lossy()), "{message}");
        assert!(message.contains("LOOM_HOME"), "{message}");
    }

    #[test]
    fn socket_path_limit_matches_the_platform() {
        assert!(matches!(MAX_SOCKET_PATH_BYTES, 103 | 107));
    }

    #[test]
    fn lock_pid_is_parsed_leniently() {
        assert_eq!(parse_lock_pid("4242\n"), Some(4242));
        assert_eq!(parse_lock_pid(" 7 "), Some(7));
        assert_eq!(parse_lock_pid(""), None);
        assert_eq!(parse_lock_pid("0"), None);
        assert_eq!(parse_lock_pid("abc"), None);
        assert_eq!(parse_lock_pid("-1"), None);
    }

    #[test]
    fn lock_taken_message_names_the_lock_and_the_holder() {
        let lock = Path::new("/home/me/.loom/run/loomd.lock");
        assert_eq!(
            lock_taken_message(lock, Some(4242)),
            "another loomd is running (PID 4242): /home/me/.loom/run/loomd.lock is locked"
        );
        assert_eq!(
            lock_taken_message(lock, None),
            "another loomd is running: /home/me/.loom/run/loomd.lock is locked"
        );
    }
}
