//! The HTTP transport of the forge clients, and the pure decisions on an answer: error
//! variant, rate limit wait, next page.

use std::time::Duration;

use loom_api::{ForgeKind, Timestamp};
use loom_update::tls::tls_config;
use ureq::Agent;
use ureq::config::RedirectAuthHeaders;

use super::{ForgeError, RateLimitWait, Token, sso_url};
use crate::error::{ErrorBody, ParseError};

/// Longest wait for one forge call.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Largest body read: a page of 100 files with their patches.
const MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;
/// Most pages read for one list. GitHub gives at most 3,000 files (30 pages of 100) and 1,000
/// search results.
pub const MAX_PAGES: usize = 100;

/// The headers of an answer that loom reads, by lowercase name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    /// The first value of `name` (any case).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

impl<const N: usize> From<[(&str, &str); N]> for Headers {
    fn from(pairs: [(&str, &str); N]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), (*value).to_owned()))
                .collect(),
        )
    }
}

/// An answer of the forge.
#[derive(Debug)]
pub struct Answer {
    /// The HTTP status.
    pub status: u16,
    /// The headers.
    pub headers: Headers,
    /// The body.
    pub body: Vec<u8>,
}

/// The transport of one forge host: one token, one REST base and an optional GraphQL
/// endpoint. [`Http::send`] refuses any other URL, so the token goes there only; redirects keep
/// it only on the same host.
pub struct Http {
    agent: Agent,
    kind: ForgeKind,
    base: String,
    graphql: Option<String>,
    token: Token,
}

impl std::fmt::Debug for Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Http")
            .field("kind", &self.kind)
            .field("base", &self.base)
            .field("graphql", &self.graphql)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

/// A request body and its content type.
pub struct Body<'a> {
    /// The `Content-Type` header.
    pub content_type: &'static str,
    /// The bytes sent.
    pub bytes: &'a [u8],
}

impl Http {
    /// A transport for the REST API at `base` (no trailing `/`) and the GraphQL endpoint
    /// `graphql`. An `https` base is called over HTTPS only, redirects included.
    pub fn new(
        kind: ForgeKind,
        base: &str,
        graphql: Option<&str>,
        token: Token,
        user_agent: &str,
    ) -> Self {
        let base = base.trim_end_matches('/').to_owned();
        let agent: Agent = Agent::config_builder()
            .tls_config(tls_config())
            .https_only(base.starts_with("https://"))
            .redirect_auth_headers(RedirectAuthHeaders::SameHost)
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .user_agent(user_agent)
            .build()
            .into();
        Self {
            agent,
            kind,
            base,
            graphql: graphql.map(str::to_owned),
            token,
        }
    }

    /// The URL of `path_and_query` on the base.
    pub fn url(&self, path_and_query: &str) -> String {
        format!("{}{path_and_query}", self.base)
    }

