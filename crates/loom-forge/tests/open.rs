//! The requests of `loom pr` on both clients, against the fake forge. A created MR is answered
//! with the MR fixtures of the spike, made drafts.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

mod common;

use common::edit;
use common::fake_forge::{FakeForge, Route, fixture};
use loom_api::ForgeKind;
use loom_forge::client::{ApiBase, ForgeError, GithubClient, GitlabClient, OpenForge, Token};
use loom_forge::model::{MrKey, MrState};
use loom_forge::open::NewMr;
use serde_json::json;

const TOKEN: &str = "fake-token-0123456789";

fn base(forge: &FakeForge) -> ApiBase {
    ApiBase {
        rest: forge.base().to_owned(),
        graphql: None,
    }
}

fn new_mr(project: &str) -> NewMr {
    NewMr {
        project: project.to_owned(),
        head: "auth-refactor".to_owned(),
        base: "main".to_owned(),
        title: "Refactor the auth".to_owned(),
        body: "## Notes\n".to_owned(),
    }
}

fn draft_pull() -> Vec<u8> {
    edit(&fixture("github/pull-412.v1.json"), |pull| {
        pull["draft"] = json!(true);
    })
}

fn draft_merge_request() -> Vec<u8> {
    edit(&fixture("gitlab/merge-request-88.v1.json"), |mr| {
        mr["draft"] = json!(true);
        // GitLab computes the diff refs after the answer to the creation.
        mr["diff_refs"] = json!(null);
    })
}

const GITHUB_FIND: &str =
    "/repos/acme/shop/pulls?head=acme%3Aauth-refactor&state=open&per_page=100";
const GITLAB_FIND: &str = "/projects/company-a%2Fcheckout/merge_requests?source_branch=auth-refactor&state=opened&per_page=100";

#[test]
fn github_finds_the_open_pull_request_of_a_branch() {
    let forge = FakeForge::start();
    let list = format!("[{}]", String::from_utf8(draft_pull()).unwrap());
    forge.route(Route::get(GITHUB_FIND).body(list));
    let client = GithubClient::new(&base(&forge), Token::new(TOKEN.to_owned()), "loom-test");
    let found = client
        .open_mrs_of_branch("acme/shop", "auth-refactor")
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].key,
        MrKey::new(ForgeKind::Github, "acme/shop", 412).unwrap()
    );
    assert_eq!(found[0].web_url, "https://github.com/acme/shop/pull/412");
    assert_eq!(found[0].state, MrState::Open);
    assert_eq!(forge.lines(), [format!("GET {GITHUB_FIND}")]);
}

#[test]
fn github_opens_a_draft_pull_request() {
    let forge = FakeForge::start();
    forge.route(
        Route::post("/repos/acme/shop/pulls")
            .status(201)
            .body(draft_pull()),
    );
    let client = GithubClient::new(&base(&forge), Token::new(TOKEN.to_owned()), "loom-test");
    let created = client.create_draft_mr(&new_mr("acme/shop")).unwrap();
    assert!(created.draft);
    assert_eq!(created.key.to_string(), "acme/shop#412");
    let requests = forge.requests();
    assert_eq!(requests[0].line(), "POST /repos/acme/shop/pulls");
    assert_eq!(
        requests[0].json(),
        json!({
            "title": "Refactor the auth",
            "head": "auth-refactor",
            "base": "main",
            "body": "## Notes\n",
            "draft": true,
        })
    );
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
}

#[test]
fn github_refuses_a_second_pull_request_of_a_branch_with_422() {
    let forge = FakeForge::start();
    forge.route(
        Route::post("/repos/acme/shop/pulls")
            .status(422)
            .body(r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for acme:auth-refactor."}]}"#),
    );
    let client = GithubClient::new(&base(&forge), Token::new(TOKEN.to_owned()), "loom-test");
    let error = client.create_draft_mr(&new_mr("acme/shop")).unwrap_err();
    assert!(
        matches!(&error, ForgeError::Unprocessable { details, .. } if details.iter().any(|detail| detail.contains("already exists"))),
        "{error:?}"
    );
}

#[test]
fn gitlab_finds_the_open_merge_request_of_a_branch() {
    let forge = FakeForge::start();
    let list = format!("[{}]", String::from_utf8(draft_merge_request()).unwrap());
    forge.route(Route::get(GITLAB_FIND).body(list));
    let client = GitlabClient::new(&base(&forge), Token::new(TOKEN.to_owned()), "loom-test");
    let found = client
        .open_mrs_of_branch("company-a/checkout", "auth-refactor")
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].key.to_string(), "company-a/checkout!88");
    assert_eq!(forge.lines(), [format!("GET {GITLAB_FIND}")]);
}

#[test]
fn gitlab_opens_a_draft_merge_request_without_diff_refs_yet() {
    let forge = FakeForge::start();
    forge.route(
        Route::post("/projects/company-a%2Fcheckout/merge_requests")
            .status(201)
            .body(draft_merge_request()),
    );
    let client = GitlabClient::new(&base(&forge), Token::new(TOKEN.to_owned()), "loom-test");
    let created = client
        .create_draft_mr(&new_mr("company-a/checkout"))
        .unwrap();
    assert!(created.draft);
    assert_eq!(
        created.web_url,
        "https://gitlab.example.com/company-a/checkout/-/merge_requests/88"
    );
    assert_eq!(
        forge.requests()[0].json(),
        json!({
            "source_branch": "auth-refactor",
            "target_branch": "main",
            "title": "Draft: Refactor the auth",
            "description": "## Notes\n",
        })
    );
}

#[test]
fn gitlab_refuses_a_second_merge_request_of_a_branch_with_409() {
    let forge = FakeForge::start();
    forge.route(
        Route::post("/projects/company-a%2Fcheckout/merge_requests")
            .status(409)
            .body(r#"{"message":["Another open merge request already exists for this source branch: !88"]}"#),
    );
    let client = GitlabClient::new(&base(&forge), Token::new(TOKEN.to_owned()), "loom-test");
    let error = client
        .create_draft_mr(&new_mr("company-a/checkout"))
        .unwrap_err();
    assert!(matches!(error, ForgeError::Conflict { .. }), "{error:?}");
}
