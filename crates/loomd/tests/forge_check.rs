//! The forge credential check against a local fake server that serves `fixtures/forge/`.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use loom_api::{CredentialOrigin, ForgeAccess, ForgeCheckOutcome, ForgeKind};
use loom_config::forges::TokenSource;
use loomd::forge::ApiBase;
use loomd::forge::check::{ForgeTarget, check_forge};
use loomd::forge::client::HttpForge;
use loomd::forge::token::TokenResolver;

const TOKEN: &str = "fake-token-0123456789";

/// One answer of the fake server.
#[derive(Clone)]
struct Route {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
}

fn fixture(path: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/forge")
        .join(path);
    std::fs::read_to_string(&path).unwrap()
}

fn route(status: u16, fixture_path: &str) -> Route {
    Route {
        status,
        headers: Vec::new(),
        body: fixture(fixture_path),
    }
}

impl Route {
    fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_owned()));
        self
    }
}

/// Path and `Authorization` header of each request.
type Requests = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// A fake forge on `127.0.0.1`: answers each path from `routes` (404 otherwise), and records
/// the `Authorization` header of each request, `None` when absent.
struct Server {
    base: String,
    seen: Requests,
}

impl Server {
    fn start(host: &str, routes: HashMap<&'static str, Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let mut authorization = None;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':')
                        && name.eq_ignore_ascii_case("authorization")
                    {
                        authorization = Some(value.trim().to_owned());
                    }
                }
                recorded.lock().unwrap().push((path.clone(), authorization));
                let route = routes.get(path.as_str()).cloned().unwrap_or_else(|| Route {
                    status: 404,
                    headers: Vec::new(),
                    body: r#"{"message":"Not Found"}"#.to_owned(),
                });
                let mut answer = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    route.status,
                    route.body.len()
                );
                for (name, value) in &route.headers {
                    write!(answer, "{name}: {value}\r\n").unwrap();
                }
                answer.push_str("\r\n");
                answer.push_str(&route.body);
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        Self {
            base: format!("http://{host}:{port}"),
            seen,
        }
    }

    fn requests(&self) -> Vec<(String, Option<String>)> {
        self.seen.lock().unwrap().clone()
    }
}

fn target(kind: ForgeKind, server: &Server) -> ForgeTarget {
    ForgeTarget {
        context: "personal".parse().unwrap(),
        host: "forge.example".to_owned(),
        kind,
        api: ApiBase {
            rest: server.base.clone(),
            graphql: None,
        },
        origin: CredentialOrigin::Host,
        source: TokenSource::Env("FORGE_TOKEN".to_owned()),
    }
}

fn run(kind: ForgeKind, server: &Server) -> ForgeCheckOutcome {
    let target = target(kind, server);
    let resolver = TokenResolver::new(
        Duration::from_secs(10),
        Box::new(|name| (name == "FORGE_TOKEN").then(|| TOKEN.to_owned())),
    );
    let client = HttpForge::new(kind, target.api.clone(), "loom-test");
    let check = check_forge(&target, &resolver, &client);
    // The token never leaves through the result.
    let printed = format!("{check:?} {}", serde_json::to_string(&check).unwrap());
    assert!(!printed.contains(TOKEN), "{printed}");
    check.result
}

#[test]
fn github_classic_token_with_repo_can_publish() {
    let server = Server::start(
        "127.0.0.1",
        HashMap::from([(
            "/user",
            route(200, "github/user.json").header("x-oauth-scopes", "repo, read:org"),
        )]),
    );
    let result = run(ForgeKind::Github, &server);
    assert!(
        matches!(&result, ForgeCheckOutcome::Authenticated { login, access: ForgeAccess::Write { .. } } if login == "carol"),
        "{result:?}"
    );
    assert_eq!(
        server.requests(),
        [("/user".to_owned(), Some(format!("Bearer {TOKEN}")))]
    );
}

#[test]
fn github_fine_grained_token_has_unknown_scopes() {
    let server = Server::start(
        "127.0.0.1",
        HashMap::from([("/user", route(200, "github/user.json"))]),
    );
    assert!(matches!(
        run(ForgeKind::Github, &server),
        ForgeCheckOutcome::Authenticated {
            access: ForgeAccess::Unknown { .. },
            ..
        }
    ));
}

