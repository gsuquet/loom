//! Responses of the API, decided without I/O. The HTTP wiring lives in the binary.

use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use loom_api::{ApiError, ContextName, ErrorCode, StatusResponse, Timestamp};
use serde::Deserialize;

/// Path of the status endpoint.
pub const STATUS_PATH: &str = "/v1/status";
/// Path of the repository list.
pub const REPOS_PATH: &str = "/v1/repos";
/// Path of the scan endpoint.
pub const REPOS_SCAN_PATH: &str = "/v1/repos/scan";
/// Path of the update endpoint.
pub const REPOS_SYNC_PATH: &str = "/v1/repos/sync";
/// Path of the registration endpoint.
pub const REPOS_REGISTER_PATH: &str = "/v1/repos/register";
/// Route of one repository: its slug `<context>/<name>`, as two path segments.
pub const REPO_PATH: &str = "/v1/repos/{context}/{name}";

/// Query parameters of `GET /v1/repos`. Empty values are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoQuery {
    /// Only the repositories of this context (`unassigned` included).
    pub context: Option<String>,
    /// Only the repositories whose name, slug, path, remote URLs or context contain this text,
    /// ASCII case ignored.
    pub query: Option<String>,
}

/// Query parameters of `GET /v1/repos/{context}/{name}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoGetQuery {
    /// `true`: read the repository's state now, then return it.
    #[serde(default)]
    pub refresh: bool,
}

/// The filter of `query`: `(context, text)`, each `None` when absent or empty.
///
/// # Errors
///
/// `invalid_request` when `context` is not a context name.
pub fn repo_filter(query: RepoQuery) -> Result<(Option<String>, Option<String>), ApiError> {
    let context = match query.context.filter(|context| !context.is_empty()) {
        None => None,
        Some(context) => Some(
            context
                .parse::<ContextName>()
                .map_err(|error| invalid_request(format!("invalid context: {error}")))?
                .as_str()
                .to_owned(),
        ),
    };
    Ok((context, query.query.filter(|text| !text.is_empty())))
}

/// The slug addressed by `/v1/repos/{context}/{name}`.
#[must_use]
pub fn slug(context: &str, name: &str) -> String {
    format!("{context}/{name}")
}

/// The path of a registration: absolute, without `.` components or trailing `/`. Symbolic
/// links are kept.
///
/// # Errors
///
/// `invalid_request` for a relative path or one with a `..` component.
pub fn register_path(path: &Path) -> Result<PathBuf, ApiError> {
    if !path.is_absolute() {
        return Err(invalid_request(format!(
            "path {} is not absolute",
            path.display()
        )));
    }
    if path.components().any(|part| part == Component::ParentDir) {
        return Err(invalid_request(format!(
            "path {} has a `..` component",
            path.display()
        )));
    }
    Ok(path.components().collect())
}

/// Error for a slug no repository has (HTTP 404).
#[must_use]
pub fn repo_not_found(slug: &str) -> ApiError {
    ApiError::new(
        ErrorCode::NotFound,
        format!("no repository with slug {slug}"),
    )
}

/// Error for a malformed request (HTTP 400).
#[must_use]
pub fn invalid_request(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::InvalidRequest, message)
}

/// Error for a failure of the daemon on a valid request (HTTP 500).
#[must_use]
pub fn internal(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::Internal, message)
}

/// Error for a request the daemon cannot serve while it stops (HTTP 503).
#[must_use]
pub fn unavailable(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::Unavailable, message)
}

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
    use super::{
        RepoQuery, method_not_allowed, not_found, register_path, repo_filter, repo_not_found, slug,
        status_at, timestamp,
    };
    use loom_api::{ApiVersion, ErrorCode, StatusResponse, Timestamp, VersionInfo};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn repo_filter_ignores_empty_values_and_checks_the_context() {
        let query = |context: Option<&str>, text: Option<&str>| RepoQuery {
            context: context.map(str::to_owned),
            query: text.map(str::to_owned),
        };
        assert_eq!(repo_filter(query(None, None)).unwrap(), (None, None));
        assert_eq!(
            repo_filter(query(Some(""), Some(""))).unwrap(),
            (None, None)
        );
        assert_eq!(
            repo_filter(query(Some("unassigned"), Some("lo om"))).unwrap(),
            (Some("unassigned".to_owned()), Some("lo om".to_owned()))
        );
        let error = repo_filter(query(Some("Bad/ctx"), None)).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn register_path_must_be_absolute_without_parent_components() {
        assert_eq!(
            register_path(Path::new("/home/me/./src/x/")).unwrap(),
            PathBuf::from("/home/me/src/x")
        );
        for bad in ["src/x", "", "/home/me/../x"] {
            let error = register_path(Path::new(bad)).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidRequest, "{bad}");
        }
    }

    #[test]
    fn slug_joins_the_two_segments() {
        assert_eq!(slug("personal", "loom-2"), "personal/loom-2");
        let error = repo_not_found("personal/x");
        assert_eq!(error.code, ErrorCode::NotFound);
        assert!(error.message.contains("personal/x"));
    }

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
