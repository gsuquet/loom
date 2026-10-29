//! The GitLab REST client against the fake forge, which serves `fixtures/forge/gitlab/`.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

mod common;

use common::fake_forge::{FakeForge, Route, fixture};
use common::{M2, V2, edit, sha, time};
use loom_api::{ForgeKind, ForgeProject, ForgeVisibility};
use loom_forge::client::{
    ApiBase, Conditional, Forge, ForgeError, GitlabClient, GitlabForge, GitlabMr, REDACTED, Token,
};
use loom_forge::gitlab::request::{BulkPublishBody, DiffLine, DiffRefs, DraftNoteBody, LineKind};
use loom_forge::model::{MrKey, MrState, ThreadSubject, Verdict, VerdictKind};

const TOKEN: &str = "fake-gitlab-token-0123";
const MRS: &str = "/merge_requests?scope=reviews_for_me&state=opened&updated_after=2026-10-27T08:00:00Z&per_page=100";
const MR: &str = "/projects/77/merge_requests/88";
const VERSIONS: &str = "/projects/77/merge_requests/88/versions?per_page=100";
const DIFFS: &str = "/projects/77/merge_requests/88/diffs?per_page=100";
const DISCUSSIONS: &str = "/projects/77/merge_requests/88/discussions?per_page=2";
const DISCUSSIONS_2: &str = "/projects/77/merge_requests/88/discussions?per_page=2&page=2";
const DRAFTS: &str = "/projects/77/merge_requests/88/draft_notes?per_page=100";
const CREATE: &str = "/projects/77/merge_requests/88/draft_notes";
const PUBLISH: &str = "/projects/77/merge_requests/88/draft_notes/bulk_publish";
const APPROVE: &str = "/projects/77/merge_requests/88/approve";
const APPROVALS: &str = "/projects/77/merge_requests/88/approvals";
/// Bob's question on `src/auth/jwt.rs`, outdated in version 2.
const QUESTION: &str = "6a9c1750b37d513a43987b574953fceb50b03ce7";
const RESOLVE: &str = "/projects/77/merge_requests/88/discussions/6a9c1750b37d513a43987b574953fceb50b03ce7?resolved=true";
const JWT: &str = "src/auth/jwt.rs";
const MR_88: GitlabMr = GitlabMr {
    project_id: 77,
    iid: 88,
};

fn client(forge: &FakeForge) -> GitlabClient {
    let base = ApiBase {
        rest: forge.base().to_owned(),
        graphql: None,
    };
    GitlabClient::new(&base, Token::new(TOKEN.to_owned()), "loom-test")
}

fn key() -> MrKey {
    MrKey::new(ForgeKind::Gitlab, "company-a/checkout", 88).unwrap()
}

/// The fixture `gitlab/<name>` as JSON.
fn json(name: &str) -> serde_json::Value {
    serde_json::from_slice(&fixture(&format!("gitlab/{name}"))).unwrap()
}

/// The `note` of the request fixture `name`.
fn note(name: &str) -> String {
    json(name)["note"].as_str().unwrap().to_owned()
}

const fn line(kind: LineKind, old: u32, new: u32) -> DiffLine {
    DiffLine { kind, old, new }
}

/// A draft note on `src/auth/jwt.rs` with the note of the request fixture `name`.
fn on_jwt(name: &str, refs: &DiffRefs, start: DiffLine, end: DiffLine) -> DraftNoteBody {
    DraftNoteBody::on_lines(&note(name), refs, JWT, JWT, start, end)
}

