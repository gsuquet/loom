//! Starting a program that was just written.

use std::io;
use std::thread;
use std::time::Duration;

/// Attempts before giving up on a busy executable.
const BUSY_ATTEMPTS: u32 = 10;
/// Pause after the first busy attempt; the n-th retry waits n times as long.
const BUSY_PAUSE: Duration = Duration::from_millis(20);

/// Runs `start`, retrying while the program is busy (`ETXTBSY`, `Text file busy`).
///
/// On Linux, a file still open for writing in any process cannot be executed. A thread that
/// forks while another thread writes a file passes that descriptor to its child until the child
/// calls `exec`, so a program written by this process can be busy for a moment after it is
/// closed. Waiting a few milliseconds is enough.
///
/// # Errors
///
/// The error of the last attempt, or the first error that is not `ExecutableFileBusy`.
pub fn retry_busy<T>(start: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry_busy_with(start, thread::sleep)
}

fn retry_busy_with<T>(
    mut start: impl FnMut() -> io::Result<T>,
    mut pause: impl FnMut(Duration),
) -> io::Result<T> {
    let mut attempt = 1;
    loop {
        match start() {
            Err(error)
                if error.kind() == io::ErrorKind::ExecutableFileBusy && attempt < BUSY_ATTEMPTS =>
            {
                pause(BUSY_PAUSE * attempt);
                attempt += 1;
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy() -> io::Error {
        io::Error::from(io::ErrorKind::ExecutableFileBusy)
    }

    #[test]
    fn a_busy_program_is_started_once_it_is_free() {
        let mut calls = 0;
        let mut pauses = Vec::new();
        let result = retry_busy_with(
            || {
                calls += 1;
                if calls < 3 { Err(busy()) } else { Ok(calls) }
            },
            |pause| pauses.push(pause),
        );
        assert_eq!(result.unwrap(), 3);
        assert_eq!(pauses, [BUSY_PAUSE, BUSY_PAUSE * 2]);
    }

    #[test]
    fn a_program_busy_for_too_long_returns_the_busy_error() {
        let mut calls = 0;
        let result: io::Result<()> = retry_busy_with(
            || {
                calls += 1;
                Err(busy())
            },
            |_| {},
        );
        assert_eq!(
            result.unwrap_err().kind(),
            io::ErrorKind::ExecutableFileBusy
        );
        assert_eq!(calls, BUSY_ATTEMPTS);
    }

    #[test]
    fn other_errors_are_not_retried() {
        let mut calls = 0;
        let result: io::Result<()> = retry_busy_with(
            || {
                calls += 1;
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            },
            |_| panic!("no pause expected"),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls, 1);
    }
}
