//! Log level and colors on stderr, from the global flags (ADR-0010). Shared by `loom` and `loomd`.

use std::ffi::OsStr;

use clap::ColorChoice;
use tracing::level_filters::LevelFilter;

/// Most verbose level logged: `-q` errors only, `info` by default, `-v` debug, `-vv` trace.
#[must_use]
pub const fn level(quiet: bool, verbose: u8) -> LevelFilter {
    match (quiet, verbose) {
        (true, _) => LevelFilter::ERROR,
        (false, 0) => LevelFilter::INFO,
        (false, 1) => LevelFilter::DEBUG,
        (false, _) => LevelFilter::TRACE,
    }
}

/// Whether to color a stream: `auto` colors only a terminal, and only when `NO_COLOR` is unset
/// or empty; `always` and `never` ignore both.
#[must_use]
pub fn use_color(choice: ColorChoice, no_color: Option<&OsStr>, is_terminal: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => is_terminal && no_color.is_none_or(OsStr::is_empty),
    }
}

#[cfg(test)]
mod tests {
    use super::{level, use_color};
    use clap::ColorChoice;
    use std::ffi::OsStr;
    use tracing::level_filters::LevelFilter;

    #[test]
    fn level_follows_quiet_and_verbose() {
        assert_eq!(level(true, 0), LevelFilter::ERROR);
        assert_eq!(level(true, 3), LevelFilter::ERROR);
        assert_eq!(level(false, 0), LevelFilter::INFO);
        assert_eq!(level(false, 1), LevelFilter::DEBUG);
        assert_eq!(level(false, 2), LevelFilter::TRACE);
        assert_eq!(level(false, 9), LevelFilter::TRACE);
    }

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
}
