//! The GitHub GraphQL calls against the fake forge, which serves `fixtures/forge/github/`.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

mod common;

use common::fake_forge::{FakeForge, Recorded, Route, fixture};
use common::{V1, V2, edit, sha};
use loom_api::ForgeKind;
use loom_forge::client::{
    ApiBase, ForgeError, GithubClient, GithubForge, GraphqlStep, NewThread, REDACTED,
    RateLimitWait, Token,
};
use loom_forge::github::ReviewState;
use loom_forge::model::{Anchor, MrKey, Side, ThreadSubject, VerdictKind};

const TOKEN: &str = "ghp_fake0123456789";
const PULL_ID: &str = "PR_kwDOAAATic4AA9-c";
const REVIEW_ID: &str = "PRR_kwDOAAATic4AABd9";
const THREAD_ID: &str = "PRRT_kwDOAAATic5AAAAB";

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

fn json(path: &str) -> serde_json::Value {
    serde_json::from_slice(&fixture(path)).unwrap()
}

/// The single request of `forge`, checked as a GraphQL `POST` with the token.
fn only_request(forge: &FakeForge) -> Recorded {
    let requests = forge.requests();
    assert_eq!(requests.len(), 1, "{:?}", forge.lines());
    let request = requests[0].clone();
    assert_eq!(request.line(), "POST /graphql");
    assert_eq!(
        request.header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(request.header("content-type"), Some("application/json"));
    request
}

fn anchor(start_line: u32, end_line: u32) -> Anchor {
    Anchor {
        path: "src/auth/jwt.rs".to_owned(),
        side: Side::New,
        start_line,
        end_line,
        sha: sha(V2),
    }
}

#[test]
fn add_review_sends_the_fixture_request_exactly() {
    let forge = FakeForge::start();
    forge.route(
        Route::post("/graphql")
            .when_body("addPullRequestReview(")
            .fixture("github/graphql-add-review.response.json"),
    );
    let expected = json("github/graphql-add-review.request.json");
    let bodies: Vec<&str> = expected["variables"]["input"]["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|thread| thread["body"].as_str().unwrap())
        .collect();
    let (one, range) = (anchor(14, 14), anchor(9, 11));
    let review = client(&forge)
        .add_review(
            PULL_ID,
            &sha(V2),
            &[
                NewThread {
                    anchor: &one,
                    body: bodies[0],
                },
                NewThread {
                    anchor: &range,
                    body: bodies[1],
                },
            ],
        )
        .unwrap();
    assert_eq!(review, REVIEW_ID);
    let request = only_request(&forge);
    assert_eq!(request.json(), expected);
    assert_eq!(request.json()["query"], expected["query"]);
}

#[test]
fn add_reply_sends_the_fixture_request_exactly() {
    let forge = FakeForge::start();
    forge.route(
        Route::post("/graphql")
            .when_body("addPullRequestReviewThreadReply(")
            .fixture("github/graphql-add-reply.response.json"),
    );
    let comment = client(&forge)
        .add_reply(REVIEW_ID, THREAD_ID, "Thanks, the leeway fixes it.")
        .unwrap();
    assert_eq!(comment, "PRRC_kwDOAAATic5AAAAG");
    assert_eq!(
        only_request(&forge).json(),
        json("github/graphql-add-reply.request.json")
    );
}

#[test]
fn submit_review_sends_the_fixture_request_exactly() {
    let forge = FakeForge::start();
    forge.route(
        Route::post("/graphql")
            .when_body("submitPullRequestReview(")
            .fixture("github/graphql-submit-review.response.json"),
    );
    let submitted = client(&forge)
        .submit_review(
            REVIEW_ID,
            VerdictKind::RequestChanges,
            Some("One rename, then good to go."),
        )
        .unwrap();
    assert_eq!(submitted.state, ReviewState::ChangesRequested);
    assert_eq!(
        submitted.url,
        "https://github.com/acme/shop/pull/412#pullrequestreview-6002"
    );
    assert_eq!(
        only_request(&forge).json(),
        json("github/graphql-submit-review.request.json")
    );
}

#[test]
fn a_verdict_without_a_summary_sends_no_body() {
    let forge = FakeForge::start();
    forge.route(Route::post("/graphql").fixture("github/graphql-submit-review.response.json"));
    client(&forge)
        .submit_review(REVIEW_ID, VerdictKind::Approve, None)
        .unwrap();
    assert_eq!(
        only_request(&forge).json()["variables"]["input"],
        serde_json::json!({"pullRequestReviewId": REVIEW_ID, "event": "APPROVE"})
    );
}

#[test]
fn resolve_thread_sends_the_fixture_request_exactly() {
    let forge = FakeForge::start();
    forge.route(
        Route::post("/graphql")
            .when_body("resolveReviewThread(")
            .fixture("github/graphql-resolve-thread.response.json"),
    );
    assert_eq!(client(&forge).resolve_thread(THREAD_ID).unwrap(), THREAD_ID);
    assert_eq!(
        only_request(&forge).json(),
        json("github/graphql-resolve-thread.request.json")
    );
}

#[test]
fn pending_reviews_of_the_viewer() {
    let forge = FakeForge::start();
    forge
        .route(
            Route::post("/graphql")
                .when_body("reviews(states: PENDING")
                .fixture("github/graphql-pending-reviews.json")
                .times(1),
        )
        .route(
            Route::post("/graphql")
                .when_body("reviews(states: PENDING")
                .fixture("github/graphql-pending-reviews.empty.json"),
        );
    let client = client(&forge);
    let pending = client.pending_reviews(&key()).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].author, "carol");
    assert_eq!(pending[0].comment_count, 2);
    assert_eq!(client.pending_reviews(&key()).unwrap().len(), 0);
    let variables = forge.requests()[0].json()["variables"].clone();
    assert_eq!(
        variables,
        serde_json::json!({"owner": "acme", "name": "shop", "number": 412})
    );
}

