//! The GitHub REST client against the fake forge, which serves `fixtures/forge/github/`.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

mod common;

use common::fake_forge::{FakeForge, Route};
use common::{V2, edit, sha};
use loom_api::{ForgeKind, Timestamp};
use loom_forge::client::{
    ApiBase, Conditional, Forge, ForgeError, GithubClient, GithubForge, REDACTED, RateLimitWait,
    ReviewRequests, Token,
};
use loom_forge::model::{MrKey, MrState};

const TOKEN: &str = "ghp_fake0123456789";
const PULL: &str = "/repos/acme/shop/pulls/412";
const FILES: &str = "/repos/acme/shop/pulls/412/files?per_page=100";
const REVIEWS: &str = "/repos/acme/shop/pulls/412/reviews?per_page=100";
const COMMENTS: &str = "/repos/acme/shop/issues/412/comments?per_page=100";
const SEARCH: &str =
    "/search/issues?q=is:pr+is:open+archived:false+review-requested:@me&per_page=100";

fn client(forge: &FakeForge) -> GithubClient {
    let base = ApiBase {
        rest: forge.base().to_owned(),
        graphql: Some(format!("{}/graphql", forge.base())),
    };
    GithubClient::new(&base, Token::new(TOKEN.to_owned()), "loom-test")
}

fn key() -> MrKey {
    MrKey::new(ForgeKind::Github, "acme/shop", 412).unwrap()
}

/// A fake forge that serves every GitHub REST fixture.
fn github() -> FakeForge {
    let forge = FakeForge::start();
    forge
        .route(
            Route::get("/user")
                .fixture("github/user.json")
                .header("x-oauth-scopes", "read:org, repo"),
        )
        .route(Route::get(SEARCH).fixture("github/search-review-requested.json"))
        .route(
            Route::get(PULL)
                .fixture("github/pull-412.v2.json")
                .header("etag", "W/\"v2\""),
        )
        .route(Route::get(FILES).fixture("github/pull-412-files.v2.json"))
        .route(Route::get(REVIEWS).fixture("github/pull-412-reviews.json"))
        .route(Route::get(COMMENTS).fixture("github/issue-412-comments.json"));
    forge
}

#[test]
fn each_rest_request_of_the_fixtures_is_sent_with_its_method_path_and_query() {
    let forge = github();
    let client = client(&forge);
    let user = client.user().unwrap();
    assert_eq!(user.user.login, "carol");
    assert_eq!(user.scopes_header.as_deref(), Some("read:org, repo"));
    let search = client.review_requests(ReviewRequests::Teams).unwrap();
    assert_eq!(search.items.len(), 1);
    assert_eq!(search.items[0].key, key());
    let Conditional::Modified { value: pull, etag } = client.pull(&key(), None).unwrap() else {
        panic!("not modified without an etag");
    };
    assert_eq!(pull.version.head_sha, sha(V2));
    assert_eq!(pull.state, MrState::Open);
    assert_eq!(etag.as_deref(), Some("W/\"v2\""));
    assert_eq!(client.files(&key()).unwrap().len(), 3);
    assert_eq!(client.reviews(&key()).unwrap().len(), 1);
    assert_eq!(client.issue_comments(&key()).unwrap().len(), 1);

    assert_eq!(
        forge.lines(),
        [
            "GET /user".to_owned(),
            format!("GET {SEARCH}"),
            format!("GET {PULL}"),
            format!("GET {FILES}"),
            format!("GET {REVIEWS}"),
            format!("GET {COMMENTS}"),
        ]
    );
    for request in forge.requests() {
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        assert_eq!(
            request.header("accept"),
            Some("application/vnd.github+json")
        );
        assert_eq!(request.header("x-github-api-version"), Some("2022-11-28"));
        assert_eq!(request.header("user-agent"), Some("loom-test"));
        assert_eq!(request.header("if-none-match"), None);
    }
}

#[test]
fn direct_review_requests_use_their_own_qualifier() {
    let forge = FakeForge::start();
    let direct =
        "/search/issues?q=is:pr+is:open+archived:false+user-review-requested:@me&per_page=100";
    forge.route(Route::get(direct).fixture("github/search-review-requested.empty.json"));
    let search = client(&forge)
        .review_requests(ReviewRequests::Direct)
        .unwrap();
    assert_eq!(search.items.len(), 0);
    assert_eq!(forge.lines(), [format!("GET {direct}")]);
}

#[test]
fn a_304_is_not_modified_and_its_body_is_not_parsed() {
    let forge = FakeForge::start();
    forge
        .route(
            Route::get(PULL)
                .when("if-none-match", "W/\"v2\"")
                .status(304)
                .body("not JSON: a body here must not be parsed"),
        )
        .route(
            Route::get(PULL)
                .fixture("github/pull-412.v2.json")
                .header("etag", "W/\"v3\""),
        );
    let client = client(&forge);
    assert_eq!(
        client.pull(&key(), Some("W/\"v2\"")).unwrap(),
        Conditional::NotModified
    );
    // Another etag: the pull request changed.
    assert!(matches!(
        client.pull(&key(), Some("W/\"v1\"")).unwrap(),
        Conditional::Modified { etag: Some(etag), .. } if etag == "W/\"v3\""
    ));
    let sent: Vec<_> = forge
        .requests()
        .iter()
        .map(|request| request.header("if-none-match").map(str::to_owned))
        .collect();
    assert_eq!(
        sent,
        [Some("W/\"v2\"".to_owned()), Some("W/\"v1\"".to_owned())]
    );
}

