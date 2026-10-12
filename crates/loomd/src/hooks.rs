//! Test hooks of the auto-upgrade (ADR-0011), honoured by debug builds only. The binary reads
//! the environment; these functions decide.

use std::path::Path;
use std::time::Duration;

use loom_config::duration::parse_duration;

/// Replaces `[update] check_interval`, without its minimum.
pub const CHECK_INTERVAL_ENV: &str = "LOOM_UPDATE_CHECK_INTERVAL";
/// Replaces the watchdog deadline.
pub const HEALTH_DEADLINE_ENV: &str = "LOOM_UPDATE_HEALTH_DEADLINE";
/// loomd fails at start unless its loom home starts with this prefix.
pub const FAIL_UNLESS_HOME_PREFIX_ENV: &str = "LOOM_TEST_FAIL_UNLESS_HOME_PREFIX";

/// The duration of a hook variable: `None` when unset or empty, in a release build, or
/// unparsable (`Err` with a message to log).
///
/// # Errors
///
/// A message naming the variable when the value is not a duration.
pub fn duration_hook(name: &str, value: Option<&str>) -> Result<Option<Duration>, String> {
    match value.filter(|value| !value.is_empty()) {
        Some(value) if cfg!(debug_assertions) => parse_duration(value)
            .map(Some)
            .map_err(|error| format!("{name} ignored: {error}")),
        _ => Ok(None),
    }
}

/// Error message when the hook prefix is set (debug build) and `home` does not start with it.
#[must_use]
pub fn fail_unless_home_prefix(prefix: Option<&str>, home: &Path) -> Option<String> {
    let prefix = prefix.filter(|prefix| !prefix.is_empty() && cfg!(debug_assertions))?;
    if home.to_string_lossy().starts_with(prefix) {
        None
    } else {
        Some(format!(
            "test hook {FAIL_UNLESS_HOME_PREFIX_ENV}: the loom home {} does not start with {prefix}",
            home.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{duration_hook, fail_unless_home_prefix};
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn duration_hook_parses_in_debug_builds() {
        assert_eq!(duration_hook("X", None), Ok(None));
        assert_eq!(duration_hook("X", Some("")), Ok(None));
        if cfg!(debug_assertions) {
            assert_eq!(
                duration_hook("X", Some("2s")),
                Ok(Some(Duration::from_secs(2)))
            );
            let error = duration_hook("X", Some("soon")).unwrap_err();
            assert!(error.starts_with("X ignored: invalid duration"), "{error}");
        } else {
            assert_eq!(duration_hook("X", Some("2s")), Ok(None));
        }
    }

    #[test]
    fn home_prefix_hook() {
        let home = Path::new("/tmp/loom-preflight-abc/home");
        assert_eq!(fail_unless_home_prefix(None, home), None);
        assert_eq!(fail_unless_home_prefix(Some(""), home), None);
        assert_eq!(
            fail_unless_home_prefix(Some("/tmp/loom-preflight-"), home),
            None
        );
        let failure = fail_unless_home_prefix(Some("/tmp/loom-preflight-"), Path::new("/h"));
        assert_eq!(failure.is_some(), cfg!(debug_assertions));
    }
}
