//! Terminal raw-mode and alternate-screen guard with panic restoration.
//!
//! Ensures the terminal leaves the alternate screen, shows the cursor and disables raw mode on
//! normal exit (`Drop`) and on panic (via a panic hook installed while the TUI is active).
//! Reinstalls any previous panic hook on drop.

use std::io::{self, Write};
use std::panic;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

/// Writes the escape sequences that enter the alternate screen and hide the cursor to `out`.
///
/// # Errors
///
/// Returns any write error from `out`.
pub fn write_enter_sequences<W: Write>(out: &mut W) -> io::Result<()> {
    execute!(out, EnterAlternateScreen, Hide)
}

/// Writes the escape sequences that show the cursor and leave the alternate screen to `out`.
///
/// # Errors
///
/// Returns any write error from `out`.
pub fn write_restore_sequences<W: Write>(out: &mut W) -> io::Result<()> {
    execute!(out, Show, LeaveAlternateScreen)?;
    out.flush()
}

type PanicHook = Arc<dyn Fn(&panic::PanicHookInfo<'_>) + Send + Sync>;

/// Guard that restores the terminal on drop and installs a panic hook so a panic restores the
/// terminal before printing the panic message. Reinstalls the previous panic hook on drop.
pub struct TerminalGuard {
    restored: Arc<AtomicBool>,
    restore_fn: Arc<dyn Fn() + Send + Sync>,
    prev_hook: Option<PanicHook>,
}

impl TerminalGuard {
    /// Enters raw mode and the alternate screen on standard output, and installs a panic hook
    /// that restores both before delegating to the previous panic hook.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if raw mode or the alternate screen cannot be enabled.
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = write_enter_sequences(&mut stdout) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self::with_restorer(|| {
            let _ = write_restore_sequences(&mut io::stdout());
            let _ = disable_raw_mode();
        }))
    }

    /// Creates a guard with a custom restoration callback (used by tests to verify panic
    /// restoration without requiring a controlling TTY).
    pub fn with_restorer<F>(restore: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        let restored = Arc::new(AtomicBool::new(false));
        let restore_fn: Arc<dyn Fn() + Send + Sync> = Arc::new(restore);

        let hook_restored = Arc::clone(&restored);
        let hook_fn = Arc::clone(&restore_fn);
        let prev_hook = panic::take_hook();
        let prev_arc: Arc<dyn Fn(&panic::PanicHookInfo<'_>) + Send + Sync> = Arc::from(prev_hook);
        let hook_prev = Arc::clone(&prev_arc);

        panic::set_hook(Box::new(move |info| {
            if !hook_restored.swap(true, Ordering::SeqCst) {
                hook_fn();
            }
            hook_prev(info);
        }));

        Self {
            restored,
            restore_fn,
            prev_hook: Some(prev_arc),
        }
    }

    /// Restores the terminal immediately (idempotent).
    pub fn restore(&self) {
        if !self.restored.swap(true, Ordering::SeqCst) {
            (self.restore_fn)();
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
        if let Some(prev) = self.prev_hook.take() {
            panic::set_hook(Box::new(move |info| (prev)(info)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn terminal_is_restored_after_a_forced_panic() {
        let output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let raw_disabled = Arc::new(AtomicBool::new(false));

        let out_clone = Arc::clone(&output);
        let raw_clone = Arc::clone(&raw_disabled);

        let guard = TerminalGuard::with_restorer(move || {
            if let Ok(mut buf) = out_clone.lock() {
                let _ = write_restore_sequences(&mut *buf);
            }
            raw_clone.store(true, Ordering::SeqCst);
        });

        let unwind = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let _active = &guard;
            panic!("forced panic to test terminal restoration");
        }));
        assert!(unwind.is_err());

        // The panic hook must have already run the restore callback before catch_unwind returned.
        assert!(raw_disabled.load(Ordering::SeqCst));
        let bytes = output.lock().unwrap().clone();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("\x1b[?1049l"),
            "missing LeaveAlternateScreen in {text:?}"
        );
        assert!(
            text.contains("\x1b[?25h"),
            "missing Show cursor in {text:?}"
        );

        // Dropping the guard restores any previous hook and is idempotent.
        drop(guard);
        assert_eq!(output.lock().unwrap().len(), bytes.len());
    }
}