    /// True when `url` is under the REST base or is the GraphQL endpoint of this transport,
    /// so the token may go there.
    pub fn allowed(&self, url: &str) -> bool {
        self.graphql.as_deref() == Some(url)
            || url
                .strip_prefix(&self.base)
                .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('?'))
    }

    /// `text` with the token redacted.
    pub fn redact(&self, text: &str) -> String {
        self.token.redact(text)
    }

    /// A [`ParseError`] as a [`ForgeError::Parse`], redacted: a body can echo the token.
    pub fn parse_error(&self, error: &ParseError) -> ForgeError {
        ForgeError::Parse(self.token.redact(&error.to_string()))
    }

    /// `method url` with the token and the headers of the forge, `extra` headers, and `body`.
    /// Any status is an [`Answer`]; a failed transport is an error, and so is a URL that is not
    /// [allowed](Self::allowed), before anything is sent.
    pub fn send(
        &self,
        method: &str,
        url: &str,
        extra: &[(&str, &str)],
        body: Option<Body<'_>>,
    ) -> Result<Answer, ForgeError> {
        if !self.allowed(url) {
            return Err(ForgeError::Unexpected(format!(
                "{} is not on the API of this forge host",
                self.token.redact(url)
            )));
        }
        let request = ureq::http::Request::builder()
            .method(method)
            .uri(url)
            .header(
                "Authorization",
                format!("Bearer {}", self.token.expose()).as_str(),
            );
        let request = match self.kind {
            ForgeKind::Github => request
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28"),
            ForgeKind::Gitlab => request.header("Accept", "application/json"),
        };
        let request = extra.iter().fold(request, |request, (name, value)| {
            request.header(*name, *value)
        });
        let result = match body {
            Some(body) => request
                .header("Content-Type", body.content_type)
                .body(body.bytes.to_vec())
                .map_err(|error| self.transport(url, &error))
                .and_then(|request| {
                    self.agent
                        .run(request)
                        .map_err(|error| self.transport(url, &error))
                }),
            None => request
                .body(())
                .map_err(|error| self.transport(url, &error))
                .and_then(|request| {
                    self.agent
                        .run(request)
                        .map_err(|error| self.transport(url, &error))
                }),
        };
        let mut response = result?;
        let status = response.status().as_u16();
        let headers = Headers(
            response
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .to_str()
                        .ok()
                        .map(|value| (name.as_str().to_owned(), value.to_owned()))
                })
                .collect(),
        );
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_BODY_BYTES)
            .read_to_vec()
            .map_err(|error| self.transport(url, &error))?;
        Ok(Answer {
            status,
            headers,
            body,
        })
    }

    /// The error of an answer with an error status, its texts redacted.
    pub fn error(&self, answer: &Answer) -> ForgeError {
        let body = self.token.redact(&String::from_utf8_lossy(&answer.body));
        classify(self.kind, answer.status, &answer.headers, body.as_bytes())
    }

    fn transport(&self, url: &str, error: &dyn std::fmt::Display) -> ForgeError {
        ForgeError::Transport(self.token.redact(&format!("{url}: {error}")))
    }
}

/// The error of an answer with an error status, by forge. Matches on the status, the headers
/// and, for a GitLab 403, the machine code of the `error` field, never on the message. Pure.
pub fn classify(kind: ForgeKind, status: u16, headers: &Headers, body: &[u8]) -> ForgeError {
    match kind {
        ForgeKind::Github => classify_github(status, headers, body),
        ForgeKind::Gitlab => classify_gitlab(status, headers, body),
    }
}

fn classify_github(status: u16, headers: &Headers, body: &[u8]) -> ForgeError {
    let message = message(body);
    match status {
        401 => ForgeError::Unauthorized { message },
        429 => ForgeError::RateLimited {
            status,
            wait: rate_limit_wait(headers),
            message,
        },
        403 => {
            if let Some(sso) = headers.get("x-github-sso") {
                return ForgeError::SsoRequired { url: sso_url(sso) };
            }
            if headers.get("x-ratelimit-remaining").map(str::trim) == Some("0")
                || headers.get("retry-after").is_some()
            {
                return ForgeError::RateLimited {
                    status,
                    wait: rate_limit_wait(headers),
                    message,
                };
            }
            ForgeError::Forbidden { message }
        }
        404 => ForgeError::NotFound { message },
        422 => ForgeError::Unprocessable {
            details: details(body),
            message,
        },
        status => ForgeError::Status { status, message },
    }
}

/// The OAuth error code of GitLab, such as `insufficient_scope`: a machine code, not a message.
#[derive(serde::Deserialize)]
struct ScopeError {
    error: Option<String>,
    error_description: Option<String>,
    scope: Option<String>,
}

/// The OAuth code of GitLab for a token without the scope of the request.
const INSUFFICIENT_SCOPE: &str = "insufficient_scope";

fn classify_gitlab(status: u16, headers: &Headers, body: &[u8]) -> ForgeError {
    let ErrorBody { message, details } =
        crate::gitlab::parse_error(body).unwrap_or_else(|_| ErrorBody {
            message: message(body),
            details: Vec::new(),
        });
    match status {
        400 => ForgeError::BadRequest { message, details },
        401 => ForgeError::Unauthorized { message },
        403 => match serde_json::from_slice::<ScopeError>(body) {
            Ok(ScopeError {
                error: Some(code),
                error_description,
                scope,
            }) if code == INSUFFICIENT_SCOPE => ForgeError::InsufficientScope {
                scope,
                message: error_description.unwrap_or(code),
            },
            Ok(_) | Err(_) => ForgeError::Forbidden { message },
        },
        404 => ForgeError::NotFound { message },
        409 => ForgeError::Conflict { message },
        422 => ForgeError::Unprocessable { message, details },
        429 => ForgeError::RateLimited {
            status,
            wait: rate_limit_wait(headers),
            message,
        },
        status => ForgeError::Status { status, message },
    }
}

