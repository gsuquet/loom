//! The `forge <context> <host>` check of `loom repo doctor`: the token resolves, `GET /user`
//! accepts it, and its scopes can publish a review.

use loom_api::{
    ContextName, CredentialOrigin, ForgeAccess, ForgeCheck, ForgeCheckOutcome, ForgeKind,
    TokenSourceKind,
};
use loom_config::config::Config;
use loom_config::forges::TokenSource;
use tracing::{info_span, warn};

use super::client::{ApiFailure, ForgeApi};
use super::token::TokenResolver;
use super::{ApiBase, Token, api_base, github_access, gitlab_access, sso_url};

/// The credential of one context for one forge host, as loomd uses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeTarget {
    /// The context.
    pub context: ContextName,
    /// The forge host.
    pub host: String,
    /// Its forge.
    pub kind: ForgeKind,
    /// Its API.
    pub api: ApiBase,
    /// Which table gives the credential.
    pub origin: CredentialOrigin,
    /// Where the token comes from.
    pub source: TokenSource,
}

/// Every credential of the contexts that `held` accepts, by context, then host. Pure.
#[must_use]
pub fn forge_targets(config: &Config, held: impl Fn(&ContextName) -> bool) -> Vec<ForgeTarget> {
    config
        .forge_credentials()
        .into_iter()
        .filter(|credential| held(credential.context))
        .map(|credential| ForgeTarget {
            context: credential.context.clone(),
            host: credential.host.as_str().to_owned(),
            kind: credential.kind,
            api: api_base(credential.kind, credential.host.as_str()),
            origin: credential.origin,
            source: credential.token.clone(),
        })
        .collect()
}

/// Checks the credential of `target` with `api`, the client of its host. A 401 resolves the
/// token again, once, as a poll does.
pub fn check_forge(
    target: &ForgeTarget,
    resolver: &TokenResolver,
    api: &dyn ForgeApi,
) -> ForgeCheck {
    let span = info_span!(
        "forge.check",
        context = target.context.as_str(),
        host = target.host.as_str()
    );
    let _entered = span.enter();
    ForgeCheck {
        context: target.context.clone(),
        host: target.host.clone(),
        kind: target.kind,
        api: target.api.rest.clone(),
        origin: target.origin,
        source: match &target.source {
            TokenSource::Command(_) => TokenSourceKind::Command,
            TokenSource::Env(name) => TokenSourceKind::Env { name: name.clone() },
        },
        result: outcome(target, resolver, api),
    }
}

fn outcome(
    target: &ForgeTarget,
    resolver: &TokenResolver,
    api: &dyn ForgeApi,
) -> ForgeCheckOutcome {
    let resolve = || {
        resolver
            .resolve(&target.source)
            .map_err(|error| ForgeCheckOutcome::TokenUnresolved {
                problem: error.problem(),
                error: error.to_string(),
                program: error.program().map(str::to_owned),
            })
    };
    let token = match resolve() {
        Ok(token) => token,
        Err(outcome) => return outcome,
    };
    let (token, user) = match api.user(&token) {
        Err(ApiFailure::Status { status: 401, .. }) => {
            // The token may have been renewed since it was resolved.
            resolver.invalidate(&target.source);
            let token = match resolve() {
                Ok(token) => token,
                Err(outcome) => return outcome,
            };
            let user = api.user(&token);
            if matches!(user, Err(ApiFailure::Status { status: 401, .. })) {
                resolver.invalidate(&target.source);
            }
            (token, user)
        }
        user => (token, user),
    };
    match user {
        Ok(user) => ForgeCheckOutcome::Authenticated {
            login: user.login,
            access: match target.kind {
                ForgeKind::Github => github_access(user.scopes_header.as_deref()),
                ForgeKind::Gitlab => gitlab_token_access(api, &token),
            },
        },
        Err(failure) => failed(failure, &token),
    }
}

/// The scopes of a GitLab personal access token; unknown for other tokens (`glab`'s OAuth
/// token).
fn gitlab_token_access(api: &dyn ForgeApi, token: &Token) -> ForgeAccess {
    match api.token_scopes(token) {
        Ok(scopes) => gitlab_access(scopes),
        Err(failure) => {
            let why = match failure {
                ApiFailure::Status {
                    status, message, ..
                } => format!("{status} {message}"),
                ApiFailure::Transport(error) => error,
            };
            warn!(
                error = token.redact(&why).as_str(),
                "cannot read the scopes of a GitLab token"
            );
            ForgeAccess::Unknown {
                reason: format!(
                    "GET /personal_access_tokens/self failed ({}): the token is not a personal access token, such as the OAuth token of glab; it needs the api scope to publish",
                    token.redact(&why)
                ),
            }
        }
    }
}

