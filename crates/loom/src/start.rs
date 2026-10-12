//! Starting `loomd` in the background when it is not running.

use std::env;
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use loom::loomd_path::loomd_candidates;
use loom_api::StatusResponse;
use loom_config::home::LOOM_HOME_ENV;
use loom_config::layout::{LOG_DIR_MODE, LOG_FILE_MODE, Layout};
use miette::{Context as _, IntoDiagnostic as _, Result, miette};
use tracing::{debug, info};

use crate::client::{Client, ClientError};

/// Longest wait for a started `loomd` to answer.
pub const START_TIMEOUT: Duration = Duration::from_secs(5);
/// Interval between two attempts to query a starting `loomd`.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Lines of the log shown when `loomd` fails to start.
const LOG_TAIL_LINES: usize = 5;
/// `loomd` exit code when it fails, including when another `loomd` holds the lock.
const LOOMD_FAILURE: i32 = 1;

/// Starts `loomd` detached, its stderr appended to `log/loomd.log`, and waits until it answers
/// on the socket of `client`.
pub async fn start(layout: &Layout, client: &Client) -> Result<StatusResponse> {
    let loomd = find_loomd()?;
    let mut log = open_log(layout)?;
    // Only what this start writes is shown on failure.
    let log_start = log
        .seek(SeekFrom::End(0))
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot read {}", layout.daemon_log.display()))?;
    info!(
        loomd = %loomd.display(),
        log = %layout.daemon_log.display(),
        "loomd is not running, starting it"
    );
    let mut child = Command::new(&loomd)
        .env(LOOM_HOME_ENV, &layout.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        // Own process group: a Ctrl-C in the terminal of `loom` does not reach `loomd`.
        .process_group(0)
        .spawn()
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot start {}", loomd.display()))?;
    debug!(pid = child.id(), "loomd spawned");

    let deadline = Instant::now() + START_TIMEOUT;
    let mut exited: Option<ExitStatus> = None;
    loop {
        if exited.is_none() {
            exited = child
                .try_wait()
                .into_diagnostic()
                .wrap_err("cannot check whether loomd is running")?;
        }
        // Exit code 1 may mean that another `loomd` won the lock: it may still answer.
        if let Some(status) = exited.filter(|status| status.code() != Some(LOOMD_FAILURE)) {
            return Err(start_failed(layout, log_start, &exited_reason(status)));
        }
        match client.status().await {
            Ok(status) => return Ok(status),
            Err(ClientError::Unreachable { .. }) => {}
            Err(error) => return Err(miette!("loomd started but {error}")),
        }
        if Instant::now() >= deadline {
            let reason = exited.map_or_else(
                || {
                    format!(
                        "loomd did not answer on {} within {} s",
                        client.socket().display(),
                        START_TIMEOUT.as_secs()
                    )
                },
                exited_reason,
            );
            return Err(start_failed(layout, log_start, &reason));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Why `loomd` stopped: its exit code, or the signal that killed it.
fn exited_reason(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("loomd exited with code {code}"),
        (None, Some(signal)) => format!("loomd was killed by signal {signal}"),
        (None, None) => format!("loomd exited with {status}"),
    }
}

/// First `loomd` candidate that is a file: next to the `loom` executable, then in `PATH`.
fn find_loomd() -> Result<PathBuf> {
    let exe = env::current_exe()
        .into_diagnostic()
        .wrap_err("cannot find the path of the loom executable")?;
    let candidates = loomd_candidates(&exe, env::var_os("PATH").as_deref());
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            miette!(
                "cannot start loomd: no loomd next to {} or in PATH; install loomd next to loom",
                exe.display()
            )
        })
}

/// Opens `log/loomd.log` for appending, creating `log/` (mode 0700) and the file (mode 0600).
fn open_log(layout: &Layout) -> Result<File> {
    DirBuilder::new()
        .recursive(true)
        .mode(LOG_DIR_MODE)
        .create(&layout.log_dir)
        .and_then(|()| fs::set_permissions(&layout.log_dir, Permissions::from_mode(LOG_DIR_MODE)))
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot create {}", layout.log_dir.display()))?;
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(LOG_FILE_MODE)
        .open(&layout.daemon_log)
        .and_then(|file| {
            file.set_permissions(Permissions::from_mode(LOG_FILE_MODE))
                .map(|()| file)
        })
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot open {}", layout.daemon_log.display()))
}

/// Error of a failed start: the reason, the log path and the last lines this start logged.
fn start_failed(layout: &Layout, log_start: u64, reason: &str) -> miette::Report {
    let log = &layout.daemon_log;
    let tail = match read_from(log, log_start) {
        Ok(text) => last_lines(&text, LOG_TAIL_LINES),
        Err(error) => format!("(cannot read the log: {error})"),
    };
    if tail.is_empty() {
        miette!(
            "cannot start loomd: {reason}; it logged nothing in {}",
            log.display()
        )
    } else {
        miette!(
            "cannot start loomd: {reason}; last lines of {}:\n{tail}",
            log.display()
        )
    }
}

/// Content of `path` from byte `offset` on, invalid UTF-8 replaced.
fn read_from(path: &Path, offset: u64) -> io::Result<String> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The last `count` non-blank lines of `text`, each indented by two spaces.
fn last_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(count)..]
        .iter()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::last_lines;

    #[test]
    fn last_lines_keeps_the_end_and_skips_blank_lines() {
        assert_eq!(last_lines("", 5), "");
        assert_eq!(last_lines("a\n\nb\n", 5), "  a\n  b");
        assert_eq!(
            last_lines("1\n2\n3\n4\n5\n6\n7\n", 5),
            "  3\n  4\n  5\n  6\n  7"
        );
    }
}
