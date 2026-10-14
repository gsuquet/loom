//! Responses of the API, decided without I/O. The HTTP wiring lives in the binary.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use loom_api::{
    ApiError, ContextName, DEFAULT_SEARCH_LIMIT, ErrorCode, SshAgentSocket, StatusResponse,
    Timestamp,
};
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
/// Path of the search endpoint.
pub const SEARCH_PATH: &str = "/v1/search";
/// Path of the diagnostics endpoint: the environment of the daemon.
pub const DIAGNOSTICS_PATH: &str = "/v1/diagnostics";
/// Path of the remote check: `git ls-remote` of each remote endpoint of the index.
pub const REMOTE_CHECK_PATH: &str = "/v1/diagnostics/remote-check";
/// Path of the tasks reindex: the tasks index rebuilt from the files of every context store.
pub const TASKS_REINDEX_PATH: &str = "/v1/tasks/reindex";
/// Path of the task list and of task creation.
pub const TASKS_PATH: &str = "/v1/tasks";
/// Route of one task, by ID or ID prefix.
pub const TASK_PATH: &str = "/v1/tasks/{id}";
/// Route of an action on a task.
pub const TASK_ACTION_PATH: &str = "/v1/tasks/{id}/actions/{action}";
/// Route of the event log of a task.
pub const TASK_EVENTS_PATH: &str = "/v1/tasks/{id}/events";
/// Route of the undo of the last change of a task.
pub const TASK_UNDO_PATH: &str = "/v1/tasks/{id}/undo";
/// Path of the backup state of the context stores.
pub const TASKS_BACKUP_PATH: &str = "/v1/tasks/backup";
/// Path of the sync of the context stores with their backup remotes.
pub const TASKS_SYNC_PATH: &str = "/v1/tasks/sync";

/// Query parameters of `POST /v1/repos/scan`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanQuery {
    /// `true`: drop the whole index and rebuild it from the roots alone.
    #[serde(default)]
    pub rebuild: bool,
    /// `true`: compute the changes, apply nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// What `SSH_AUTH_SOCK` (`value`) points to; `kind` tells whether a path is a socket, `None`
/// when nothing can be examined there. An empty value counts as unset.
pub fn agent_socket(
    value: Option<&OsStr>,
    kind: impl FnOnce(&Path) -> Option<bool>,
) -> SshAgentSocket {
    let Some(path) = value.filter(|value| !value.is_empty()).map(PathBuf::from) else {
        return SshAgentSocket::Unset;
    };
    match kind(&path) {
        None => SshAgentSocket::Missing { path },
        Some(false) => SshAgentSocket::NotASocket { path },
        Some(true) => SshAgentSocket::Socket { path },
    }
}

/// `HOME` as reported: `None` when unset or empty.
#[must_use]
pub fn home_value(value: Option<&OsStr>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

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

/// Query parameters of `GET /v1/search`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    /// The query: whitespace-separated terms.
    pub q: String,
    /// Only the repositories of this context (`unassigned` included); ignored when empty.
    pub context: Option<String>,
    /// Most results returned, at least 1; default [`DEFAULT_SEARCH_LIMIT`].
    pub limit: Option<usize>,
}

/// A search request, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// The query.
    pub q: String,
    /// The context to search in.
    pub context: Option<String>,
    /// Most results returned.
    pub limit: usize,
}

/// The request of `query`.
///
/// # Errors
///
/// `invalid_request` when `context` is not a context name or `limit` is 0.
pub fn search_request(query: SearchQuery) -> Result<SearchRequest, ApiError> {
    let (context, _) = repo_filter(RepoQuery {
        context: query.context,
        query: None,
    })?;
    let limit = match query.limit {
        None => DEFAULT_SEARCH_LIMIT,
        Some(0) => return Err(invalid_request("limit must be at least 1")),
        Some(limit) => limit,
    };
    Ok(SearchRequest {
        q: query.q,
        context,
        limit,
    })
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
        RepoQuery, ScanQuery, SearchQuery, SearchRequest, agent_socket, home_value,
        method_not_allowed, not_found, register_path, repo_filter, repo_not_found, search_request,
        slug, status_at, timestamp,
    };
    use loom_api::{
        ApiVersion, DEFAULT_SEARCH_LIMIT, ErrorCode, SshAgentSocket, StatusResponse, Timestamp,
        VersionInfo,
    };
    use std::ffi::OsStr;
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
    fn search_request_defaults_and_checks() {
        let query = |context: Option<&str>, limit: Option<usize>| SearchQuery {
            q: "lo om".to_owned(),
            context: context.map(str::to_owned),
            limit,
        };
        assert_eq!(
            search_request(query(None, None)).unwrap(),
            SearchRequest {
                q: "lo om".to_owned(),
                context: None,
                limit: DEFAULT_SEARCH_LIMIT,
            }
        );
        assert_eq!(
            search_request(query(Some(""), Some(3))).unwrap(),
            SearchRequest {
                q: "lo om".to_owned(),
                context: None,
                limit: 3,
            }
        );
        assert_eq!(
            search_request(query(Some("work"), Some(1)))
                .unwrap()
                .context
                .as_deref(),
            Some("work")
        );
        for bad in [query(Some("Bad/ctx"), None), query(None, Some(0))] {
            assert_eq!(
                search_request(bad).unwrap_err().code,
                ErrorCode::InvalidRequest
            );
        }
        let parse = |text: &str| serde_json::from_str::<SearchQuery>(text);
        assert!(parse("{}").is_err(), "q is required");
        assert!(parse(r#"{"q": "x", "sort": "name"}"#).is_err());
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
    fn agent_socket_states() {
        let socket =
            |value: Option<&str>, kind: Option<bool>| agent_socket(value.map(OsStr::new), |_| kind);
        assert_eq!(socket(None, Some(true)), SshAgentSocket::Unset);
        assert_eq!(socket(Some(""), Some(true)), SshAgentSocket::Unset);
        assert_eq!(
            socket(Some("/a"), None),
            SshAgentSocket::Missing { path: "/a".into() }
        );
        assert_eq!(
            socket(Some("/a"), Some(false)),
            SshAgentSocket::NotASocket { path: "/a".into() }
        );
        assert_eq!(
            socket(Some("/a"), Some(true)),
            SshAgentSocket::Socket { path: "/a".into() }
        );
        assert_eq!(home_value(Some(OsStr::new(""))), None);
        assert_eq!(home_value(None), None);
        assert_eq!(
            home_value(Some(OsStr::new("/home/me"))),
            Some(PathBuf::from("/home/me"))
        );
    }

    #[test]
    fn scan_query_defaults_and_rejects_unknown_parameters() {
        let parse = |text: &str| serde_json::from_str::<ScanQuery>(text);
        assert_eq!(parse("{}").unwrap(), ScanQuery::default());
        assert!(
            parse(r#"{"rebuild": true, "dry_run": true}"#)
                .unwrap()
                .rebuild
        );
        assert!(parse(r#"{"force": true}"#).is_err());
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
            disk: None,
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
