//! GitHub REST API client for releases (ADR-0009), blocking, over HTTPS with rustls and the
//! operating system's trust store.

use std::ffi::OsStr;
use std::time::Duration;

use ureq::Agent;

use crate::release::Release;
use crate::tls::tls_config;

/// Default API base URL.
pub const GITHUB_API: &str = "https://api.github.com";
/// Repository releases are published in.
pub const REPOSITORY: &str = "gsuquet/loom";
/// Environment variable replacing [`GITHUB_API`] (test hook).
pub const RELEASES_URL_ENV: &str = "LOOM_RELEASES_URL";
/// Token variables, in resolution order.
pub const TOKEN_ENVS: [&str; 2] = ["LOOM_GITHUB_TOKEN", "GITHUB_TOKEN"];

/// Largest asset accepted, in bytes.
const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
/// Largest JSON response accepted, in bytes.
const MAX_JSON_BYTES: u64 = 16 * 1024 * 1024;
/// Whole-request timeout, download included.
const TIMEOUT: Duration = Duration::from_secs(300);

/// API base URL: `releases_url` (`LOOM_RELEASES_URL`) when set and non-empty, otherwise
/// [`GITHUB_API`], without a trailing `/`. Pure.
#[must_use]
pub fn base_url(releases_url: Option<&str>) -> String {
    releases_url
        .filter(|url| !url.is_empty())
        .unwrap_or(GITHUB_API)
        .trim_end_matches('/')
        .to_owned()
}

/// Token to authenticate with, if any. Pure.
///
/// Order: `LOOM_GITHUB_TOKEN`, `GITHUB_TOKEN`, then `gh_token` (`gh auth token`), called only
/// when `LOOM_RELEASES_URL` is unset so that a github.com credential never goes elsewhere.
#[must_use]
pub fn resolve_token(
    loom_token: Option<&OsStr>,
    github_token: Option<&OsStr>,
    releases_url_set: bool,
    gh_token: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let from_env = |value: Option<&OsStr>| {
        value
            .and_then(OsStr::to_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
    };
    from_env(loom_token)
        .or_else(|| from_env(github_token))
        .or_else(|| {
            if releases_url_set {
                None
            } else {
                gh_token()
                    .map(|token| token.trim().to_owned())
                    .filter(|token| !token.is_empty())
            }
        })
}

/// Why a request failed.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// 404.
    #[error("{what} not found ({url})")]
    NotFound {
        /// What was requested.
        what: String,
        /// URL, without credentials.
        url: String,
    },
    /// 401, 403 or 429.
    #[error(
        "HTTP {status} while fetching {what} ({url}): access denied or API rate limit exceeded; set LOOM_GITHUB_TOKEN or GITHUB_TOKEN"
    )]
    Denied {
        /// Status code.
        status: u16,
        /// What was requested.
        what: String,
        /// URL.
        url: String,
    },
    /// Any other status.
    #[error("HTTP {status} while fetching {what} ({url})")]
    Status {
        /// Status code.
        status: u16,
        /// What was requested.
        what: String,
        /// URL.
        url: String,
    },
    /// Connection, TLS, protocol or size error.
    #[error("cannot fetch {what} ({url}): {source}")]
    Transport {
        /// What was requested.
        what: String,
        /// URL.
        url: String,
        /// Cause.
        #[source]
        source: Box<ureq::Error>,
    },
    /// The response is not the expected JSON.
    #[error("unexpected response for {what} ({url}): {source}")]
    Json {
        /// What was requested.
        what: String,
        /// URL.
        url: String,
        /// Cause.
        #[source]
        source: serde_json::Error,
    },
}

/// Client of the releases API of [`REPOSITORY`].
pub struct Client {
    agent: Agent,
    base: String,
    token: Option<String>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the token.
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Client for the API at `base` (see [`base_url`]). Against [`GITHUB_API`], only HTTPS is
    /// used, redirects included; a test server set with `LOOM_RELEASES_URL` may use plain HTTP.
    /// The token is never sent after a redirect (ureq's default). Server certificates are
    /// verified against the operating system's trust store (`rustls-platform-verifier`), so a
    /// TLS-inspecting proxy whose root the system trusts is accepted.
    #[must_use]
    pub fn new(base: String, token: Option<String>, user_agent: &str) -> Self {
        let agent: Agent = Agent::config_builder()
            .tls_config(tls_config())
            .https_only(base == GITHUB_API)
            .timeout_global(Some(TIMEOUT))
            .user_agent(user_agent)
            .build()
            .into();
        Self { agent, base, token }
    }

    fn releases_url(&self) -> String {
        format!("{}/repos/{REPOSITORY}/releases", self.base)
    }

