//! Responses of the API, decided without I/O. The HTTP wiring lives in the binary.

use std::time::{SystemTime, UNIX_EPOCH};

use loom_api::{ApiError, ErrorCode, StatusResponse, Timestamp};

/// Path of the status endpoint.
pub const STATUS_PATH: &str = "/v1/status";

/// Whole seconds since the Unix epoch of `time`; `None` before the epoch or after year 9999.
#[must_use]
pub fn timestamp(time: SystemTime) -> Option<Timestamp> {
    let secs = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Timestamp::from_unix_secs(secs)
}

/// `base` with `uptime_secs` computed at `now`. A clock set back before the start gives 0.
#[must_use]
pub fn status_at(base: &StatusResponse, now: Timestamp) -> StatusResponse {
    StatusResponse {
        uptime_secs: now.unix_secs().saturating_sub(base.started_at.unix_secs()),
        ..base.clone()
    }
}

/// Error for a path no endpoint serves (HTTP 404).
#[must_use]
pub fn not_found(path: &str) -> ApiError {
    ApiError::new(ErrorCode::NotFound, format!("no endpoint at {path}"))
}

/// Error for a method an existing endpoint does not accept (HTTP 405).
#[must_use]
pub fn method_not_allowed(method: &str, path: &str) -> ApiError {
    ApiError::new(
        ErrorCode::InvalidRequest,
        format!("method {method} is not allowed on {path}"),
    )
}

/// Error for a clock the daemon cannot read as a [`Timestamp`] (HTTP 500).
#[must_use]
pub fn clock_error() -> ApiError {
    ApiError::new(
        ErrorCode::Internal,
        "the system clock is outside the range of timestamps",
    )
}

#[cfg(test)]
mod tests {
    use super::{method_not_allowed, not_found, status_at, timestamp};
    use loom_api::{ApiVersion, ErrorCode, StatusResponse, Timestamp, VersionInfo};
    use std::time::{Duration, UNIX_EPOCH};

    fn at(secs: u64) -> Timestamp {
        Timestamp::from_unix_secs(secs).unwrap()
    }

    fn base() -> StatusResponse {
        StatusResponse {
            api_version: ApiVersion::CURRENT,
            daemon: VersionInfo::current(),
            pid: 42,
            started_at: at(1_000),
            uptime_secs: 0,
            loom_home: "/l".into(),
            socket_path: "/l/run/loomd.sock".into(),
            config_path: "/l/config.toml".into(),
            update: None,
        }
    }

    #[test]
    fn uptime_is_whole_seconds_since_start() {
        let status = status_at(&base(), at(1_090));
        assert_eq!(status.uptime_secs, 90);
        assert_eq!(
            StatusResponse {
                uptime_secs: 0,
                ..status
            },
            base()
        );
    }

    #[test]
    fn clock_set_back_gives_zero_uptime() {
        assert_eq!(status_at(&base(), at(10)).uptime_secs, 0);
    }

    #[test]
    fn timestamp_truncates_to_whole_seconds() {
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_millis(1_999)),
            Some(at(1))
        );
        assert_eq!(timestamp(UNIX_EPOCH - Duration::from_secs(1)), None);
    }

    #[test]
    fn errors_have_the_right_codes() {
        let error = not_found("/v1/nope");
        assert_eq!(error.code, ErrorCode::NotFound);
        assert!(error.message.contains("/v1/nope"));

        let error = method_not_allowed("POST", "/v1/status");
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert!(error.message.contains("POST"));
        assert!(error.message.contains("/v1/status"));
    }
}