#[test]
fn two_pages_are_joined_through_link() {
    let forge = FakeForge::start();
    let all = common::fake_forge::fixture("github/pull-412-files.v2.json");
    let first = edit(&all, |files| {
        files.as_array_mut().unwrap().truncate(2);
    });
    let second = edit(&all, |files| {
        files.as_array_mut().unwrap().drain(..2);
    });
    let page_2 = format!("{FILES}&page=2");
    forge
        .route(Route::get(FILES).body(first).header(
            "link",
            &format!(r#"<{{base}}{page_2}>; rel="next", <{{base}}{page_2}>; rel="last""#),
        ))
        .route(Route::get(&page_2).body(second).header(
            "link",
            &format!(r#"<{{base}}{FILES}>; rel="prev", <{{base}}{FILES}>; rel="first""#),
        ));
    let files = client(&forge).files(&key()).unwrap();
    let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["Cargo.lock", "src/api/login.rs", "src/auth/jwt.rs"]);
    assert_eq!(
        forge.lines(),
        [format!("GET {FILES}"), format!("GET {page_2}")]
    );
    // The token goes to the next page too: it is on the same host.
    assert!(
        forge
            .requests()
            .iter()
            .all(|request| request.header("authorization").is_some())
    );
}

#[test]
fn search_pages_are_joined() {
    let forge = FakeForge::start();
    let page_2 = format!("{SEARCH}&page=2");
    forge
        .route(
            Route::get(SEARCH)
                .fixture("github/search-review-requested.json")
                .header("link", &format!(r#"<{{base}}{page_2}>; rel="next""#)),
        )
        .route(Route::get(&page_2).fixture("github/search-review-requested.json"));
    let search = client(&forge)
        .review_requests(ReviewRequests::Teams)
        .unwrap();
    assert_eq!(search.items.len(), 2);
    assert_eq!(search.total_count, 1);
}

#[test]
fn a_next_page_on_another_host_is_not_followed() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(FILES)
            .fixture("github/pull-412-files.v2.json")
            .header(
                "link",
                r#"<https://evil.example/repos/acme/shop/pulls/412/files?page=2>; rel="next""#,
            ),
    );
    let error = client(&forge).files(&key()).unwrap_err();
    assert!(matches!(error, ForgeError::Unexpected(_)), "{error:?}");
    assert_eq!(forge.requests().len(), 1);
}

#[test]
fn a_next_page_that_loops_stops_after_the_page_limit() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(FILES)
            .fixture("github/pull-412-files.v2.json")
            .header("link", &format!(r#"<{{base}}{FILES}>; rel="next""#)),
    );
    let error = client(&forge).files(&key()).unwrap_err();
    assert!(matches!(error, ForgeError::Unexpected(_)), "{error:?}");
    assert_eq!(forge.requests().len(), 100);
}

#[test]
fn a_401_is_unauthorized() {
    let forge = FakeForge::start();
    forge.route(
        Route::get("/user")
            .status(401)
            .fixture("github/error-401.json"),
    );
    let error = client(&forge).user().unwrap_err();
    assert!(
        matches!(&error, ForgeError::Unauthorized { message } if message == "Bad credentials"),
        "{error:?}"
    );
}

#[test]
fn a_403_rate_limit_waits_for_the_reset() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(PULL)
            .status(403)
            .fixture("github/error-403-rate-limit.json")
            .header("x-ratelimit-limit", "5000")
            .header("x-ratelimit-remaining", "0")
            .header("x-ratelimit-reset", "1791792000"),
    );
    let error = client(&forge).pull(&key(), None).unwrap_err();
    let reset = Timestamp::from_unix_secs(1_791_792_000).unwrap();
    assert!(
        matches!(
            error,
            ForgeError::RateLimited {
                status: 403,
                wait: RateLimitWait::Until(at),
                ..
            } if at == reset
        ),
        "{error:?}"
    );
}

#[test]
fn a_429_waits_for_retry_after() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(SEARCH)
            .status(429)
            .fixture("github/error-403-rate-limit.json")
            .header("retry-after", "60")
            .header("x-ratelimit-reset", "1791792000"),
    );
    let error = client(&forge)
        .review_requests(ReviewRequests::Teams)
        .unwrap_err();
    assert!(
        matches!(
            error,
            ForgeError::RateLimited { status: 429, wait: RateLimitWait::After(wait), .. }
                if wait.as_secs() == 60
        ),
        "{error:?}"
    );
}

