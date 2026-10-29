//! Test hooks of loomd (auto-upgrade, ADR-0011; event heartbeat; schedule interval; forge API), honoured
//! by debug builds only. The binary reads the environment; these functions decide.

use std::path::Path;
use std::time::Duration;

use loom_config::duration::parse_duration;

/// Replaces `[update] check_interval`, without its minimum.
pub const CHECK_INTERVAL_ENV: &str = "LOOM_UPDATE_CHECK_INTERVAL";
/// Replaces the watchdog deadline.
pub const HEALTH_DEADLINE_ENV: &str = "LOOM_UPDATE_HEALTH_DEADLINE";
/// Replaces the 15 s heartbeat of `GET /v1/events`.
pub const EVENTS_HEARTBEAT_ENV: &str = "LOOM_TEST_EVENTS_HEARTBEAT";
/// Replaces the 1 min interval between two checks of the schedules.
pub const SCHEDULE_INTERVAL_ENV: &str = "LOOM_TEST_SCHEDULE_INTERVAL";
/// Replaces the 10 s that API requests in flight have to finish after the first signal.
pub const SHUTDOWN_GRACE_ENV: &str = "LOOM_TEST_SHUTDOWN_GRACE";
/// Replaces the 30 s between the start of loomd and the first poll of each forge host.
pub const REVIEW_START_DELAY_ENV: &str = "LOOM_TEST_REVIEW_START_DELAY";
/// loomd fails at start unless its loom home starts with this prefix.
pub const FAIL_UNLESS_HOME_PREFIX_ENV: &str = "LOOM_TEST_FAIL_UNLESS_HOME_PREFIX";
/// Replaces the API of forge hosts.
///
/// The value is the base URL of a fake forge for every host, or
/// `<host>=<base>[,<host>=<base>…]`, such as `github.com=http://127.0.0.1:8080`. The GitHub
/// GraphQL endpoint is `<base>/graphql`.
pub const FORGE_API_ENV: &str = "LOOM_TEST_FORGE_API";

/// The API base of `host` in the hook variable: `None` when unset or empty, when a list does
/// not name the host, or in a release build.
#[must_use]
pub fn forge_api_hook(value: Option<&str>, host: &str) -> Option<String> {
    let value = value.filter(|value| !value.is_empty() && cfg!(debug_assertions))?;
    let base = if value.contains('=') {
        value
            .split(',')
            .filter_map(|entry| entry.split_once('='))
            .find(|(name, _)| name.trim() == host)?
            .1
    } else {
        value
    };
    Some(base.trim().trim_end_matches('/').to_owned())
}

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
    use super::{duration_hook, fail_unless_home_prefix, forge_api_hook};
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
    fn forge_api_hook_in_debug_builds() {
        assert_eq!(forge_api_hook(None, "github.com"), None);
        assert_eq!(forge_api_hook(Some(""), "github.com"), None);
        let one = forge_api_hook(Some("http://127.0.0.1:9/"), "github.com");
        let list = "github.com=http://127.0.0.1:1/, gitlab.example.com=http://127.0.0.1:2,bad";
        if cfg!(debug_assertions) {
            assert_eq!(one.as_deref(), Some("http://127.0.0.1:9"));
            assert_eq!(
                forge_api_hook(Some(list), "github.com").as_deref(),
                Some("http://127.0.0.1:1")
            );
            assert_eq!(
                forge_api_hook(Some(list), "gitlab.example.com").as_deref(),
                Some("http://127.0.0.1:2")
            );
            assert_eq!(forge_api_hook(Some(list), "gitlab.com"), None);
        } else {
            assert_eq!(one, None);
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
