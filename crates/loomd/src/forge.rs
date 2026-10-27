//! Forge credentials (ADR-0016, decision 6).
//!
//! The API of a forge host, the token resolved from `token_cmd` or `token_env`, the minimal
//! client of `GET /user`, and the check of `loom repo doctor`. The decisions here are pure;
//! [`token`] runs the commands and [`client`] calls the forge.

pub mod check;
pub mod client;
pub mod token;

use std::fmt;

use loom_api::{ForgeAccess, ForgeKind};

/// The API endpoints of a forge host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiBase {
    /// Base of the REST API, without a trailing `/`.
    pub rest: String,
    /// The GraphQL endpoint (GitHub only).
    pub graphql: Option<String>,
}

/// The API of `host`: `api.github.com` for `github.com`, `/api/v3` and `/api/graphql` on a
/// GitHub Enterprise Server, `/api/v4` on GitLab. Pure.
#[must_use]
pub fn api_base(kind: ForgeKind, host: &str) -> ApiBase {
    match kind {
        ForgeKind::Github if host == "github.com" => ApiBase {
            rest: "https://api.github.com".to_owned(),
            graphql: Some("https://api.github.com/graphql".to_owned()),
        },
        ForgeKind::Github => ApiBase {
            rest: format!("https://{host}/api/v3"),
            graphql: Some(format!("https://{host}/api/graphql")),
        },
        ForgeKind::Gitlab => ApiBase {
            rest: format!("https://{host}/api/v4"),
            graphql: None,
        },
    }
}

/// A forge token. Its `Debug` and `Display` never show the value, so that it cannot reach a
/// log, a span or an error by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// A token from its value.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    /// The value, for the `Authorization` header only.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// `text` with every occurrence of the token replaced by `<redacted>`.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        if self.0.is_empty() {
            return text.to_owned();
        }
        text.replace(&self.0, REDACTED)
    }
}

/// What replaces a token in a text.
pub const REDACTED: &str = "<redacted>";

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Token({REDACTED})")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// What a GitHub token can do, from the `x-oauth-scopes` header of `GET /user`. Pure.
///
/// Classic and OAuth tokens (`gh`) have the header: `repo` publishes reviews. Fine-grained
/// tokens and GitHub App tokens do not: their scopes are unknown.
#[must_use]
pub fn github_access(scopes_header: Option<&str>) -> ForgeAccess {
    let Some(header) = scopes_header else {
        return ForgeAccess::Unknown {
            reason: "GitHub gives no scopes for a fine-grained token: it needs Pull requests read and write, and Contents read".to_owned(),
        };
    };
    let scopes: Vec<String> = header
        .split(',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned)
        .collect();
    if scopes.iter().any(|scope| scope == "repo") {
        ForgeAccess::Write { scopes }
    } else {
        ForgeAccess::ReadOnly { scopes }
    }
}

/// What a GitLab token can do, from the `scopes` of `GET /personal_access_tokens/self`: `api`
/// publishes, `read_api` only reads. Pure.
#[must_use]
pub fn gitlab_access(scopes: Vec<String>) -> ForgeAccess {
    if scopes.iter().any(|scope| scope == "api") {
        ForgeAccess::Write { scopes }
    } else {
        ForgeAccess::ReadOnly { scopes }
    }
}

/// The authorization URL of an `x-github-sso` header (`required; url=<url>`). `None` when the
/// header gives none. Pure.
#[must_use]
pub fn sso_url(header: &str) -> Option<String> {
    header
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("url="))
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use loom_api::{ForgeAccess, ForgeKind};

    use super::{REDACTED, Token, api_base, github_access, gitlab_access, sso_url};

    #[test]
    fn api_bases_of_public_and_self_hosted_forges() {
        let github = api_base(ForgeKind::Github, "github.com");
        assert_eq!(github.rest, "https://api.github.com");
        assert_eq!(
            github.graphql.as_deref(),
            Some("https://api.github.com/graphql")
        );
        let enterprise = api_base(ForgeKind::Github, "github.company-b.example");
        assert_eq!(enterprise.rest, "https://github.company-b.example/api/v3");
        assert_eq!(
            enterprise.graphql.as_deref(),
            Some("https://github.company-b.example/api/graphql")
        );
        let gitlab = api_base(ForgeKind::Gitlab, "gitlab.company-a.example");
        assert_eq!(gitlab.rest, "https://gitlab.company-a.example/api/v4");
        assert_eq!(gitlab.graphql, None);
        assert_eq!(
            api_base(ForgeKind::Gitlab, "gitlab.com").rest,
            "https://gitlab.com/api/v4"
        );
    }

    #[test]
    fn a_token_never_prints_its_value() {
        let token = Token::new("ghp_secret".to_owned());
        assert_eq!(format!("{token:?}"), format!("Token({REDACTED})"));
        assert_eq!(token.to_string(), REDACTED);
        assert_eq!(
            token.redact("bad ghp_secret, ghp_secret"),
            format!("bad {REDACTED}, {REDACTED}")
        );
        assert_eq!(Token::new(String::new()).redact("text"), "text");
    }

    #[test]
    fn github_scopes() {
        assert!(matches!(
            github_access(Some("gist, read:org, repo, workflow")),
            ForgeAccess::Write { scopes } if scopes == ["gist", "read:org", "repo", "workflow"]
        ));
        assert!(matches!(
            github_access(Some("read:org, public_repo")),
            ForgeAccess::ReadOnly { scopes } if scopes == ["read:org", "public_repo"]
        ));
        // A classic token without any scope.
        assert!(matches!(
            github_access(Some("")),
            ForgeAccess::ReadOnly { scopes } if scopes.is_empty()
        ));
        // A fine-grained token.
        assert!(matches!(github_access(None), ForgeAccess::Unknown { .. }));
    }

    #[test]
    fn gitlab_scopes() {
        assert!(matches!(
            gitlab_access(vec!["api".to_owned()]),
            ForgeAccess::Write { .. }
        ));
        assert!(matches!(
            gitlab_access(vec!["read_api".to_owned(), "read_user".to_owned()]),
            ForgeAccess::ReadOnly { scopes } if scopes.len() == 2
        ));
    }

    #[test]
    fn sso_urls() {
        assert_eq!(
            sso_url("required; url=https://github.com/orgs/acme/sso?authorization_request=x"),
            Some("https://github.com/orgs/acme/sso?authorization_request=x".to_owned())
        );
        assert_eq!(sso_url("partial-results; organizations=21955855"), None);
        assert_eq!(sso_url("required; url="), None);
    }
}
