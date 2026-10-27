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
use ratatui::Terminal;
use ratatui::backend::Backend;

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

/// Leaves the alternate screen, then raw mode. If raw mode cannot be left, the screen is
/// entered again and the error returned, so that the terminal stays as it was.
///
/// # Errors
///
/// The first error of the steps.
pub fn suspend_with(
    leave_screen: impl FnOnce() -> io::Result<()>,
    disable_raw: impl FnOnce() -> io::Result<()>,
    enter_screen: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    leave_screen()?;
    if let Err(error) = disable_raw() {
        let _ = enter_screen();
        return Err(error);
    }
    Ok(())
}

/// Enters raw mode, then the alternate screen. If the screen cannot be entered, raw mode is
/// left again and the error returned, so that the terminal is in its normal state.
///
/// # Errors
///
/// The first error of the steps.
pub fn resume_with(
    enable_raw: impl FnOnce() -> io::Result<()>,
    enter_screen: impl FnOnce() -> io::Result<()>,
    disable_raw: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    enable_raw()?;
    if let Err(error) = enter_screen() {
        let _ = disable_raw();
        return Err(error);
    }
    Ok(())
}

/// Why [`while_suspended`] failed.
#[derive(Debug, thiserror::Error)]
pub enum SuspendError {
    /// The terminal could not be given away. The work did not run, and the terminal is as it was.
    #[error("cannot leave the terminal: {0}")]
    Suspend(io::Error),
    /// The terminal could not be taken back. The caller must stop the TUI.
    #[error("cannot take the terminal back: {0}")]
    Resume(io::Error),
}

/// Runs `work` with the terminal given away: `suspend`, then `work`, then `resume`. `resume`
/// runs whatever `work` returns.
///
/// # Errors
///
/// [`SuspendError`] when the terminal cannot be given away or taken back.
pub async fn while_suspended<T>(
    suspend: impl FnOnce() -> io::Result<()>,
    resume: impl FnOnce() -> io::Result<()>,
    work: impl std::future::Future<Output = T>,
) -> Result<T, SuspendError> {
    suspend().map_err(SuspendError::Suspend)?;
    let output = work.await;
    resume().map_err(SuspendError::Resume)?;
    Ok(output)
}

/// Clears the screen and makes the next draw of `terminal` write every cell, for example after
/// the editor used the terminal.
///
/// `Terminal::clear` is not used: it first asks the terminal for the cursor position, and that
/// question gets no answer in time after the editor, so the clear is not done and the next draw
/// writes only the cells that changed. A resize to the current size clears the screen and the
/// back buffer without that question, and also takes a new size of the window.
///
/// # Errors
///
/// The error of the backend when it cannot give the size or clear the screen.
pub fn repaint_all<B: Backend>(terminal: &mut Terminal<B>) -> Result<(), B::Error> {
    let size = terminal.size()?;
    terminal.resize(size.into())
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

    /// Gives the terminal back for a program that needs it, such as the editor: leaves the
    /// alternate screen and raw mode. If it fails half way, it puts the screen back, so that
    /// the TUI can go on. Call [`Self::resume`] when the program ends.
    ///
    /// This does not touch the `restored` flag of the guard, on purpose: the flag says that the
    /// guard has restored the terminal for good (panic hook and drop). A suspension is
    /// temporary, and a panic or a drop while the editor runs must still restore the terminal
    /// (the restore sequences are safe to write twice).
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the terminal cannot be restored.
    pub fn suspend(&self) -> io::Result<()> {
        suspend_with(
            || write_restore_sequences(&mut io::stdout()),
            disable_raw_mode,
            || write_enter_sequences(&mut io::stdout()),
        )
    }

    /// Takes the terminal back after [`Self::suspend`].
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if raw mode or the alternate screen cannot be enabled. The
    /// terminal is then in its normal state.
    pub fn resume(&self) -> io::Result<()> {
        resume_with(
            enable_raw_mode,
            || write_enter_sequences(&mut io::stdout()),
            disable_raw_mode,
        )
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

    use ratatui::backend::{ClearType, TestBackend, WindowSize};
    use ratatui::buffer::Cell;
    use ratatui::layout::{Position, Size};
    use ratatui::widgets::Paragraph;

    /// A [`TestBackend`] that gets no answer when it asks for the cursor position, as the
    /// terminal after the editor.
    struct NoCursorAnswer(TestBackend);

    fn no_answer() -> io::Error {
        io::Error::other("the cursor position could not be read")
    }

    impl Backend for NoCursorAnswer {
        type Error = io::Error;

        fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            self.0.draw(content).map_err(|never| match never {})
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            self.0.hide_cursor().map_err(|never| match never {})
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            self.0.show_cursor().map_err(|never| match never {})
        }

        fn get_cursor_position(&mut self) -> io::Result<Position> {
            Err(no_answer())
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
            self.0
                .set_cursor_position(position)
                .map_err(|never| match never {})
        }

        fn clear(&mut self) -> io::Result<()> {
            Backend::clear(&mut self.0).map_err(|never| match never {})
        }

        fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
            self.0
                .clear_region(clear_type)
                .map_err(|never| match never {})
        }

        fn size(&self) -> io::Result<Size> {
            self.0.size().map_err(|never| match never {})
        }

        fn window_size(&mut self) -> io::Result<WindowSize> {
            self.0.window_size().map_err(|never| match never {})
        }

        fn flush(&mut self) -> io::Result<()> {
            Backend::flush(&mut self.0).map_err(|never| match never {})
        }
    }

    fn draw_page(terminal: &mut Terminal<NoCursorAnswer>) {
        terminal
            .draw(|frame| frame.render_widget(Paragraph::new("task page"), frame.area()))
            .unwrap();
    }

    #[test]
    fn repaint_all_draws_the_whole_screen_again_after_the_editor() {
        let mut terminal = Terminal::new(NoCursorAnswer(TestBackend::new(12, 2))).unwrap();
        draw_page(&mut terminal);
        // The editor leaves a screen that the TUI did not draw.
        Backend::clear(&mut terminal.backend_mut().0).unwrap();

        repaint_all(&mut terminal).unwrap();
        draw_page(&mut terminal);

        terminal
            .backend()
            .0
            .assert_buffer_lines(["task page   ", "            "]);
    }

    #[test]
    fn without_repaint_all_the_next_draw_writes_nothing() {
        let mut terminal = Terminal::new(NoCursorAnswer(TestBackend::new(12, 2))).unwrap();
        draw_page(&mut terminal);
        Backend::clear(&mut terminal.backend_mut().0).unwrap();

        // `Terminal::clear` asks for the cursor position first, gets no answer and does nothing.
        assert!(terminal.clear().is_err());
        draw_page(&mut terminal);

        terminal
            .backend()
            .0
            .assert_buffer_lines(["            ", "            "]);
    }

    #[test]
    fn repaint_all_takes_a_new_size_of_the_window() {
        let mut terminal = Terminal::new(NoCursorAnswer(TestBackend::new(12, 2))).unwrap();
        draw_page(&mut terminal);
        terminal.backend_mut().0.resize(10, 3);

        repaint_all(&mut terminal).unwrap();
        draw_page(&mut terminal);

        terminal
            .backend()
            .0
            .assert_buffer_lines(["task page ", "          ", "          "]);
    }

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