#[test]
fn review_threads_follow_thread_and_comment_pages() {
    let forge = FakeForge::start();
    let all = fixture("github/graphql-review-threads.json");
    let threads = "/data/repository/pullRequest/reviewThreads";
    // Page 1: bob's question, with more comments than the page gives; page 2: the nitpick.
    let page_1 = edit(&all, |body| {
        let connection = body.pointer_mut(threads).unwrap();
        connection["pageInfo"] = serde_json::json!({"hasNextPage": true, "endCursor": "T1"});
        let nodes = connection["nodes"].as_array_mut().unwrap();
        nodes.truncate(1);
        let comments = &mut nodes[0]["comments"];
        comments["pageInfo"] = serde_json::json!({"hasNextPage": true, "endCursor": "C1"});
        comments["nodes"].as_array_mut().unwrap().truncate(1);
    });
    let page_2 = edit(&all, |body| {
        body.pointer_mut(threads).unwrap()["nodes"]
            .as_array_mut()
            .unwrap()
            .remove(0);
    });
    // The comments of the question, from the start, in two pages.
    let comments = serde_json::from_slice::<serde_json::Value>(&all).unwrap()["data"]["repository"]
        ["pullRequest"]["reviewThreads"]["nodes"][0]["comments"]["nodes"]
        .clone();
    let comments_page = |nodes: serde_json::Value, next: Option<&str>| {
        serde_json::to_vec(&serde_json::json!({"data": {"node": {"comments": {
            "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next},
            "nodes": nodes,
        }}}}))
        .unwrap()
    };
    forge
        .route(
            Route::post("/graphql")
                .when_body("reviewThreads(")
                .when_body(r#""cursor":"T1""#)
                .body(page_2),
        )
        .route(
            Route::post("/graphql")
                .when_body("reviewThreads(")
                .body(page_1),
        )
        .route(
            Route::post("/graphql")
                .when_body("PullRequestReviewThread")
                .when_body(r#""cursor":"C2""#)
                .body(comments_page(
                    serde_json::json!([comments[1].clone()]),
                    None,
                )),
        )
        .route(
            Route::post("/graphql")
                .when_body("PullRequestReviewThread")
                .body(comments_page(
                    serde_json::json!([comments[0].clone()]),
                    Some("C2"),
                )),
        );
    let threads = client(&forge).review_threads(&key()).unwrap();
    assert_eq!(threads.pull_request_id, PULL_ID);
    assert_eq!(threads.head_sha, sha(V2));
    let ids: Vec<_> = threads
        .threads
        .iter()
        .map(|thread| thread.id.as_str())
        .collect();
    assert_eq!(ids, [THREAD_ID, "PRRT_kwDOAAATic5AAAAE"]);
    let question = &threads.threads[0];
    let authors: Vec<_> = question
        .comments
        .iter()
        .map(|comment| comment.author.as_str())
        .collect();
    assert_eq!(authors, ["bob", "alice"]);
    assert!(question.outdated);
    assert!(matches!(
        &question.subject,
        ThreadSubject::Lines { anchor } if anchor.end_line == 10 && anchor.sha == sha(V1)
    ));

    let variables: Vec<_> = forge
        .requests()
        .iter()
        .map(|request| request.json()["variables"].clone())
        .collect();
    assert_eq!(
        variables,
        [
            serde_json::json!({"owner": "acme", "name": "shop", "number": 412}),
            serde_json::json!({"id": THREAD_ID}),
            serde_json::json!({"id": THREAD_ID, "cursor": "C2"}),
            serde_json::json!({"owner": "acme", "name": "shop", "number": 412, "cursor": "T1"}),
        ]
    );
}

#[test]
fn errors_with_a_200_are_errors_of_their_step() {
    for (path, step) in [
        (
            "github/graphql-error-one-pending-review.json",
            GraphqlStep::AddReview,
        ),
        (
            "github/graphql-error-line-not-in-diff.json",
            GraphqlStep::AddReview,
        ),
    ] {
        let forge = FakeForge::start();
        forge.route(Route::post("/graphql").fixture(path));
        let one = anchor(14, 14);
        let error = client(&forge)
            .add_review(
                PULL_ID,
                &sha(V2),
                &[NewThread {
                    anchor: &one,
                    body: "b",
                }],
            )
            .unwrap_err();
        let ForgeError::Graphql { step: got, errors } = &error else {
            panic!("{path}: {error:?}");
        };
        assert_eq!(*got, step, "{path}");
        assert_eq!(errors[0].kind.as_deref(), Some("UNPROCESSABLE"), "{path}");
        assert_eq!(errors[0].path, ["addPullRequestReview"], "{path}");
    }
}

#[test]
fn errors_of_a_read_name_the_read() {
    let forge = FakeForge::start();
    forge.route(Route::post("/graphql").body(
        r#"{"data":{"repository":null},"errors":[{"type":"NOT_FOUND","path":["repository"],"message":"Could not resolve to a Repository with the name 'acme/shop'."}]}"#,
    ));
    let error = client(&forge).review_threads(&key()).unwrap_err();
    assert!(
        matches!(&error, ForgeError::Graphql { step: GraphqlStep::ReviewThreads, errors }
            if errors[0].kind.as_deref() == Some("NOT_FOUND")),
        "{error:?}"
    );
}

#[test]
fn the_point_based_rate_limit_waits_for_the_reset() {
    let forge = FakeForge::start();
    forge.route(
        Route::post("/graphql")
            .body(r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded for user ID 1003."}]}"#)
            .header("x-ratelimit-remaining", "0")
            .header("x-ratelimit-reset", "1791792000"),
    );
    let error = client(&forge).pending_reviews(&key()).unwrap_err();
    assert!(
        matches!(
            error,
            ForgeError::RateLimited {
                status: 200,
                wait: RateLimitWait::Until(_),
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn error_statuses_of_the_graphql_endpoint() {
    let forge = FakeForge::start();
    forge
        .route(
            Route::post("/graphql")
                .when_body("resolveReviewThread(")
                .status(401)
                .fixture("github/error-401.json"),
        )
        .route(
            Route::post("/graphql")
                .status(403)
                .fixture("github/error-403-rate-limit.json")
                .header("x-ratelimit-remaining", "0")
                .header("x-ratelimit-reset", "1791792000"),
        );
    let client = client(&forge);
    let error = client.resolve_thread(THREAD_ID).unwrap_err();
    assert!(
        matches!(error, ForgeError::Unauthorized { .. }),
        "{error:?}"
    );
    let error = client.add_reply(REVIEW_ID, THREAD_ID, "b").unwrap_err();
    assert!(
        matches!(error, ForgeError::RateLimited { status: 403, .. }),
        "{error:?}"
    );
}

#[test]
fn a_host_without_graphql_sends_nothing() {
    let forge = FakeForge::start();
    let base = ApiBase {
        rest: forge.base().to_owned(),
        graphql: None,
    };
    let client = GithubClient::new(&base, Token::new(TOKEN.to_owned()), "loom-test");
    let error = client.resolve_thread(THREAD_ID).unwrap_err();
    assert!(matches!(error, ForgeError::Unexpected(_)), "{error:?}");
    assert_eq!(forge.requests().len(), 0);
}

#[test]
fn a_gitlab_key_is_refused_before_any_request() {
    let forge = FakeForge::start();
    let gitlab = MrKey::new(ForgeKind::Gitlab, "company-a/checkout", 88).unwrap();
    let error = client(&forge).review_threads(&gitlab).unwrap_err();
    assert!(matches!(error, ForgeError::Unexpected(_)), "{error:?}");
    assert_eq!(forge.requests().len(), 0);
}

#[test]
fn the_token_is_in_no_graphql_error() {
    let forge = FakeForge::start();
    forge
        .route(Route::post("/graphql").when_body("resolveReviewThread(").body(format!(
            r#"{{"errors":[{{"type":"FORBIDDEN","path":["{TOKEN}"],"message":"no access for {TOKEN}"}}]}}"#
        )))
        .route(Route::post("/graphql").when_body("addPullRequestReviewThreadReply(").body(
            format!(r#"{{"data":{{"addPullRequestReviewThreadReply":{{"comment":"{TOKEN}"}}}}}}"#),
        ))
        .route(Route::post("/graphql").body(format!(
            r#"{{"errors":[{{"type":"RATE_LIMITED","message":"{TOKEN}"}}]}}"#
        )));
    let client = client(&forge);
    for error in [
        client.resolve_thread(THREAD_ID).unwrap_err(),
        client.add_reply(REVIEW_ID, THREAD_ID, "b").unwrap_err(),
        client.pending_reviews(&key()).unwrap_err(),
    ] {
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains(TOKEN), "{shown}");
        assert!(
            shown.contains(REDACTED) || matches!(error, ForgeError::Parse(_)),
            "{shown}"
        );
    }
}