/// A fake forge that serves every GitLab fixture with a success status.
fn gitlab() -> FakeForge {
    let forge = FakeForge::start();
    forge
        .route(Route::get("/user").fixture("gitlab/user.json"))
        .route(
            Route::get("/personal_access_tokens/self")
                .fixture("gitlab/personal-access-token-self.json"),
        )
        .route(
            Route::get(MRS)
                .fixture("gitlab/merge-requests-reviews-for-me.json")
                .header("x-next-page", ""),
        )
        .route(
            Route::get(MR)
                .fixture("gitlab/merge-request-88.v2.json")
                .header("etag", "W/\"v2\""),
        )
        .route(Route::get(VERSIONS).fixture("gitlab/merge-request-88-versions.json"))
        .route(
            Route::get(DIFFS)
                .fixture("gitlab/merge-request-88-diffs.json")
                .header("x-next-page", ""),
        )
        .route(
            Route::get(DISCUSSIONS)
                .fixture("gitlab/merge-request-88-discussions.page-1.json")
                .header("x-next-page", "2")
                .header("x-total-pages", "2"),
        )
        .route(
            Route::get(DISCUSSIONS_2)
                .fixture("gitlab/merge-request-88-discussions.page-2.json")
                .header("x-next-page", "")
                .header("x-total-pages", "2"),
        )
        .route(Route::get(DRAFTS).fixture("gitlab/draft-notes.empty.json"))
        .route(
            Route::post(CREATE)
                .status(201)
                .fixture("gitlab/draft-note-create.new-line.response.json"),
        )
        .route(Route::post(PUBLISH).status(204).body(""))
        .route(
            Route::post(APPROVE)
                .status(201)
                .fixture("gitlab/approve.response.json"),
        )
        .route(Route::get(APPROVALS).fixture("gitlab/approve.response.json"))
        .route(Route::new("PUT", RESOLVE).body(r#"{"id":"x","notes":[]}"#));
    forge
}

/// Asserts the headers of every request: the token, JSON, no GitHub header, and a JSON body on
/// each `POST` only.
fn assert_headers(forge: &FakeForge) {
    for request in &forge.requests() {
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        assert_eq!(request.header("accept"), Some("application/json"));
        assert_eq!(request.header("user-agent"), Some("loom-test"));
        assert_eq!(request.header("if-none-match"), None);
        assert_eq!(request.header("x-github-api-version"), None);
        if request.method == "POST" {
            assert_eq!(request.header("content-type"), Some("application/json"));
        } else {
            assert!(request.body.is_empty(), "{}", request.line());
        }
    }
}

#[test]
fn each_read_of_the_fixtures_is_sent_with_its_method_path_and_query() {
    let forge = gitlab();
    let client = client(&forge);
    // The discussions are read 2 per page, as in the fixtures, to show two pages.
    let paged = self::client(&forge).with_per_page(2);

    let user = client.user().unwrap();
    assert_eq!((user.user.id, user.user.login.as_str()), (13, "carol"));
    assert_eq!(user.scopes_header, None);
    assert_eq!(client.token_scopes().unwrap().scopes, ["api"]);
    let mrs = client
        .review_requests(Some(time("2026-10-27T08:00:00Z")))
        .unwrap();
    assert_eq!(mrs.len(), 1);
    assert_eq!(mrs[0].key, key());
    let Conditional::Modified { value: mr, etag } = client.merge_request(MR_88, None).unwrap()
    else {
        panic!("not modified without an etag");
    };
    assert_eq!(mr.version.head_sha, sha(V2));
    assert_eq!(mr.state, MrState::Open);
    assert_eq!(etag.as_deref(), Some("W/\"v2\""));
    let versions = client.versions(MR_88).unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(client.diffs(MR_88).unwrap().len(), 3);
    let threads = paged.discussions(MR_88, &sha(V2)).unwrap();
    let ids: Vec<_> = threads.iter().map(|thread| thread.id.as_str()).collect();
    // Both pages, in order; the last discussion of page 2 has only a system note: dropped.
    assert_eq!(
        ids,
        [
            QUESTION,
            "87805b7c09016a7058e91bdbe7b29d1f284a39e6",
            "3f2a0b1c4d5e6f708192a3b4c5d6e7f809102132",
        ]
    );
    assert_eq!(client.draft_notes(MR_88).unwrap(), []);

    assert_eq!(
        forge.lines(),
        [
            "GET /user".to_owned(),
            "GET /personal_access_tokens/self".to_owned(),
            format!("GET {MRS}"),
            format!("GET {MR}"),
            format!("GET {VERSIONS}"),
            format!("GET {DIFFS}"),
            format!("GET {DISCUSSIONS}"),
            format!("GET {DISCUSSIONS_2}"),
            format!("GET {DRAFTS}"),
        ]
    );
    assert_headers(&forge);
}

#[test]
fn each_write_of_the_fixtures_is_sent_with_its_method_path_query_and_body() {
    let forge = gitlab();
    let client = client(&forge);
    let Conditional::Modified { value: mr, .. } = client.merge_request(MR_88, None).unwrap() else {
        panic!("not modified without an etag");
    };
    let refs = DiffRefs::of(&mr.version).unwrap();
    assert_eq!(refs.base_sha, sha(M2));
    let new_line = line(LineKind::Added, 12, 14);
    let context = line(LineKind::Unchanged, 13, 16);
    let notes = [
        on_jwt(
            "draft-note-create.new-line.request.json",
            &refs,
            new_line,
            new_line,
        ),
        on_jwt(
            "draft-note-create.range.request.json",
            &refs,
            line(LineKind::Unchanged, 9, 9),
            line(LineKind::Unchanged, 11, 11),
        ),
        on_jwt(
            "draft-note-create.context-line.request.json",
            &refs,
            context,
            context,
        ),
        DraftNoteBody::reply(
            &note("draft-note-create.reply.request.json"),
            QUESTION,
            true,
        ),
    ];
    let created: Vec<_> = notes
        .iter()
        .map(|body| client.create_draft_note(MR_88, body).unwrap())
        .collect();
    // The fake forge answers every create with the new line's draft note.
    assert_eq!(created[0].id, 901);
    assert!(matches!(
        &created[0].subject,
        Some(ThreadSubject::Lines { anchor }) if anchor.end_line == 14 && anchor.path == JWT
    ));
    let verdict = Verdict {
        kind: VerdictKind::RequestChanges,
        body: Some(note("draft-notes-bulk-publish.request.json")),
    };
    client
        .bulk_publish(MR_88, &BulkPublishBody::for_verdict(Some(&verdict)))
        .unwrap();
    let approved = client.approve(MR_88, &sha(V2)).unwrap();
    assert!(approved.user_has_approved);
    assert_eq!(approved.approved_by, ["carol"]);
    assert!(client.approvals(MR_88).unwrap().user_has_approved);
    client.resolve_discussion(MR_88, QUESTION).unwrap();

    assert_eq!(
        forge.lines(),
        [
            format!("GET {MR}"),
            format!("POST {CREATE}"),
            format!("POST {CREATE}"),
            format!("POST {CREATE}"),
            format!("POST {CREATE}"),
            format!("POST {PUBLISH}"),
            format!("POST {APPROVE}"),
            format!("GET {APPROVALS}"),
            format!("PUT {RESOLVE}"),
        ]
    );
    let bodies: Vec<_> = forge
        .requests()
        .iter()
        .filter(|request| request.method == "POST")
        .map(common::fake_forge::Recorded::json)
        .collect();
    assert_eq!(
        bodies,
        [
            json("draft-note-create.new-line.request.json"),
            json("draft-note-create.range.request.json"),
            json("draft-note-create.context-line.request.json"),
            json("draft-note-create.reply.request.json"),
            json("draft-notes-bulk-publish.request.json"),
            json("approve.request.json"),
        ]
    );
    assert_headers(&forge);
}

#[test]
fn the_first_poll_has_no_updated_after() {
    let forge = FakeForge::start();
    let all = "/merge_requests?scope=reviews_for_me&state=opened&per_page=100";
    forge.route(Route::get(all).fixture("gitlab/merge-requests-reviews-for-me.empty.json"));
    assert_eq!(client(&forge).review_requests(None).unwrap(), []);
    assert_eq!(forge.lines(), [format!("GET {all}")]);
}

#[test]
fn a_304_is_not_modified_and_its_body_is_not_parsed() {
    let forge = FakeForge::start();
    forge
        .route(
            Route::get(MR)
                .when("if-none-match", "W/\"v2\"")
                .status(304)
                .body("not JSON: a body here must not be parsed"),
        )
        .route(
            Route::get(MR)
                .fixture("gitlab/merge-request-88.v2.json")
                .header("etag", "W/\"v3\""),
        );
    let client = client(&forge);
    assert_eq!(
        client.merge_request(MR_88, Some("W/\"v2\"")).unwrap(),
        Conditional::NotModified
    );
    assert!(matches!(
        client.merge_request(MR_88, Some("W/\"v1\"")).unwrap(),
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
fn an_empty_x_next_page_stops_even_with_a_link() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(DIFFS)
            .fixture("gitlab/merge-request-88-diffs.json")
            .header("x-next-page", "")
            .header("link", &format!(r#"<{{base}}{DIFFS}&page=2>; rel="next""#)),
    );
    assert_eq!(client(&forge).diffs(MR_88).unwrap().len(), 3);
    assert_eq!(forge.lines(), [format!("GET {DIFFS}")]);
}

#[test]
fn without_x_next_page_the_link_header_gives_the_next_page() {
    let forge = FakeForge::start();
    let all = fixture("gitlab/merge-request-88-diffs.json");
    let first = edit(&all, |diffs| {
        diffs.as_array_mut().unwrap().truncate(1);
    });
    let second = edit(&all, |diffs| {
        diffs.as_array_mut().unwrap().drain(..1);
    });
    let page_2 = format!("{DIFFS}&id_after=9");
    forge
        .route(
            Route::get(DIFFS)
                .body(first)
                .header("link", &format!(r#"<{{base}}{page_2}>; rel="next""#)),
        )
        .route(Route::get(&page_2).body(second));
    assert_eq!(client(&forge).diffs(MR_88).unwrap().len(), 3);
    assert_eq!(
        forge.lines(),
        [format!("GET {DIFFS}"), format!("GET {page_2}")]
    );
}

#[test]
fn a_next_page_on_another_host_is_not_followed() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(DIFFS)
            .fixture("gitlab/merge-request-88-diffs.json")
            .header(
                "link",
                r#"<https://evil.example/api/v4/projects/77/merge_requests/88/diffs?page=2>; rel="next""#,
            ),
    );
    let error = client(&forge).diffs(MR_88).unwrap_err();
    assert!(matches!(error, ForgeError::Unexpected(_)), "{error:?}");
    assert_eq!(forge.requests().len(), 1);
}

#[test]
fn an_x_next_page_that_is_not_a_number_fails_the_list() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(DIFFS)
            .fixture("gitlab/merge-request-88-diffs.json")
            .header("x-next-page", "next"),
    );
    let error = client(&forge).diffs(MR_88).unwrap_err();
    assert!(matches!(error, ForgeError::Unexpected(_)), "{error:?}");
    assert_eq!(forge.requests().len(), 1);
}

#[test]
fn a_foreign_draft_note_is_listed() {
    let forge = FakeForge::start();
    forge.route(Route::get(DRAFTS).fixture("gitlab/draft-notes.foreign.json"));
    let drafts = client(&forge).draft_notes(MR_88).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].id, 880);
}

#[test]
fn a_401_is_unauthorized() {
    let forge = FakeForge::start();
    forge.route(
        Route::get("/user")
            .status(401)
            .fixture("gitlab/error-401.json"),
    );
    let error = client(&forge).user().unwrap_err();
    assert!(
        matches!(&error, ForgeError::Unauthorized { message } if message == "401 Unauthorized"),
        "{error:?}"
    );
}

#[test]
fn a_line_outside_the_diff_is_a_bad_request() {
    let forge = FakeForge::start();
    forge.route(
        Route::post(CREATE)
            .status(400)
            .fixture("gitlab/error-400-line-not-in-diff.json"),
    );
    let error = client(&forge)
        .create_draft_note(MR_88, &DraftNoteBody::general("x"))
        .unwrap_err();
    let ForgeError::BadRequest { message, details } = &error else {
        panic!("not a bad request: {error:?}");
    };
    assert!(message.starts_with("base: Note"), "{message}");
    assert_eq!(details.len(), 1);
}

#[test]
fn a_write_with_a_read_api_token_is_an_insufficient_scope() {
    let forge = FakeForge::start();
    forge
        .route(
            Route::get("/personal_access_tokens/self")
                .fixture("gitlab/personal-access-token-self.read-api.json"),
        )
        .route(
            Route::post(PUBLISH)
                .status(403)
                .fixture("gitlab/error-403-insufficient-scope.json"),
        );
    let client = client(&forge);
    assert_eq!(client.token_scopes().unwrap().scopes, ["read_api"]);
    let error = client
        .bulk_publish(MR_88, &BulkPublishBody::for_verdict(None))
        .unwrap_err();
    assert!(
        matches!(&error, ForgeError::InsufficientScope { scope: Some(scope), .. } if scope == "api"),
        "{error:?}"
    );
}

#[test]
fn a_plain_403_is_forbidden() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(MR)
            .status(403)
            .body(r#"{"message":"403 Forbidden"}"#),
    );
    let error = client(&forge).merge_request(MR_88, None).unwrap_err();
    assert!(matches!(error, ForgeError::Forbidden { .. }), "{error:?}");
}