/// `retry-after` (seconds) first, then `x-ratelimit-reset` (GitHub) or `ratelimit-reset`
/// (GitLab), in Unix seconds. Pure.
pub fn rate_limit_wait(headers: &Headers) -> RateLimitWait {
    if let Some(seconds) = headers
        .get("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        return RateLimitWait::After(Duration::from_secs(seconds));
    }
    headers
        .get("x-ratelimit-reset")
        .or_else(|| headers.get("ratelimit-reset"))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .and_then(Timestamp::from_unix_secs)
        .map_or(RateLimitWait::Unknown, RateLimitWait::Until)
}

/// The URL of the `rel="next"` page of a `link` header. Pure.
///
/// Each link is `<url>; param; param`, links are separated by `,`. A URL can hold a `,`, so
/// the URL is read up to its `>` before the parameters are split.
pub fn next_link(link: &str) -> Option<String> {
    let mut rest = link;
    loop {
        let start = rest.find('<')?;
        let after = &rest[start + 1..];
        let end = after.find('>')?;
        let url = &after[..end];
        let tail = &after[end + 1..];
        let params_end = tail.find('<').unwrap_or(tail.len());
        let params = &tail[..params_end];
        let next = params.split([';', ',']).any(|param| {
            param
                .trim()
                .strip_prefix("rel=")
                .is_some_and(|rel| rel.trim_matches('"').split(' ').any(|rel| rel == "next"))
        });
        if next {
            return Some(url.to_owned());
        }
        rest = &tail[params_end..];
    }
}

