//! Forge credentials and polling (ADR-0016).
//!
//! `token_cmd`, `token_env`, `poll_interval` and `review_requests` of `[forges."<host>"]`, the
//! `[contexts.<ctx>.forges."<host>"]` overrides, and the credential of a context for a host.

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

pub use loom_api::CredentialOrigin;
use loom_api::{ContextName, ForgeKind};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

use crate::config::Config;
use crate::duration::{format_duration, parse_duration};
use crate::repos::ForgeHost;

/// Forge of the public hosts `github.com` and `gitlab.com`; `None` for any other host. Pure.
#[must_use]
pub fn public_forge(host: &str) -> Option<ForgeKind> {
    match host {
        "github.com" => Some(ForgeKind::Github),
        "gitlab.com" => Some(ForgeKind::Gitlab),
        _ => None,
    }
}

/// Where a forge token comes from. The token itself is never in the configuration. `Debug`
/// does not print the text of `token_cmd`: it can hold a secret.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TokenSource {
    /// `token_cmd`: a command run through `sh -c`; its standard output, trimmed, is the token.
    Command(String),
    /// `token_env`: the name of an environment variable of loomd that holds the token.
    Env(String),
}

impl fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command(_) => f.write_str("Command(<token_cmd>)"),
            Self::Env(name) => f.debug_tuple("Env").field(name).finish(),
        }
    }
}

/// `token_cmd` and `token_env` of one table: at most one of them.
fn token_source(
    token_cmd: Option<String>,
    token_env: Option<String>,
) -> Result<Option<TokenSource>, String> {
    match (token_cmd, token_env) {
        (Some(_), Some(_)) => Err(
            "token_cmd and token_env are both set: give only one source for the token".to_owned(),
        ),
        (Some(command), None) => {
            if command.trim().is_empty() {
                return Err("token_cmd is empty".to_owned());
            }
            Ok(Some(TokenSource::Command(command)))
        }
        (None, Some(name)) => {
            if !is_env_name(&name) {
                return Err(format!(
                    "invalid token_env `{name}`: expected the name of an environment variable (letters, digits, `_`, not starting with a digit)"
                ));
            }
            Ok(Some(TokenSource::Env(name)))
        }
        (None, None) => Ok(None),
    }
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Shortest `[forges."<host>"] poll_interval` accepted.
pub const MIN_POLL_INTERVAL: Duration = Duration::from_mins(1);
/// Default `[forges."<host>"] poll_interval`.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_mins(5);

/// `[forges."<host>"] poll_interval`: a duration of at least [`MIN_POLL_INTERVAL`], written as
/// text (`"5m"`). Default [`DEFAULT_POLL_INTERVAL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollInterval(Duration);

impl PollInterval {
    /// The duration.
    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }
}

impl Default for PollInterval {
    fn default() -> Self {
        Self(DEFAULT_POLL_INTERVAL)
    }
}

impl<'de> Deserialize<'de> for PollInterval {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let duration = parse_duration(&text)
            .map_err(|error| D::Error::custom(format!("poll_interval: {error}")))?;
        if duration < MIN_POLL_INTERVAL {
            return Err(D::Error::custom(format!(
                "poll_interval `{text}` is shorter than the minimum {}",
                format_duration(MIN_POLL_INTERVAL)
            )));
        }
        Ok(Self(duration))
    }
}

/// `[forges."<host>"] review_requests`: which GitHub review requests find an MR. GitLab has
/// one kind of request and ignores it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRequests {
    /// Requests to me and to my teams (`review-requested:@me`).
    #[default]
    Teams,
    /// Requests to me only (`user-review-requested:@me`).
    Direct,
}

/// `[forges."<host>"]`: the forge of a host, and its credential for every context.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawForge")]
pub struct ForgeConfig {
    /// `kind`: `github` or `gitlab`.
    pub kind: ForgeKind,
    /// `token_cmd` or `token_env`; `None` when the table gives no credential.
    pub token: Option<TokenSource>,
    /// `poll_interval`.
    pub poll_interval: PollInterval,
    /// `review_requests`.
    pub review_requests: ReviewRequests,
}