#[test]
fn a_403_with_sso_names_the_authorization_url() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(PULL)
            .status(403)
            .body(r#"{"message":"Resource protected by organization SAML enforcement."}"#)
            .header(
                "x-github-sso",
                "required; url=https://github.com/orgs/acme/sso?authorization_request=A1",
            ),
    );
    let error = client(&forge).pull(&key(), None).unwrap_err();
    assert!(
        matches!(&error, ForgeError::SsoRequired { url: Some(url) }
            if url == "https://github.com/orgs/acme/sso?authorization_request=A1"),
        "{error:?}"
    );
}

#[test]
fn a_plain_403_is_forbidden() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(REVIEWS)
            .status(403)
            .body(r#"{"message":"Resource not accessible by personal access token"}"#)
            .header("x-ratelimit-remaining", "4999"),
    );
    let error = client(&forge).reviews(&key()).unwrap_err();
    assert!(matches!(error, ForgeError::Forbidden { .. }), "{error:?}");
}

#[test]
fn a_404_is_not_found() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(COMMENTS)
            .status(404)
            .body(r#"{"message":"Not Found"}"#),
    );
    let error = client(&forge).issue_comments(&key()).unwrap_err();
    assert!(matches!(error, ForgeError::NotFound { .. }), "{error:?}");
}

#[test]
fn a_422_keeps_its_validation_errors() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(REVIEWS)
            .status(422)
            .fixture("github/error-422-own-pull.json"),
    );
    let error = client(&forge).reviews(&key()).unwrap_err();
    assert!(
        matches!(&error, ForgeError::Unprocessable { details, .. }
            if details == &["Review Can not approve your own pull request"]),
        "{error:?}"
    );
}

#[test]
fn an_error_on_a_later_page_fails_the_list() {
    let forge = FakeForge::start();
    let page_2 = format!("{FILES}&page=2");
    forge
        .route(
            Route::get(FILES)
                .fixture("github/pull-412-files.v2.json")
                .header("link", &format!(r#"<{{base}}{page_2}>; rel="next""#)),
        )
        .route(
            Route::get(&page_2)
                .status(502)
                .body("<html>Bad gateway</html>"),
        );
    let error = client(&forge).files(&key()).unwrap_err();
    assert!(
        matches!(error, ForgeError::Status { status: 502, .. }),
        "{error:?}"
    );
}

#[test]
fn a_body_that_is_not_the_payload_is_a_parse_error() {
    let forge = FakeForge::start();
    forge.route(Route::get("/user").body("[]"));
    let error = client(&forge).user().unwrap_err();
    assert!(matches!(error, ForgeError::Parse(_)), "{error:?}");
}

#[test]
fn no_answer_is_a_transport_error() {
    let base = ApiBase {
        // Port 9 (discard) of `127.0.0.1` refuses connections on test machines.
        rest: "http://127.0.0.1:9".to_owned(),
        graphql: None,
    };
    let client = GithubClient::new(&base, Token::new(TOKEN.to_owned()), "loom-test");
    let error = client.user().unwrap_err();
    assert!(matches!(error, ForgeError::Transport(_)), "{error:?}");
}

#[test]
fn a_gitlab_key_is_refused_before_any_request() {
    let forge = FakeForge::start();
    let gitlab = MrKey::new(ForgeKind::Gitlab, "company-a/checkout", 88).unwrap();
    let error = client(&forge).files(&gitlab).unwrap_err();
    assert!(matches!(error, ForgeError::Unexpected(_)), "{error:?}");
    assert_eq!(forge.requests().len(), 0);
}

#[test]
fn the_token_is_in_no_debug_output_and_no_error() {
    let forge = FakeForge::start();
    // A forge that echoes the token in its error message.
    forge
        .route(
            Route::get("/user")
                .status(401)
                .body(format!(r#"{{"message":"Bad credentials: {TOKEN}"}}"#)),
        )
        .route(
            Route::get(PULL)
                .status(422)
                .body(format!(r#"{{"message":"x","errors":["{TOKEN}"]}}"#)),
        )
        .route(
            Route::get(REVIEWS)
                .status(403)
                .body(format!(r#"{{"message":"{TOKEN}"}}"#))
                .header("x-ratelimit-remaining", "0"),
        );
    let client = client(&forge);
    assert!(!format!("{client:?}").contains(TOKEN));
    assert!(format!("{client:?}").contains(REDACTED));
    let errors = [
        client.user().unwrap_err(),
        client.pull(&key(), None).unwrap_err(),
        client.reviews(&key()).unwrap_err(),
    ];
    for error in &errors {
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains(TOKEN), "{shown}");
        assert!(shown.contains(REDACTED), "{shown}");
    }
    let refused = GithubClient::new(
        &ApiBase {
            rest: format!("http://127.0.0.1:9/{TOKEN}"),
            graphql: None,
        },
        Token::new(TOKEN.to_owned()),
        "loom-test",
    )
    .user()
    .unwrap_err();
    assert!(!format!("{refused} {refused:?}").contains(TOKEN));
}
