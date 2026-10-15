//! The timer of the archive passes (ADR-0013, decision 10): asks the task thread for a pass
//! every [`ARCHIVE_INTERVAL`]. The pass at start is queued by the task store itself, right after
//! the reconciliation on start; what a pass archives is decided in `loom_store` and
//! `loomd::task_archive`.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use loomd::task_archive::ARCHIVE_INTERVAL;
use miette::{Context as _, IntoDiagnostic as _, Result};
use tracing::warn;

/// The running timer.
pub struct Archiver {
    stop: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

/// Starts the timer: `request` asks for a pass, `false` once the task thread is gone (the timer
/// then ends).
pub fn start(request: impl Fn() -> bool + Send + 'static) -> Result<Archiver> {
    start_every(ARCHIVE_INTERVAL, request)
}

fn start_every(
    interval: Duration,
    request: impl Fn() -> bool + Send + 'static,
) -> Result<Archiver> {
    let (stop, stopped) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("task-archive".to_owned())
        .spawn(move || {
            loop {
                match stopped.recv_timeout(interval) {
                    Err(RecvTimeoutError::Timeout) => {
                        if !request() {
                            return;
                        }
                    }
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .into_diagnostic()
        .wrap_err("cannot start the task archive timer")?;
    Ok(Archiver { stop, thread })
}

impl Archiver {
    /// Stops the timer and waits for it.
    pub fn stop(self) {
        let _ = self.stop.send(());
        if self.thread.join().is_err() {
            warn!("the task archive timer panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;

    #[test]
    fn the_timer_asks_for_passes_until_stopped() {
        let count = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&count);
        let archiver = start_every(Duration::from_millis(5), move || {
            counted.fetch_add(1, Ordering::SeqCst);
            true
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while count.load(Ordering::SeqCst) < 2 {
            assert!(Instant::now() < deadline, "no pass requested");
            thread::sleep(Duration::from_millis(5));
        }
        archiver.stop();
        let after = count.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(count.load(Ordering::SeqCst), after);
    }

    #[test]
    fn the_timer_ends_with_the_task_thread() {
        let archiver = start_every(Duration::from_millis(1), || false).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !archiver.thread.is_finished() {
            assert!(Instant::now() < deadline, "the timer did not end");
            thread::sleep(Duration::from_millis(5));
        }
        archiver.stop();
    }
}