    /// `GET /releases/latest`: the latest stable release.
    ///
    /// # Errors
    ///
    /// [`HttpError`].
    pub fn latest(&self) -> Result<Release, HttpError> {
        self.json(
            &format!("{}/latest", self.releases_url()),
            "the latest release",
        )
    }

    /// `GET /releases/tags/<tag>`, `None` on 404.
    ///
    /// # Errors
    ///
    /// [`HttpError`] other than not found.
    pub fn by_tag(&self, tag: &str) -> Result<Option<Release>, HttpError> {
        match self.json(
            &format!("{}/tags/{tag}", self.releases_url()),
            &format!("release {tag}"),
        ) {
            Ok(release) => Ok(Some(release)),
            Err(HttpError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// `GET /releases?per_page=30`: the 30 most recent releases.
    ///
    /// # Errors
    ///
    /// [`HttpError`].
    pub fn list(&self) -> Result<Vec<Release>, HttpError> {
        self.json(
            &format!("{}?per_page=30", self.releases_url()),
            "the release list",
        )
    }

    /// `GET /releases/assets/<id>` with `Accept: application/octet-stream`, `name` for messages.
    ///
    /// # Errors
    ///
    /// [`HttpError`].
    pub fn asset(&self, id: u64, name: &str) -> Result<Vec<u8>, HttpError> {
        let url = format!("{}/assets/{id}", self.releases_url());
        let mut response = self.get(&url, "application/octet-stream", name)?;
        response
            .body_mut()
            .with_config()
            .limit(MAX_ASSET_BYTES)
            .read_to_vec()
            .map_err(|error| transport(name, &url, error))
    }

    fn json<T: serde::de::DeserializeOwned>(&self, url: &str, what: &str) -> Result<T, HttpError> {
        let mut response = self.get(url, "application/vnd.github+json", what)?;
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_JSON_BYTES)
            .read_to_vec()
            .map_err(|error| transport(what, url, error))?;
        serde_json::from_slice(&body).map_err(|source| HttpError::Json {
            what: what.to_owned(),
            url: url.to_owned(),
            source,
        })
    }

    fn get(
        &self,
        url: &str,
        accept: &str,
        what: &str,
    ) -> Result<ureq::http::Response<ureq::Body>, HttpError> {
        let mut request = self
            .agent
            .get(url)
            .header("Accept", accept)
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.token {
            request = request.header("Authorization", &format!("Bearer {token}"));
        }
        request.call().map_err(|error| match error {
            ureq::Error::StatusCode(404) => HttpError::NotFound {
                what: what.to_owned(),
                url: url.to_owned(),
            },
            ureq::Error::StatusCode(status @ (401 | 403 | 429)) => HttpError::Denied {
                status,
                what: what.to_owned(),
                url: url.to_owned(),
            },
            ureq::Error::StatusCode(status) => HttpError::Status {
                status,
                what: what.to_owned(),
                url: url.to_owned(),
            },
            other => transport(what, url, other),
        })
    }
}

fn transport(what: &str, url: &str, error: ureq::Error) -> HttpError {
    HttpError::Transport {
        what: what.to_owned(),
        url: url.to_owned(),
        source: Box::new(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_defaults_to_github_and_drops_trailing_slashes() {
        assert_eq!(base_url(None), GITHUB_API);
        assert_eq!(base_url(Some("")), GITHUB_API);
        assert_eq!(
            base_url(Some("http://127.0.0.1:8080/")),
            "http://127.0.0.1:8080"
        );
    }

    #[allow(clippy::unnecessary_wraps)] // Matches `env::var_os`.
    fn os(s: &str) -> Option<&OsStr> {
        Some(OsStr::new(s))
    }

    #[test]
    fn token_resolution_order() {
        let never = || -> Option<String> { panic!("gh must not run") };
        assert_eq!(
            resolve_token(os("loom"), os("gh"), false, never),
            Some("loom".to_owned())
        );
        assert_eq!(
            resolve_token(os(" "), os("github"), false, never),
            Some("github".to_owned())
        );
        assert_eq!(
            resolve_token(None, None, false, || Some("from-gh\n".to_owned())),
            Some("from-gh".to_owned())
        );
        assert_eq!(resolve_token(None, os(""), false, || None), None);
    }

    #[test]
    fn gh_is_not_asked_for_another_server() {
        let never = || -> Option<String> { panic!("gh must not run") };
        assert_eq!(resolve_token(None, None, true, never), None);
        assert_eq!(
            resolve_token(None, os("explicit"), true, never),
            Some("explicit".to_owned())
        );
    }

    #[test]
    fn debug_hides_the_token() {
        let client = Client::new(GITHUB_API.to_owned(), Some("s3cr3t".to_owned()), "loom");
        let text = format!("{client:?}");
        assert!(!text.contains("s3cr3t"), "{text}");
    }
}