fn failed(failure: ApiFailure, token: &Token) -> ForgeCheckOutcome {
    match failure {
        ApiFailure::Status {
            status: 403,
            sso: Some(sso),
            ..
        } => ForgeCheckOutcome::SsoRequired { url: sso_url(&sso) },
        ApiFailure::Status {
            status, message, ..
        } => ForgeCheckOutcome::Rejected {
            status,
            error: token.redact(&message),
        },
        ApiFailure::Transport(error) => ForgeCheckOutcome::Unreachable {
            error: token.redact(&error),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::Path;
    use std::time::Duration;

    use loom_api::{
        CredentialOrigin, ForgeAccess, ForgeCheckOutcome, ForgeKind, TokenProblem, TokenSourceKind,
    };
    use loom_config::config::parse_config;
    use loom_config::forges::TokenSource;

    use super::{ForgeTarget, check_forge, forge_targets};
    use crate::forge::client::{ApiFailure, ForgeApi, User};
    use crate::forge::token::TokenResolver;
    use crate::forge::{Token, api_base};

    /// Answers from a script, and records the tokens it got.
    struct Fake {
        users: RefCell<Vec<Result<User, ApiFailure>>>,
        scopes: Result<Vec<String>, ApiFailure>,
        seen: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(users: Vec<Result<User, ApiFailure>>) -> Self {
            Self {
                users: RefCell::new(users),
                scopes: Ok(vec!["api".to_owned()]),
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl ForgeApi for Fake {
        fn user(&self, token: &Token) -> Result<User, ApiFailure> {
            self.seen.borrow_mut().push(token.expose().to_owned());
            self.users.borrow_mut().remove(0)
        }

        fn token_scopes(&self, _token: &Token) -> Result<Vec<String>, ApiFailure> {
            self.scopes.clone()
        }
    }

    #[allow(clippy::unnecessary_wraps)] // An answer of the script.
    fn user(scopes: Option<&str>) -> Result<User, ApiFailure> {
        Ok(User {
            login: "carol".to_owned(),
            scopes_header: scopes.map(str::to_owned),
        })
    }

    fn status(status: u16, message: &str) -> Result<User, ApiFailure> {
        Err(ApiFailure::Status {
            status,
            message: message.to_owned(),
            sso: None,
        })
    }

    fn target(kind: ForgeKind, source: TokenSource) -> ForgeTarget {
        let host = match kind {
            ForgeKind::Github => "github.com",
            ForgeKind::Gitlab => "gitlab.example.com",
        };
        ForgeTarget {
            context: "personal".parse().unwrap(),
            host: host.to_owned(),
            kind,
            api: api_base(kind, host),
            origin: CredentialOrigin::Host,
            source,
        }
    }

    fn resolver() -> TokenResolver {
        TokenResolver::new(Duration::from_secs(10), Box::new(|_| None))
    }

    fn command(text: &str) -> TokenSource {
        TokenSource::Command(text.to_owned())
    }

    #[test]
    fn targets_of_held_contexts() {
        let config = parse_config(
            r#"
[contexts.personal]
[contexts.company-a]
[contexts.company-a.forges."github.com"]
token_env = "COMPANY_A_GITHUB_TOKEN"
[forges."github.com"]
kind = "github"
token_cmd = "gh auth token"
[forges."gitlab.company-a.example"]
kind = "gitlab"
"#,
            Path::new("/c.toml"),
        )
        .unwrap();
        let targets = forge_targets(&config, |context| context.as_str() == "company-a");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].host, "github.com");
        assert_eq!(targets[0].origin, CredentialOrigin::Context);
        assert_eq!(targets[0].api.rest, "https://api.github.com");
        assert_eq!(forge_targets(&config, |_| true).len(), 2);
    }

    #[test]
    fn a_classic_github_token_with_repo_can_publish() {
        let fake = Fake::new(vec![user(Some("repo, read:org"))]);
        let check = check_forge(
            &target(ForgeKind::Github, command("echo tok")),
            &resolver(),
            &fake,
        );
        assert_eq!(check.source, TokenSourceKind::Command);
        assert!(matches!(
            check.result,
            ForgeCheckOutcome::Authenticated { ref login, access: ForgeAccess::Write { .. } } if login == "carol"
        ));
    }

    #[test]
    fn a_read_only_gitlab_token_cannot_publish() {
        let mut fake = Fake::new(vec![user(None)]);
        fake.scopes = Ok(vec!["read_api".to_owned()]);
        let check = check_forge(
            &target(ForgeKind::Gitlab, command("echo tok")),
            &resolver(),
            &fake,
        );
        assert!(matches!(
            check.result,
            ForgeCheckOutcome::Authenticated {
                access: ForgeAccess::ReadOnly { .. },
                ..
            }
        ));
    }

    #[test]
    fn gitlab_scopes_are_unknown_for_an_oauth_token() {
        let mut fake = Fake::new(vec![user(None)]);
        fake.scopes = Err(ApiFailure::Status {
            status: 401,
            message: "401 Unauthorized".to_owned(),
            sso: None,
        });
        let check = check_forge(
            &target(ForgeKind::Gitlab, command("echo tok")),
            &resolver(),
            &fake,
        );
        assert!(matches!(
            check.result,
            ForgeCheckOutcome::Authenticated { access: ForgeAccess::Unknown { ref reason }, .. }
                if reason.contains("glab")
        ));
    }

    #[test]
    fn a_401_resolves_the_token_again_once() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("n");
        // Gives tok-1, then tok-2: a renewed token.
        let source = command(&format!(
            "echo x >> '{0}'; echo tok-$(wc -l < '{0}' | tr -d ' ')",
            counter.display()
        ));
        let resolver = resolver();
        let fake = Fake::new(vec![status(401, "Bad credentials"), user(Some("repo"))]);
        let check = check_forge(&target(ForgeKind::Github, source.clone()), &resolver, &fake);
        assert!(matches!(
            check.result,
            ForgeCheckOutcome::Authenticated { .. }
        ));
        assert_eq!(*fake.seen.borrow(), ["tok-1", "tok-2"]);

        // Refused twice: rejected, and the token is not kept.
        let fake = Fake::new(vec![
            status(401, "Bad credentials"),
            status(401, "Bad credentials"),
        ]);
        resolver.invalidate(&source);
        let check = check_forge(&target(ForgeKind::Github, source), &resolver, &fake);
        assert_eq!(
            check.result,
            ForgeCheckOutcome::Rejected {
                status: 401,
                error: "Bad credentials".to_owned()
            }
        );
        assert_eq!(*fake.seen.borrow(), ["tok-3", "tok-4"]);
    }

    #[test]
    fn a_token_that_cannot_be_resolved_fails_before_any_call() {
        let fake = Fake::new(vec![]);
        for (source, problem) in [
            (command("exit 1"), TokenProblem::CommandFailed),
            (command("true"), TokenProblem::Empty),
            (
                command("loom-no-such-program-xyz"),
                TokenProblem::CommandNotFound,
            ),
            (TokenSource::Env("UNSET".to_owned()), TokenProblem::EnvUnset),
        ] {
            let check = check_forge(&target(ForgeKind::Github, source), &resolver(), &fake);
            assert!(
                matches!(check.result, ForgeCheckOutcome::TokenUnresolved { problem: got, .. } if got == problem),
                "{check:?}"
            );
        }
        assert!(fake.seen.borrow().is_empty());
    }

    #[test]
    fn sso_and_transport_failures() {
        let fake = Fake::new(vec![Err(ApiFailure::Status {
            status: 403,
            message: "Resource protected by organization SAML enforcement".to_owned(),
            sso: Some(
                "required; url=https://github.com/orgs/acme/sso?authorization_request=x".to_owned(),
            ),
        })]);
        let check = check_forge(
            &target(ForgeKind::Github, command("echo tok")),
            &resolver(),
            &fake,
        );
        assert_eq!(
            check.result,
            ForgeCheckOutcome::SsoRequired {
                url: Some("https://github.com/orgs/acme/sso?authorization_request=x".to_owned())
            }
        );
        let fake = Fake::new(vec![Err(ApiFailure::Transport(
            "connection refused, Bearer tok".to_owned(),
        ))]);
        let check = check_forge(
            &target(ForgeKind::Github, command("echo tok")),
            &resolver(),
            &fake,
        );
        assert_eq!(
            check.result,
            ForgeCheckOutcome::Unreachable {
                error: "connection refused, Bearer <redacted>".to_owned()
            }
        );
    }
}