#[test]
fn a_404_is_not_found() {
    let forge = FakeForge::start();
    forge.route(
        Route::get(VERSIONS)
            .status(404)
            .body(r#"{"message":"404 Project Not Found"}"#),
    );
    let error = client(&forge).versions(MR_88).unwrap_err();
    assert!(
        matches!(&error, ForgeError::NotFound { message } if message == "404 Project Not Found"),
        "{error:?}"
    );
}

#[test]
fn an_approval_after_a_new_push_is_a_conflict() {
    let forge = FakeForge::start();
    forge.route(
        Route::post(APPROVE)
            .status(409)
            .fixture("gitlab/error-409-sha-mismatch.json"),
    );
    let error = client(&forge).approve(MR_88, &sha(V2)).unwrap_err();
    assert!(
        matches!(&error, ForgeError::Conflict { message } if message.starts_with("SHA does not match")),
        "{error:?}"
    );
}

#[test]
fn an_error_on_a_later_page_fails_the_list() {
    let forge = FakeForge::start();
    forge
        .route(
            Route::get(DISCUSSIONS)
                .fixture("gitlab/merge-request-88-discussions.page-1.json")
                .header("x-next-page", "2"),
        )
        .route(
            Route::get(DISCUSSIONS_2)
                .status(502)
                .body("<html>Bad gateway</html>"),
        );
    let error = client(&forge)
        .with_per_page(2)
        .discussions(MR_88, &sha(V2))
        .unwrap_err();
    assert!(
        matches!(error, ForgeError::Status { status: 502, .. }),
        "{error:?}"
    );
}

#[test]
fn a_discussion_id_cannot_change_the_path() {
    let forge = FakeForge::start();
    let encoded = "/projects/77/merge_requests/88/discussions/%2E%2E%2Fapprove%3Fx?resolved=true";
    forge.route(Route::new("PUT", encoded));
    client(&forge)
        .resolve_discussion(MR_88, "../approve?x")
        .unwrap();
    assert_eq!(forge.lines(), [format!("PUT {encoded}")]);
}

#[test]
fn per_page_stays_between_1_and_100() {
    let forge = FakeForge::start();
    forge
        .route(Route::get("/projects/77/merge_requests/88/diffs?per_page=1").body("[]"))
        .route(Route::get(DIFFS).body("[]"));
    client(&forge).with_per_page(0).diffs(MR_88).unwrap();
    client(&forge).with_per_page(200).diffs(MR_88).unwrap();
    assert_eq!(
        forge.lines(),
        [
            "GET /projects/77/merge_requests/88/diffs?per_page=1".to_owned(),
            format!("GET {DIFFS}"),
        ]
    );
}

#[test]
fn the_token_is_in_no_debug_output_and_no_error() {
    let forge = FakeForge::start();
    forge
        .route(
            Route::get("/user")
                .status(401)
                .body(format!(r#"{{"message":"401 Unauthorized: {TOKEN}"}}"#)),
        )
        .route(
            Route::post(CREATE)
                .status(400)
                .body(format!(r#"{{"message":{{"base":["{TOKEN}"]}}}}"#)),
        )
        .route(Route::post(PUBLISH).status(403).body(format!(
            r#"{{"error":"insufficient_scope","error_description":"{TOKEN}"}}"#
        )))
        .route(Route::get(MR).body(format!(r#"{{"id":"{TOKEN}"}}"#)));
    let client = client(&forge);
    assert!(!format!("{client:?}").contains(TOKEN));
    assert!(format!("{client:?}").contains(REDACTED));
    let errors = [
        client.user().unwrap_err(),
        client
            .create_draft_note(MR_88, &DraftNoteBody::general("x"))
            .unwrap_err(),
        client
            .bulk_publish(MR_88, &BulkPublishBody::for_verdict(None))
            .unwrap_err(),
        client.merge_request(MR_88, None).unwrap_err(),
    ];
    for error in &errors {
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains(TOKEN), "{shown}");
        assert!(shown.contains(REDACTED), "{shown}");
    }
}

#[test]
fn a_401_on_approve_reads_the_approvals_to_tell_why() {
    let unauthorized = r#"{"message":"401 Unauthorized"}"#;
    // Approved already (a resumed publish): a success.
    let forge = FakeForge::start();
    forge
        .route(Route::post(APPROVE).status(401).body(unauthorized))
        .route(Route::get(APPROVALS).fixture("gitlab/approve.response.json"));
    let approvals = client(&forge).approve(MR_88, &sha(V2)).unwrap();
    assert!(approvals.user_has_approved);
    assert_eq!(
        forge.lines(),
        [format!("POST {APPROVE}"), format!("GET {APPROVALS}")]
    );
    // Not approved: GitLab refuses the approval itself, the token is good.
    let forge = FakeForge::start();
    let not_approved = edit(
        &common::fake_forge::fixture("gitlab/approve.response.json"),
        |body| body["user_has_approved"] = false.into(),
    );
    forge
        .route(Route::post(APPROVE).status(401).body(unauthorized))
        .route(Route::get(APPROVALS).body(not_approved));
    let error = client(&forge).approve(MR_88, &sha(V2)).unwrap_err();
    assert!(
        matches!(error, ForgeError::ApprovalRefused { .. }),
        "{error:?}"
    );
    // A token that is not valid fails the approvals too.
    let forge = FakeForge::start();
    forge
        .route(
            Route::post(APPROVE)
                .status(401)
                .fixture("gitlab/error-401.json"),
        )
        .route(
            Route::get(APPROVALS)
                .status(401)
                .fixture("gitlab/error-401.json"),
        );
    let error = client(&forge).approve(MR_88, &sha(V2)).unwrap_err();
    assert!(
        matches!(error, ForgeError::Unauthorized { .. }),
        "{error:?}"
    );
}

#[test]
fn project_metadata_with_the_count_of_open_merge_requests() {
    let forge = FakeForge::start();
    forge
        .route(Route::get("/projects/company-a%2Fcheckout").fixture("gitlab/project.json"))
        .route(
            Route::get("/projects/77/merge_requests?state=opened&per_page=1")
                .body("[]")
                .header("x-total", "4"),
        );
    let project = client(&forge).project("company-a/checkout").unwrap();
    assert_eq!(
        project,
        ForgeProject {
            id: 77,
            default_branch: Some("main".to_owned()),
            visibility: ForgeVisibility::Internal,
            archived: true,
            web_url: "https://gitlab.example.com/company-a/checkout".to_owned(),
            open_mrs: Some(4),
        }
    );
    for request in forge.requests() {
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
    }
}

#[test]
fn project_without_a_count_or_not_visible() {
    // GitLab leaves `x-total` out above 10,000 items.
    let forge = FakeForge::start();
    forge
        .route(Route::get("/projects/company-a%2Fsub%2Fcheckout").fixture("gitlab/project.json"))
        .route(Route::get("/projects/77/merge_requests?state=opened&per_page=1").body("[]"));
    let project = client(&forge).project("company-a/sub/checkout").unwrap();
    assert_eq!(project.open_mrs, None);

    let forge = FakeForge::start();
    forge.route(
        Route::get("/projects/company-a%2Fgone")
            .status(404)
            .body(r#"{"message":"404 Project Not Found"}"#),
    );
    assert!(matches!(
        client(&forge).project("company-a/gone"),
        Err(ForgeError::NotFound { .. })
    ));
    assert_eq!(forge.requests().len(), 1);
}