impl ForgeConfig {
    /// A table with only `kind`.
    #[must_use]
    pub fn new(kind: ForgeKind) -> Self {
        Self {
            kind,
            token: None,
            poll_interval: PollInterval::default(),
            review_requests: ReviewRequests::default(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawForge {
    kind: ForgeKind,
    token_cmd: Option<String>,
    token_env: Option<String>,
    #[serde(default)]
    poll_interval: PollInterval,
    #[serde(default)]
    review_requests: ReviewRequests,
}

impl TryFrom<RawForge> for ForgeConfig {
    type Error = String;

    fn try_from(raw: RawForge) -> Result<Self, Self::Error> {
        Ok(Self {
            kind: raw.kind,
            token: token_source(raw.token_cmd, raw.token_env)?,
            poll_interval: raw.poll_interval,
            review_requests: raw.review_requests,
        })
    }
}

/// `[contexts.<ctx>.forges."<host>"]`: the credential of one context for a host, such as
/// another account on the same host.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawContextForge")]
pub struct ContextForge {
    /// `token_cmd` or `token_env`.
    pub token: TokenSource,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContextForge {
    token_cmd: Option<String>,
    token_env: Option<String>,
}

impl TryFrom<RawContextForge> for ContextForge {
    type Error = String;

    fn try_from(raw: RawContextForge) -> Result<Self, Self::Error> {
        token_source(raw.token_cmd, raw.token_env)?
            .map(|token| Self { token })
            .ok_or_else(|| "expected token_cmd or token_env".to_owned())
    }
}

/// The credential of one context for one forge host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeCredential<'a> {
    /// The context.
    pub context: &'a ContextName,
    /// The forge host.
    pub host: &'a ForgeHost,
    /// Its forge.
    pub kind: ForgeKind,
    /// Where the token comes from.
    pub token: &'a TokenSource,
    /// Which table gives it.
    pub origin: CredentialOrigin,
}

impl Config {
    /// The forge of `host`: its `[forges."<host>"] kind`, otherwise [`public_forge`].
    #[must_use]
    pub fn forge_kind(&self, host: &str) -> Option<ForgeKind> {
        self.forges
            .iter()
            .find(|(name, _)| name.as_str() == host)
            .map_or_else(|| public_forge(host), |(_, forge)| Some(forge.kind))
    }

    /// The credential of `context` for `host`: `[contexts.<ctx>.forges."<host>"]` first, then
    /// `[forges."<host>"]`. `None`: the context does not poll that host.
    #[must_use]
    pub fn forge_credential<'a>(
        &'a self,
        context: &'a ContextName,
        host: &'a ForgeHost,
    ) -> Option<ForgeCredential<'a>> {
        let kind = self.forge_kind(host.as_str())?;
        let own = self
            .contexts
            .get(context)
            .and_then(|config| config.forges.get(host))
            .map(|forge| (&forge.get_ref().token, CredentialOrigin::Context));
        let shared = || {
            self.forges
                .get(host)
                .and_then(|forge| forge.token.as_ref())
                .map(|token| (token, CredentialOrigin::Host))
        };
        let (token, origin) = own.or_else(shared)?;
        Some(ForgeCredential {
            context,
            host,
            kind,
            token,
            origin,
        })
    }

    /// Every credential of every context, by context, then host. A context without any
    /// credential for a host does not appear for it.
    #[must_use]
    pub fn forge_credentials(&self) -> Vec<ForgeCredential<'_>> {
        let mut credentials = Vec::new();
        for (name, context) in &self.contexts {
            let hosts: BTreeSet<&ForgeHost> =
                self.forges.keys().chain(context.forges.keys()).collect();
            credentials.extend(
                hosts
                    .into_iter()
                    .filter_map(|host| self.forge_credential(name, host)),
            );
        }
        credentials
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use loom_api::{ContextName, ForgeKind};

    use super::{CredentialOrigin, DEFAULT_POLL_INTERVAL, ReviewRequests, TokenSource};
    use crate::config::{Config, parse_config};
    use crate::repos::ForgeHost;

    const PATH: &str = "/home/me/.loom/config.toml";

    fn parse(text: &str) -> Config {
        parse_config(text, Path::new(PATH)).unwrap()
    }

    fn error(text: &str) -> String {
        parse_config(text, Path::new(PATH)).unwrap_err().to_string()
    }

    fn host(name: &str) -> ForgeHost {
        ForgeHost::try_from(name.to_owned()).unwrap()
    }

    fn context(name: &str) -> ContextName {
        name.parse().unwrap()
    }

    const EXAMPLE: &str = r#"
[contexts.personal]
[contexts.company-a]

[contexts.company-a.forges."github.com"]
token_cmd = "op read op://company-a/github/token"

[forges."github.com"]
kind = "github"
token_cmd = "gh auth token --hostname github.com"
review_requests = "direct"

[forges."gitlab.company-a.example"]
kind = "gitlab"
token_env = "GITLAB_TOKEN"
poll_interval = "10m"

[forges."git.company-b.example"]
kind = "gitlab"
"#;

    #[test]
    fn credential_keys_parse() {
        let config = parse(EXAMPLE);
        let github = &config.forges[&host("github.com")];
        assert_eq!(
            github.token,
            Some(TokenSource::Command(
                "gh auth token --hostname github.com".to_owned()
            ))
        );
        assert_eq!(github.review_requests, ReviewRequests::Direct);
        assert_eq!(github.poll_interval.get(), DEFAULT_POLL_INTERVAL);
        let gitlab = &config.forges[&host("gitlab.company-a.example")];
        assert_eq!(
            gitlab.token,
            Some(TokenSource::Env("GITLAB_TOKEN".to_owned()))
        );
        assert_eq!(gitlab.review_requests, ReviewRequests::Teams);
        assert_eq!(gitlab.poll_interval.get(), Duration::from_mins(10));
        assert_eq!(config.forges[&host("git.company-b.example")].token, None);
    }

    #[test]
    fn the_context_table_wins_over_the_host_table() {
        let config = parse(EXAMPLE);
        let (company, personal) = (context("company-a"), context("personal"));
        let github = host("github.com");
        let own = config.forge_credential(&company, &github).unwrap();
        assert_eq!(own.origin, CredentialOrigin::Context);
        assert_eq!(
            own.token,
            &TokenSource::Command("op read op://company-a/github/token".to_owned())
        );
        let shared = config.forge_credential(&personal, &github).unwrap();
        assert_eq!(shared.origin, CredentialOrigin::Host);
        assert_eq!(shared.kind, ForgeKind::Github);
        // No credential: the context does not poll that host.
        let (company_b, gitlab_com) = (host("git.company-b.example"), host("gitlab.com"));
        assert_eq!(config.forge_credential(&personal, &company_b), None);
        assert_eq!(config.forge_credential(&personal, &gitlab_com), None);
    }

    #[test]
    fn every_credential_of_every_context() {
        let config = parse(EXAMPLE);
        let listed: Vec<(String, String, CredentialOrigin)> = config
            .forge_credentials()
            .iter()
            .map(|credential| {
                (
                    credential.context.to_string(),
                    credential.host.as_str().to_owned(),
                    credential.origin,
                )
            })
            .collect();
        let row = |context: &str, host: &str, origin| (context.to_owned(), host.to_owned(), origin);
        assert_eq!(
            listed,
            [
                row("company-a", "github.com", CredentialOrigin::Context),
                row(
                    "company-a",
                    "gitlab.company-a.example",
                    CredentialOrigin::Host
                ),
                row("personal", "github.com", CredentialOrigin::Host),
                row(
                    "personal",
                    "gitlab.company-a.example",
                    CredentialOrigin::Host
                ),
            ]
        );
    }

    #[test]
    fn a_context_table_of_a_public_host_needs_no_host_table() {
        let config =
            parse("[contexts.personal.forges.\"gitlab.com\"]\ntoken_env = \"GITLAB_COM_TOKEN\"\n");
        let (personal, gitlab_com) = (context("personal"), host("gitlab.com"));
        let credential = config.forge_credential(&personal, &gitlab_com).unwrap();
        assert_eq!(credential.kind, ForgeKind::Gitlab);
        assert_eq!(credential.origin, CredentialOrigin::Context);
    }

    #[test]
    fn debug_does_not_print_the_command() {
        let source = TokenSource::Command("op read op://secret".to_owned());
        assert_eq!(format!("{source:?}"), "Command(<token_cmd>)");
    }

    #[test]
    fn token_cmd_and_token_env_together_are_rejected() {
        let both = "[forges.\"github.com\"]\nkind = \"github\"\ntoken_cmd = \"gh auth token\"\ntoken_env = \"GITHUB_TOKEN\"\n";
        assert!(
            error(both).contains("token_cmd and token_env are both set"),
            "{}",
            error(both)
        );
        let both = "[contexts.personal.forges.\"github.com\"]\ntoken_cmd = \"gh auth token\"\ntoken_env = \"GITHUB_TOKEN\"\n";
        assert!(
            error(both).contains("token_cmd and token_env are both set"),
            "{}",
            error(both)
        );
    }

    #[test]
    fn invalid_credential_values_are_rejected() {
        for (text, needle) in [
            (
                "[forges.\"github.com\"]\nkind = \"github\"\ntoken_cmd = \"  \"\n",
                "token_cmd is empty",
            ),
            (
                "[forges.\"github.com\"]\nkind = \"github\"\ntoken_env = \"1TOKEN\"\n",
                "invalid token_env `1TOKEN`",
            ),
            (
                "[forges.\"github.com\"]\nkind = \"github\"\ntoken_env = \"A=B\"\n",
                "invalid token_env",
            ),
            (
                "[contexts.personal.forges.\"github.com\"]\n",
                "expected token_cmd or token_env",
            ),
            (
                "[contexts.personal.forges.\"github.com\"]\ntoken_cmd = \"x\"\nkind = \"github\"\n",
                "unknown field `kind`",
            ),
            (
                "[forges.\"github.com\"]\nkind = \"github\"\npoll_interval = \"30s\"\n",
                "poll_interval `30s` is shorter than the minimum 1m",
            ),
            (
                "[forges.\"github.com\"]\nkind = \"github\"\nreview_requests = \"all\"\n",
                "unknown variant `all`",
            ),
            (
                "[forges.\"git.example\"]\nkind = \"gitea\"\n",
                "unknown variant `gitea`",
            ),
            (
                "[forges.\"git.example\"]\ntoken_env = \"TOKEN\"\n",
                "missing field `kind`",
            ),
        ] {
            let message = error(text);
            assert!(message.contains(needle), "{text}\n{message}");
        }
    }

    #[test]
    fn a_context_table_of_an_unknown_host_is_rejected() {
        let text = "[contexts.personal]\n\n[contexts.personal.forges.\"git.example\"]\ntoken_env = \"TOKEN\"\n";
        let error = parse_config(text, Path::new(PATH)).unwrap_err();
        assert!(
            error.message.contains("forge of `git.example` is unknown"),
            "{error}"
        );
        assert_eq!(error.location.map(|at| at.line), Some(3));
    }
}
