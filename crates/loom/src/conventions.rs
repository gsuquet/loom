//! Process conventions shared by every command (ADR-0010): colors,
//! confirmations, exit codes, stdout writes.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::ColorChoice;

/// Whether to color a stream.
///
/// `auto` colors only a terminal, and only when `NO_COLOR` is unset or empty.
/// `always` and `never` ignore both.
#[must_use]
pub fn use_color(choice: ColorChoice, no_color: Option<&OsStr>, is_terminal: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => is_terminal && no_color.is_none_or(OsStr::is_empty),
    }
}

/// What a command does when it needs a confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// `-y` was given: go ahead without asking.
    Proceed,
    /// Ask on the terminal.
    Ask,
    /// No terminal to ask on and no `-y`: fail with [`Exit::Failure`] and
    /// [`CONFIRMATION_REFUSED`].
    Refuse,
}

/// Message printed when a confirmation is refused.
pub const CONFIRMATION_REFUSED: &str =
    "confirmation required but stdin is not a terminal; pass -y/--yes to confirm";

/// How to handle a confirmation. `-y` is the only way to skip one.
#[must_use]
pub const fn confirmation(yes: bool, stdin_is_terminal: bool) -> Confirmation {
    match (yes, stdin_is_terminal) {
        (true, _) => Confirmation::Proceed,
        (false, true) => Confirmation::Ask,
        (false, false) => Confirmation::Refuse,
    }
}

/// Exit codes of `loom` and `loomd`. Any other code is a bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The command succeeded.
    Success,
    /// The command failed.
    Failure,
    /// Invalid arguments. `clap` exits with this code itself.
    Usage,
    /// Interrupted by `SIGINT` (128 + 2).
    Interrupted,
}

impl Exit {
    /// Numeric exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Failure => 1,
            Self::Usage => 2,
            Self::Interrupted => 130,
        }
    }
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        Self::from(exit.code())
    }
}

/// Writes `text` to `out` and flushes. A closed reader (`loom … | head`) is
/// not an error: the command ends successfully.
///
/// # Errors
///
/// Any I/O error other than [`io::ErrorKind::BrokenPipe`].
pub fn write_out(out: &mut impl Write, text: &str) -> io::Result<()> {
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::{Confirmation, Exit, confirmation, use_color, write_out};
    use clap::ColorChoice;
    use std::ffi::OsStr;
    use std::io::{self, Write};

    #[test]
    fn color_decision_table() {
        let set = Some(OsStr::new("1"));
        let empty = Some(OsStr::new(""));
        let cases = [
            (ColorChoice::Auto, None, true, true),
            (ColorChoice::Auto, None, false, false),
            (ColorChoice::Auto, set, true, false),
            (ColorChoice::Auto, empty, true, true),
            (ColorChoice::Always, set, false, true),
            (ColorChoice::Never, None, true, false),
        ];
        for (choice, no_color, is_terminal, expected) in cases {
            assert_eq!(
                use_color(choice, no_color, is_terminal),
                expected,
                "{choice:?} NO_COLOR={no_color:?} terminal={is_terminal}"
            );
        }
    }

    #[test]
    fn confirmation_decision_table() {
        assert_eq!(confirmation(true, true), Confirmation::Proceed);
        assert_eq!(confirmation(true, false), Confirmation::Proceed);
        assert_eq!(confirmation(false, true), Confirmation::Ask);
        assert_eq!(confirmation(false, false), Confirmation::Refuse);
    }

    #[test]
    fn refusal_message_names_the_flag() {
        assert!(super::CONFIRMATION_REFUSED.contains("-y/--yes"));
    }

    #[test]
    fn exit_codes() {
        let codes = [Exit::Success, Exit::Failure, Exit::Usage, Exit::Interrupted].map(Exit::code);
        assert_eq!(codes, [0, 1, 2, 130]);
    }

    struct Failing(io::ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(self.0.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn broken_pipe_is_not_an_error() {
        assert!(write_out(&mut Failing(io::ErrorKind::BrokenPipe), "x").is_ok());
    }

    #[test]
    fn other_write_errors_are_reported() {
        let error = write_out(&mut Failing(io::ErrorKind::PermissionDenied), "x").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn writes_the_text() {
        let mut out = Vec::new();
        write_out(&mut out, "hello\n").unwrap();
        assert_eq!(out, b"hello\n");
    }
}
