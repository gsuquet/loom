//! Log level on stderr, from the global flags (ADR-0010).

use tracing::level_filters::LevelFilter;

/// Most verbose level logged: `-q` errors only, `info` by default, `-v` debug, `-vv` trace.
/// Same rule as `loomd`.
#[must_use]
pub const fn level(quiet: bool, verbose: u8) -> LevelFilter {
    match (quiet, verbose) {
        (true, _) => LevelFilter::ERROR,
        (false, 0) => LevelFilter::INFO,
        (false, 1) => LevelFilter::DEBUG,
        (false, _) => LevelFilter::TRACE,
    }
}

#[cfg(test)]
mod tests {
    use super::level;
    use tracing::level_filters::LevelFilter;

    #[test]
    fn level_follows_quiet_and_verbose() {
        assert_eq!(level(true, 0), LevelFilter::ERROR);
        assert_eq!(level(false, 0), LevelFilter::INFO);
        assert_eq!(level(false, 1), LevelFilter::DEBUG);
        assert_eq!(level(false, 2), LevelFilter::TRACE);
        assert_eq!(level(false, 9), LevelFilter::TRACE);
    }
}
