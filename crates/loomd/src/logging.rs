//! Log level and colors on stderr, from the global flags (ADR-0010).

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

/// Whether to color stderr: `auto` colors only a terminal, and only when `NO_COLOR` is unset
/// or empty; `always` and `never` ignore both. Same rule as `loom`.
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
        assert_eq!(level(false, 0), LevelFilter::INFO);
        assert_eq!(level(false, 1), LevelFilter::DEBUG);
        assert_eq!(level(false, 2), LevelFilter::TRACE);
        assert_eq!(level(false, 9), LevelFilter::TRACE);
    }

    #[test]
    fn color_rule() {
        let set = Some(OsStr::new("1"));
        let empty = Some(OsStr::new(""));
        assert!(use_color(ColorChoice::Auto, None, true));
        assert!(use_color(ColorChoice::Auto, empty, true));
        assert!(!use_color(ColorChoice::Auto, set, true));
        assert!(!use_color(ColorChoice::Auto, None, false));
        assert!(use_color(ColorChoice::Always, set, false));
        assert!(!use_color(ColorChoice::Never, None, true));
    }
}
