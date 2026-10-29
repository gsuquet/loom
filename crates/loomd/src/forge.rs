//! Forge credentials (ADR-0016, decision 6).
//!
//! The token resolved from `token_cmd` or `token_env`, the minimal
//! client of `GET /user`, and the check of `loom repo doctor`. The decisions here are pure;
//! [`token`] runs the commands and [`client`] calls the forge.

pub mod check;
pub mod client;
pub mod metadata;
pub mod open;
pub mod token;

use loom_api::ForgeAccess;

pub use loom_forge::client::{ApiBase, REDACTED, Token, api_base, sso_url};

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

#[cfg(test)]
mod tests {
    use loom_api::ForgeAccess;

    use super::{github_access, gitlab_access};

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
}