#[derive(serde::Deserialize)]
struct ErrorFields {
    message: Option<serde_json::Value>,
    error: Option<String>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

/// The message of an error body: `message`, otherwise `error`, otherwise its first 200
/// characters.
fn message(body: &[u8]) -> String {
    if let Ok(error) = serde_json::from_slice::<ErrorFields>(body) {
        match (error.message, error.error) {
            (Some(serde_json::Value::String(message)), _) => return message,
            (Some(message), _) => return message.to_string(),
            (None, Some(error)) => return error,
            (None, None) => {}
        }
    }
    let text = String::from_utf8_lossy(body);
    text.chars().take(200).collect::<String>().trim().to_owned()
}

/// The validation errors of an error body (`errors`, strings or objects with `message`).
fn details(body: &[u8]) -> Vec<String> {
    serde_json::from_slice::<ErrorFields>(body)
        .map(|error| {
            error
                .errors
                .into_iter()
                .map(|detail| match detail {
                    serde_json::Value::String(text) => text,
                    serde_json::Value::Object(ref fields) => fields
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .map_or_else(|| detail.to_string(), str::to_owned),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use loom_api::{ForgeKind, Timestamp};

    use super::{Headers, classify, next_link, rate_limit_wait};
    use crate::client::{ForgeError, RateLimitWait};

    const RATE: &[u8] = br#"{"message":"API rate limit exceeded for user ID 1003."}"#;

    #[test]
    fn statuses_map_to_variants_without_reading_the_message() {
        let none = Headers::default();
        // The same message under every status: only the status and headers decide.
        let body = br#"{"message":"Not Found"}"#;
        assert!(matches!(
            classify(ForgeKind::Github, 401, &none, body),
            ForgeError::Unauthorized { .. }
        ));
        assert!(matches!(
            classify(ForgeKind::Github, 403, &none, body),
            ForgeError::Forbidden { .. }
        ));
        assert!(matches!(
            classify(ForgeKind::Github, 404, &none, body),
            ForgeError::NotFound { .. }
        ));
        assert!(matches!(
            classify(ForgeKind::Github, 422, &none, body),
            ForgeError::Unprocessable { .. }
        ));
        assert!(matches!(
            classify(ForgeKind::Github, 500, &none, body),
            ForgeError::Status { status: 500, .. }
        ));
        // A rate limit message under a plain 403 is a plain 403.
        assert!(matches!(
            classify(ForgeKind::Github, 403, &none, RATE),
            ForgeError::Forbidden { .. }
        ));
    }

    #[test]
    fn rate_limits_by_status_and_headers() {
        let exhausted = Headers::from([
            ("X-RateLimit-Remaining", "0"),
            ("X-RateLimit-Reset", "1791792000"),
        ]);
        let reset = Timestamp::from_unix_secs(1_791_792_000).unwrap();
        assert!(matches!(
            classify(ForgeKind::Github, 403, &exhausted, RATE),
            ForgeError::RateLimited { status: 403, wait: RateLimitWait::Until(at), .. } if at == reset
        ));
        // A secondary rate limit: requests remain, but `retry-after` says to wait.
        let secondary = Headers::from([("x-ratelimit-remaining", "12"), ("retry-after", "30")]);
        assert!(matches!(
            classify(ForgeKind::Github, 403, &secondary, RATE),
            ForgeError::RateLimited { wait: RateLimitWait::After(wait), .. }
                if wait == Duration::from_secs(30)
        ));
        let spaced = Headers::from([("x-ratelimit-remaining", " 0 ")]);
        assert!(matches!(
            classify(ForgeKind::Github, 403, &spaced, RATE),
            ForgeError::RateLimited { .. }
        ));
        assert!(matches!(
            classify(ForgeKind::Github, 429, &Headers::default(), b""),
            ForgeError::RateLimited {
                status: 429,
                wait: RateLimitWait::Unknown,
                ..
            }
        ));
    }

    #[test]
    fn retry_after_wins_over_the_reset() {
        let both = Headers::from([("retry-after", "5"), ("x-ratelimit-reset", "1791792000")]);
        assert_eq!(
            rate_limit_wait(&both),
            RateLimitWait::After(Duration::from_secs(5))
        );
        // An HTTP date in `retry-after` is not read: the reset is used.
        let date = Headers::from([
            ("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT"),
            ("x-ratelimit-reset", "1791792000"),
        ]);
        assert!(matches!(rate_limit_wait(&date), RateLimitWait::Until(_)));
        let bad = Headers::from([("x-ratelimit-reset", "soon")]);
        assert_eq!(rate_limit_wait(&bad), RateLimitWait::Unknown);
    }

    #[test]
    fn sso_wins_over_a_rate_limit() {
        let headers = Headers::from([
            (
                "x-github-sso",
                "required; url=https://github.com/orgs/acme/sso?x=1",
            ),
            ("x-ratelimit-remaining", "0"),
        ]);
        assert!(matches!(
            classify(ForgeKind::Github, 403, &headers, b"{}"),
            ForgeError::SsoRequired { url: Some(url) } if url == "https://github.com/orgs/acme/sso?x=1"
        ));
    }

    #[test]
    fn messages_and_details_of_error_bodies() {
        let ForgeError::Unprocessable { message, details } = classify(
            ForgeKind::Github,
            422,
            &Headers::default(),
            br#"{"message":"Unprocessable Entity","errors":["Review Can not approve your own pull request",{"message":"other"},{"code":"x"}]}"#,
        ) else {
            panic!("not a 422");
        };
        assert_eq!(message, "Unprocessable Entity");
        assert_eq!(
            details,
            [
                "Review Can not approve your own pull request",
                "other",
                r#"{"code":"x"}"#
            ]
        );
        let ForgeError::Status { message, .. } = classify(
            ForgeKind::Github,
            502,
            &Headers::default(),
            b"<html>Bad gateway</html>",
        ) else {
            panic!("not a status");
        };
        assert_eq!(message, "<html>Bad gateway</html>");
        let ForgeError::Forbidden { message } = classify(
            ForgeKind::Github,
            403,
            &Headers::default(),
            br#"{"error":"insufficient_scope","scope":"api"}"#,
        ) else {
            panic!("not a 403");
        };
        assert_eq!(message, "insufficient_scope");
    }

    #[test]
    fn gitlab_statuses_map_to_variants_without_reading_the_message() {
        let none = Headers::default();
        let gitlab = |status, body: &[u8]| classify(ForgeKind::Gitlab, status, &none, body);
        let body = br#"{"message":"404 Not found"}"#;
        assert!(matches!(gitlab(401, body), ForgeError::Unauthorized { .. }));
        assert!(matches!(gitlab(403, body), ForgeError::Forbidden { .. }));
        assert!(matches!(gitlab(404, body), ForgeError::NotFound { .. }));
        assert!(matches!(
            gitlab(409, body),
            ForgeError::Conflict { message } if message == "404 Not found"
        ));
        assert!(matches!(gitlab(400, body), ForgeError::BadRequest { .. }));
        assert!(matches!(
            gitlab(422, body),
            ForgeError::Unprocessable { .. }
        ));
        assert!(matches!(
            gitlab(500, b"<html>oops</html>"),
            ForgeError::Status { status: 500, message } if message == "<html>oops</html>"
        ));
        // GitLab limits the rate with 429 only: a 403 is never a rate limit there.
        let exhausted = Headers::from([("x-ratelimit-remaining", "0"), ("retry-after", "5")]);
        assert!(matches!(
            classify(ForgeKind::Gitlab, 403, &exhausted, body),
            ForgeError::Forbidden { .. }
        ));
        let limited = Headers::from([("RateLimit-Reset", "1791792000")]);
        let reset = Timestamp::from_unix_secs(1_791_792_000).unwrap();
        assert!(matches!(
            classify(ForgeKind::Gitlab, 429, &limited, b"Retry later"),
            ForgeError::RateLimited { status: 429, wait: RateLimitWait::Until(at), .. } if at == reset
        ));
    }

    #[test]
    fn a_gitlab_403_with_the_insufficient_scope_code_names_the_scope() {
        let none = Headers::default();
        let ForgeError::InsufficientScope { scope, message } = classify(
            ForgeKind::Gitlab,
            403,
            &none,
            br#"{"error":"insufficient_scope","error_description":"Needs more.","scope":"api"}"#,
        ) else {
            panic!("not an insufficient scope");
        };
        assert_eq!(scope.as_deref(), Some("api"));
        assert_eq!(message, "Needs more.");
        assert!(matches!(
            classify(ForgeKind::Gitlab, 403, &none, br#"{"error":"insufficient_scope"}"#),
            ForgeError::InsufficientScope { scope: None, message } if message == "insufficient_scope"
        ));
        // Another OAuth code, or the code in the message, is a plain 403.
        for body in [
            br#"{"error":"invalid_token"}"#.as_slice(),
            br#"{"message":"insufficient_scope"}"#,
        ] {
            assert!(matches!(
                classify(ForgeKind::Gitlab, 403, &none, body),
                ForgeError::Forbidden { .. }
            ));
        }
    }

    #[test]
    fn a_gitlab_400_keeps_its_validation_errors() {
        let ForgeError::BadRequest { message, details } = classify(
            ForgeKind::Gitlab,
            400,
            &Headers::default(),
            br#"{"message":{"base":["first","second"]}}"#,
        ) else {
            panic!("not a 400");
        };
        assert_eq!(message, "base: first");
        assert_eq!(details, ["base: first", "base: second"]);
    }

    #[test]
    fn github_keeps_400_and_409_as_plain_statuses() {
        let none = Headers::default();
        for status in [400, 409] {
            assert!(matches!(
                classify(ForgeKind::Github, status, &none, b"{}"),
                ForgeError::Status { .. }
            ));
        }
    }

    #[test]
    fn next_links() {
        let link = r#"<https://api.github.com/search/issues?q=x&page=2>; rel="next", <https://api.github.com/search/issues?q=x&page=5>; rel="last""#;
        assert_eq!(
            next_link(link).as_deref(),
            Some("https://api.github.com/search/issues?q=x&page=2")
        );
        let last = r#"<https://api.github.com/x?page=1>; rel="prev", <https://api.github.com/x?page=1>; rel="first""#;
        assert_eq!(next_link(last), None);
        assert_eq!(next_link(""), None);
        assert_eq!(next_link("garbage; rel=\"next\""), None);
        // A comma in the URL.
        assert_eq!(
            next_link(r#"<https://h/x?labels=a,b&page=1>; rel="prev", <https://h/x?labels=a,b&page=3>; rel="next""#).as_deref(),
            Some("https://h/x?labels=a,b&page=3")
        );
        // Several relations in one parameter.
        assert_eq!(
            next_link(r#"<https://h/x?page=3>; rel="last next""#).as_deref(),
            Some("https://h/x?page=3")
        );
    }
}
