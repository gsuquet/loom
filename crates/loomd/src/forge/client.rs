//! The forge calls of the credential check, behind [`ForgeApi`]: `GET /user` and, on GitLab,
//! `GET /personal_access_tokens/self`. The MR clients implement the same trait later.

use std::time::Duration;

use loom_api::ForgeKind;
use loom_update::tls::tls_config;
use serde::Deserialize;
use ureq::Agent;
use ureq::config::RedirectAuthHeaders;

use super::{ApiBase, Token};

/// Longest wait for one forge call.
const TIMEOUT: Duration = Duration::from_secs(20);
/// Largest JSON body read.
const MAX_JSON_BYTES: u64 = 1024 * 1024;

/// The user of a token, from `GET /user`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    /// GitHub `login`, GitLab `username`.
    pub login: String,
    /// The `x-oauth-scopes` header (GitHub classic and OAuth tokens only).
    pub scopes_header: Option<String>,
}

/// Why a forge call failed. Never contains the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiFailure {
    /// The forge answered with an error status.
    Status {
        /// HTTP status.
        status: u16,
        /// The `message` (GitHub, GitLab) or `error` (GitLab OAuth) of the body, otherwise the
        /// start of the body.
        message: String,
        /// The `x-github-sso` header.
        sso: Option<String>,
    },
    /// No answer, or an answer that cannot be read.
    Transport(String),
}

/// The forge calls the credential check makes.
pub trait ForgeApi {
    /// `GET /user` with `token`.
    ///
    /// # Errors
    ///
    /// [`ApiFailure`].
    fn user(&self, token: &Token) -> Result<User, ApiFailure>;

    /// The scopes of a GitLab personal access token: `GET /personal_access_tokens/self`.
    ///
    /// # Errors
    ///
    /// [`ApiFailure`]; the OAuth token of `glab` gets an error status.
    fn token_scopes(&self, token: &Token) -> Result<Vec<String>, ApiFailure>;
}

/// The HTTP client of one forge host.
pub struct HttpForge {
    agent: Agent,
    kind: ForgeKind,
    base: ApiBase,
}

impl std::fmt::Debug for HttpForge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpForge")
            .field("kind", &self.kind)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl HttpForge {
    /// A client of the API at `base`. An `https` base is called over HTTPS only, redirects
    /// included. The `Authorization` header follows a redirect only to the same host over
    /// HTTPS: the token never reaches another host.
    #[must_use]
    pub fn new(kind: ForgeKind, base: ApiBase, user_agent: &str) -> Self {
        let agent: Agent = Agent::config_builder()
            .tls_config(tls_config())
            .https_only(base.rest.starts_with("https://"))
            .redirect_auth_headers(RedirectAuthHeaders::SameHost)
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .user_agent(user_agent)
            .build()
            .into();
        Self { agent, kind, base }
    }

    /// `GET <base><path>`: the status, the headers loom reads, and the body.
    fn get(&self, path: &str, token: &Token) -> Result<Answer, ApiFailure> {
        let url = format!("{}{path}", self.base.rest);
        let mut request = self
            .agent
            .get(&url)
            .header("Authorization", &format!("Bearer {}", token.expose()));
        request = match self.kind {
            ForgeKind::Github => request
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28"),
            ForgeKind::Gitlab => request.header("Accept", "application/json"),
        };
        let mut response = request
            .call()
            .map_err(|error| ApiFailure::Transport(token.redact(&format!("{url}: {error}"))))?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let status = response.status().as_u16();
        let scopes = header("x-oauth-scopes");
        let sso = header("x-github-sso");
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_JSON_BYTES)
            .read_to_vec()
            .map_err(|error| ApiFailure::Transport(token.redact(&format!("{url}: {error}"))))?;
        if !(200..300).contains(&status) {
            return Err(ApiFailure::Status {
                status,
                message: token.redact(&error_message(&body)),
                sso,
            });
        }
        Ok(Answer { scopes, body })
    }
}

struct Answer {
    scopes: Option<String>,
    body: Vec<u8>,
}

#[derive(Deserialize)]
struct UserBody {
    login: Option<String>,
    username: Option<String>,
}

#[derive(Deserialize)]
struct TokenBody {
    scopes: Vec<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: Option<serde_json::Value>,
    error: Option<String>,
}

/// The message of an error body: `message`, otherwise `error`, otherwise its first 200 bytes.
fn error_message(body: &[u8]) -> String {
    if let Ok(error) = serde_json::from_slice::<ErrorBody>(body) {
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

fn json<T: serde::de::DeserializeOwned>(body: &[u8], what: &str) -> Result<T, ApiFailure> {
    serde_json::from_slice(body)
        .map_err(|error| ApiFailure::Transport(format!("unexpected answer to {what}: {error}")))
}

impl ForgeApi for HttpForge {
    fn user(&self, token: &Token) -> Result<User, ApiFailure> {
        let answer = self.get("/user", token)?;
        let body: UserBody = json(&answer.body, "GET /user")?;
        let login = body.login.or(body.username).ok_or_else(|| {
            ApiFailure::Transport("unexpected answer to GET /user: no login".to_owned())
        })?;
        Ok(User {
            login,
            scopes_header: answer.scopes,
        })
    }

    fn token_scopes(&self, token: &Token) -> Result<Vec<String>, ApiFailure> {
        let answer = self.get("/personal_access_tokens/self", token)?;
        let body: TokenBody = json(&answer.body, "GET /personal_access_tokens/self")?;
        Ok(body.scopes)
    }
}

#[cfg(test)]
mod tests {
    use super::error_message;

    #[test]
    fn error_messages_of_both_forges() {
        assert_eq!(
            error_message(br#"{"message":"Bad credentials","status":"401"}"#),
            "Bad credentials"
        );
        assert_eq!(
            error_message(br#"{"message":"401 Unauthorized"}"#),
            "401 Unauthorized"
        );
        assert_eq!(
            error_message(br#"{"error":"insufficient_scope","scope":"api"}"#),
            "insufficient_scope"
        );
        assert_eq!(
            error_message(br#"{"message":{"base":["is invalid"]}}"#),
            r#"{"base":["is invalid"]}"#
        );
        assert_eq!(
            error_message(b"<html>Bad gateway</html>"),
            "<html>Bad gateway</html>"
        );
    }
}