#[test]
fn a_bad_token_is_rejected_after_one_new_resolution() {
    let server = Server::start(
        "127.0.0.1",
        HashMap::from([("/user", route(401, "github/error-401.json"))]),
    );
    assert_eq!(
        run(ForgeKind::Github, &server),
        ForgeCheckOutcome::Rejected {
            status: 401,
            error: "Bad credentials".to_owned()
        }
    );
    assert_eq!(server.requests().len(), 2);
    let server = Server::start(
        "127.0.0.1",
        HashMap::from([("/user", route(401, "gitlab/error-401.json"))]),
    );
    assert_eq!(
        run(ForgeKind::Gitlab, &server),
        ForgeCheckOutcome::Rejected {
            status: 401,
            error: "401 Unauthorized".to_owned()
        }
    );
}

#[test]
fn gitlab_token_scopes() {
    let server = Server::start(
        "127.0.0.1",
        HashMap::from([
            ("/user", route(200, "gitlab/user.json")),
            (
                "/personal_access_tokens/self",
                route(200, "gitlab/personal-access-token-self.json"),
            ),
        ]),
    );
    assert!(matches!(
        run(ForgeKind::Gitlab, &server),
        ForgeCheckOutcome::Authenticated { ref login, access: ForgeAccess::Write { .. } } if login == "carol"
    ));
    let server = Server::start(
        "127.0.0.1",
        HashMap::from([
            ("/user", route(200, "gitlab/user.json")),
            (
                "/personal_access_tokens/self",
                route(200, "gitlab/personal-access-token-self.read-api.json"),
            ),
        ]),
    );
    assert!(matches!(
        run(ForgeKind::Gitlab, &server),
        ForgeCheckOutcome::Authenticated { access: ForgeAccess::ReadOnly { ref scopes }, .. } if scopes == &["read_api"]
    ));
    // No personal access token endpoint for this token: scopes unknown.
    let server = Server::start(
        "127.0.0.1",
        HashMap::from([("/user", route(200, "gitlab/user.json"))]),
    );
    assert!(matches!(
        run(ForgeKind::Gitlab, &server),
        ForgeCheckOutcome::Authenticated {
            access: ForgeAccess::Unknown { .. },
            ..
        }
    ));
}

#[test]
fn saml_sso_is_reported_with_its_url() {
    let url = "https://github.com/orgs/acme/sso?authorization_request=A1";
    let server = Server::start(
        "127.0.0.1",
        HashMap::from([(
            "/user",
            Route {
                status: 403,
                headers: vec![("x-github-sso", format!("required; url={url}"))],
                body: r#"{"message":"Resource protected by organization SAML enforcement."}"#
                    .to_owned(),
            },
        )]),
    );
    assert_eq!(
        run(ForgeKind::Github, &server),
        ForgeCheckOutcome::SsoRequired {
            url: Some(url.to_owned())
        }
    );
}

#[test]
fn the_token_does_not_follow_a_redirect_to_another_host() {
    // `localhost` and `127.0.0.1` are two hosts for the client.
    let other = Server::start(
        "localhost",
        HashMap::from([("/user", route(200, "github/user.json"))]),
    );
    let first = Server::start(
        "127.0.0.1",
        HashMap::from([(
            "/user",
            Route {
                status: 302,
                headers: vec![("Location", format!("{}/user", other.base))],
                body: String::new(),
            },
        )]),
    );
    let _ = run(ForgeKind::Github, &first);
    let reached = other.requests();
    assert_eq!(reached.len(), 1);
    assert!(
        reached
            .iter()
            .all(|(_, authorization)| authorization.is_none())
    );
}

#[test]
fn an_unreachable_forge_is_reported() {
    // Nothing listens on this port once the listener is dropped.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let server = Server {
        base: format!("http://127.0.0.1:{port}"),
        seen: Arc::default(),
    };
    assert!(matches!(
        run(ForgeKind::Github, &server),
        ForgeCheckOutcome::Unreachable { .. }
    ));
}
