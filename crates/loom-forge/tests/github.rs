//! GitHub parsers over `fixtures/forge/github/`.

#![allow(clippy::unwrap_used, reason = "a failing helper must fail the test")]

mod common;

use common::{M1, M2, V1, V2, edit, sha, time};
use loom_api::ForgeKind;
use loom_forge::ParseError;
use loom_forge::github::{
    ReviewState, parse_add_reply, parse_add_review, parse_error, parse_files, parse_issue_comments,
    parse_pending_reviews, parse_pull, parse_resolve_thread, parse_review_threads, parse_reviews,
    parse_search, parse_submit_review, parse_user,
};
use loom_forge::model::{
    Anchor, FileChange, MrForgeIds, MrKey, MrState, MrVersion, Side, ThreadSubject,
};

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("../../../fixtures/forge/github/", $name)).as_slice()
    };
}

#[test]
fn user() {
    let user = parse_user(fixture!("user.json")).unwrap();
    assert_eq!((user.id, user.login.as_str()), (1003, "carol"));
}

#[test]
fn pull_request_of_each_version() {
    let v1 = parse_pull(fixture!("pull-412.v1.json")).unwrap();
    assert_eq!(v1.key, MrKey::parse("acme/shop#412").unwrap());
    assert_eq!(
        v1.forge_ids,
        MrForgeIds::Github {
            node_id: "PR_kwDOAAATic4AA9-c".to_owned(),
            id: 900_412,
        }
    );
    assert_eq!(v1.title, "Auth refactor");
    assert_eq!(v1.description, "Refactor the token checks.\n\nCloses #400.");
    assert_eq!(v1.author, "alice");
    assert_eq!(v1.state, MrState::Open);
    assert!(!v1.draft);
    assert_eq!(v1.labels, ["security"]);
    assert_eq!(v1.reviewers, ["carol"]);
    assert_eq!(
        (v1.base_ref.as_str(), v1.head_ref.as_str()),
        ("main", "auth-refactor")
    );
    assert_eq!(
        v1.version,
        MrVersion {
            head_sha: sha(V1),
            base_sha: sha(M1),
            merge_base: None,
            forge_version_id: None,
            created_at: None,
        }
    );
    assert_eq!(v1.web_url, "https://github.com/acme/shop/pull/412");
    assert_eq!(v1.created_at, time("2026-10-21T10:05:00Z"));
    assert_eq!(v1.updated_at, time("2026-10-21T17:00:00Z"));

    let v2 = parse_pull(fixture!("pull-412.v2.json")).unwrap();
    assert_eq!(
        (v2.version.head_sha, v2.version.base_sha),
        (sha(V2), sha(M2))
    );
}

#[test]
fn pull_request_states() {
    let body = fixture!("pull-412.v2.json");
    let closed = edit(body, |pull| pull["state"] = "closed".into());
    assert_eq!(parse_pull(&closed).unwrap().state, MrState::Closed);
    let merged = edit(body, |pull| {
        pull["state"] = "closed".into();
        pull["merged_at"] = "2026-10-24T08:00:00Z".into();
    });
    assert_eq!(parse_pull(&merged).unwrap().state, MrState::Merged);
    let draft = edit(body, |pull| pull["draft"] = true.into());
    assert!(parse_pull(&draft).unwrap().draft);
}

#[test]
fn pull_request_failures_are_typed_errors() {
    let body = fixture!("pull-412.v2.json");
    // Unknown fields are ignored; optional fields can be null or missing.
    let extra = edit(body, |pull| {
        pull["new_field"] = serde_json::json!({"nested": [1, 2]});
        pull["body"] = serde_json::Value::Null;
        pull["user"] = serde_json::Value::Null;
        pull.as_object_mut().unwrap().remove("labels");
    });
    let pull = parse_pull(&extra).unwrap();
    assert_eq!(
        (pull.description.as_str(), pull.author.as_str()),
        ("", "ghost")
    );
    assert_eq!(pull.labels, Vec::<String>::new());

    for (case, body) in [
        (
            "missing head",
            edit(body, |pull| {
                drop(pull.as_object_mut().unwrap().remove("head"));
            }),
        ),
        (
            "short SHA",
            edit(body, |pull| pull["head"]["sha"] = "84280c4".into()),
        ),
        (
            "bad time",
            edit(body, |pull| pull["created_at"] = "yesterday".into()),
        ),
        (
            "number as text",
            edit(body, |pull| pull["number"] = "412".into()),
        ),
        ("not JSON", b"<html>".to_vec()),
        ("truncated", body[..body.len() / 2].to_vec()),
        ("empty", Vec::new()),
    ] {
        assert!(
            matches!(parse_pull(&body), Err(ParseError::Json { .. })),
            "{case}"
        );
    }
    for (case, body) in [
        (
            "unknown state",
            edit(body, |pull| pull["state"] = "draft".into()),
        ),
        (
            "no base repository",
            edit(body, |pull| pull["base"]["repo"] = serde_json::Value::Null),
        ),
        (
            "bad repository",
            edit(body, |pull| {
                pull["base"]["repo"]["full_name"] = "shop".into();
            }),
        ),
    ] {
        assert!(
            matches!(parse_pull(&body), Err(ParseError::Invalid { .. })),
            "{case}"
        );
    }
}

#[test]
fn search_of_review_requests() {
    let page = parse_search(fixture!("search-review-requested.json")).unwrap();
    assert_eq!((page.total_count, page.incomplete_results), (1, false));
    let [item] = page.items.as_slice() else {
        panic!("{:?}", page.items)
    };
    assert_eq!(item.key.to_string(), "acme/shop#412");
    assert_eq!(item.key.forge(), ForgeKind::Github);
    assert_eq!(
        (item.title.as_str(), item.author.as_str()),
        ("Auth refactor", "alice")
    );
    assert_eq!(item.state, MrState::Open);
    assert_eq!(item.head_sha, None);
    assert_eq!(item.updated_at, time("2026-10-23T11:06:00Z"));

    let empty = parse_search(fixture!("search-review-requested.empty.json")).unwrap();
    assert_eq!(empty.items, Vec::<loom_forge::model::MrSummary>::new());

    // Issues are left out; GitHub Enterprise Server URLs give the same key.
    let body = edit(fixture!("search-review-requested.json"), |search| {
        let mut issue = search["items"][0].clone();
        issue.as_object_mut().unwrap().remove("pull_request");
        search["items"][0]["repository_url"] =
            "https://github.example/api/v3/repos/acme/shop".into();
        search["items"].as_array_mut().unwrap().push(issue);
    });
    let page = parse_search(&body).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].key.to_string(), "acme/shop#412");

    let bad = edit(fixture!("search-review-requested.json"), |search| {
        search["items"][0]["repository_url"] = "shop".into();
    });
    assert!(matches!(
        parse_search(&bad),
        Err(ParseError::Invalid { .. })
    ));
}

#[test]
fn files_of_the_pull_request() {
    let files = parse_files(fixture!("pull-412-files.v2.json")).unwrap();
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["Cargo.lock", "src/api/login.rs", "src/auth/jwt.rs"]);
    assert!(
        files
            .iter()
            .all(|file| file.change == FileChange::Modified && !file.too_large)
    );
    assert_eq!(
        files[0].patch.as_deref(),
        Some("@@ -1 +1 @@\n-# lock v1\n+# lock v2")
    );

    let body = edit(fixture!("pull-412-files.v2.json"), |files| {
        files[0]["status"] = "renamed".into();
        files[0]["previous_filename"] = "Cargo.lock.old".into();
        files[1].as_object_mut().unwrap().remove("patch");
    });
    let files = parse_files(&body).unwrap();
    assert_eq!(files[0].change, FileChange::Renamed);
    assert_eq!(files[0].old_path.as_deref(), Some("Cargo.lock.old"));
    assert_eq!(
        (files[1].patch.as_deref(), files[1].too_large),
        (None, true)
    );

    let bad = edit(fixture!("pull-412-files.v2.json"), |files| {
        files[0]["status"] = "moved".into();
    });
    assert!(matches!(parse_files(&bad), Err(ParseError::Invalid { .. })));
}

#[test]
fn reviews_and_general_comments() {
    let reviews = parse_reviews(fixture!("pull-412-reviews.json")).unwrap();
    let [review] = reviews.as_slice() else {
        panic!("{reviews:?}")
    };
    assert_eq!(
        (review.author.as_str(), review.state),
        ("bob", ReviewState::Commented)
    );
    assert_eq!(review.commit_sha, Some(sha(V1)));
    assert_eq!(review.submitted_at, Some(time("2026-10-21T16:10:00Z")));

    let comments = parse_issue_comments(fixture!("issue-412-comments.json")).unwrap();
    let [thread] = comments.as_slice() else {
        panic!("{comments:?}")
    };
    assert_eq!(thread.subject, ThreadSubject::General);
    assert!(!thread.resolved && !thread.outdated);
    assert_eq!(thread.comments[0].body, "I will look at the login part.");
    assert_eq!(thread.comments[0].author, "bob");
}

#[test]
fn review_threads_keep_outdated_and_resolved_flags() {
    let page = parse_review_threads(fixture!("graphql-review-threads.json")).unwrap();
    assert_eq!(page.pull_request_id, "PR_kwDOAAATic4AA9-c");
    assert_eq!(page.head_sha, sha(V2));
    assert_eq!(page.next_cursor, None);
    assert_eq!(page.threads_with_more_comments, Vec::<String>::new());
    let [question, nitpick] = page.threads.as_slice() else {
        panic!("{:?}", page.threads)
    };
    assert_eq!(question.id, "PRRT_kwDOAAATic5AAAAB");
    assert!(question.outdated && !question.resolved);
    assert_eq!(
        question.subject,
        ThreadSubject::Lines {
            anchor: Anchor {
                path: "src/auth/jwt.rs".to_owned(),
                side: Side::New,
                start_line: 10,
                end_line: 10,
                sha: sha(V1),
            }
        }
    );
    let authors: Vec<&str> = question
        .comments
        .iter()
        .map(|c| c.author.as_str())
        .collect();
    assert_eq!(authors, ["bob", "alice"]);

    assert!(!nitpick.outdated && nitpick.resolved);
    let ThreadSubject::Lines { anchor } = &nitpick.subject else {
        panic!("{nitpick:?}")
    };
    assert_eq!(
        (anchor.start_line, anchor.end_line, &anchor.sha),
        (6, 6, &sha(V2))
    );
}

#[test]
fn review_threads_pages_ranges_files_and_pending_comments() {
    let body = edit(fixture!("graphql-review-threads.json"), |data| {
        let threads = &mut data["data"]["repository"]["pullRequest"]["reviewThreads"];
        threads["pageInfo"]["hasNextPage"] = true.into();
        let nodes = &mut threads["nodes"];
        // A range on the left side of the current diff.
        nodes[1]["diffSide"] = "LEFT".into();
        nodes[1]["startLine"] = 4.into();
        // A file thread.
        nodes[0]["subjectType"] = "FILE".into();
        nodes[0]["comments"]["pageInfo"]["hasNextPage"] = true.into();
        // A thread of my pending review only.
        let mut pending = nodes[1].clone();
        pending["id"] = "PRRT_pending".into();
        pending["comments"]["nodes"][0]["state"] = "PENDING".into();
        nodes.as_array_mut().unwrap().push(pending);
    });
    let page = parse_review_threads(&body).unwrap();
    assert_eq!(page.next_cursor.as_deref(), Some("Y3Vyc29yOnYyOpK0"));
    assert_eq!(page.threads_with_more_comments, ["PRRT_kwDOAAATic5AAAAB"]);
    assert_eq!(page.threads.len(), 2);
    assert_eq!(
        page.threads[0].subject,
        ThreadSubject::File {
            path: "src/auth/jwt.rs".to_owned(),
            sha: sha(V1),
        }
    );
    let ThreadSubject::Lines { anchor } = &page.threads[1].subject else {
        panic!("{:?}", page.threads[1])
    };
    assert_eq!(
        (anchor.side, anchor.start_line, anchor.end_line),
        (Side::Old, 4, 6)
    );
}

#[test]
fn pending_reviews() {
    let reviews = parse_pending_reviews(fixture!("graphql-pending-reviews.json")).unwrap();
    let [review] = reviews.as_slice() else {
        panic!("{reviews:?}")
    };
    assert_eq!(review.id, "PRR_kwDOAAATic4AABe1");
    assert_eq!(review.author, "carol");
    assert_eq!(review.created_at, time("2026-10-22T09:00:00Z"));
    assert_eq!(review.comment_count, 2);
    assert_eq!(
        parse_pending_reviews(fixture!("graphql-pending-reviews.empty.json")).unwrap(),
        Vec::<loom_forge::github::PendingReview>::new()
    );
}

#[test]
fn publish_mutations() {
    assert_eq!(
        parse_add_review(fixture!("graphql-add-review.response.json")).unwrap(),
        "PRR_kwDOAAATic4AABd9"
    );
    assert_eq!(
        parse_add_reply(fixture!("graphql-add-reply.response.json")).unwrap(),
        "PRRC_kwDOAAATic5AAAAG"
    );
    let submitted = parse_submit_review(fixture!("graphql-submit-review.response.json")).unwrap();
    assert_eq!(submitted.state, ReviewState::ChangesRequested);
    assert_eq!(submitted.submitted_at, time("2026-10-23T14:00:00Z"));
    assert_eq!(
        submitted.url,
        "https://github.com/acme/shop/pull/412#pullrequestreview-6002"
    );
    assert_eq!(
        parse_resolve_thread(fixture!("graphql-resolve-thread.response.json")).unwrap(),
        "PRRT_kwDOAAATic5AAAAB"
    );

    let pending = edit(fixture!("graphql-submit-review.response.json"), |data| {
        data["data"]["submitPullRequestReview"]["pullRequestReview"]["state"] = "PENDING".into();
    });
    assert!(matches!(
        parse_submit_review(&pending),
        Err(ParseError::Invalid { .. })
    ));
    let unresolved = edit(fixture!("graphql-resolve-thread.response.json"), |data| {
        data["data"]["resolveReviewThread"]["thread"]["isResolved"] = false.into();
    });
    assert!(matches!(
        parse_resolve_thread(&unresolved),
        Err(ParseError::Invalid { .. })
    ));
    assert!(matches!(
        parse_add_reply(br#"{"data": null}"#),
        Err(ParseError::Invalid { .. })
    ));
    assert!(matches!(
        parse_add_reply(br#"{"data": {}}"#),
        Err(ParseError::Invalid { .. })
    ));
    assert!(matches!(
        parse_add_reply(br#"{"data": {"addPullRequestReviewThreadReply": {}}}"#),
        Err(ParseError::Json { .. })
    ));
}

#[test]
fn graphql_errors_are_typed() {
    for body in [
        fixture!("graphql-error-one-pending-review.json"),
        fixture!("graphql-error-line-not-in-diff.json"),
    ] {
        let Err(ParseError::Graphql { errors, .. }) = parse_add_review(body) else {
            panic!("{:?}", parse_add_review(body))
        };
        let [error] = errors.as_slice() else {
            panic!("{errors:?}")
        };
        assert_eq!(error.kind.as_deref(), Some("UNPROCESSABLE"));
        assert_eq!(error.path, ["addPullRequestReview"]);
    }
    // Errors without type or path, and mixed paths.
    let body = br#"{"errors": [{"message": "boom"}, {"message": "x", "path": ["a", 0, "b"]}]}"#;
    let Err(ParseError::Graphql { errors, .. }) = parse_review_threads(body) else {
        panic!()
    };
    assert_eq!((errors[0].kind.as_deref(), errors[0].path.len()), (None, 0));
    assert_eq!(errors[1].path, ["a", "0", "b"]);
    let body = br#"{"data": null, "errors": null}"#;
    assert!(matches!(
        parse_review_threads(body),
        Err(ParseError::Invalid { .. })
    ));
    let body = br#"{"data": {"repository": null}}"#;
    assert!(matches!(
        parse_pending_reviews(body),
        Err(ParseError::Invalid { .. })
    ));
}

#[test]
fn error_bodies() {
    let error = parse_error(fixture!("error-401.json")).unwrap();
    assert_eq!(error.message, "Bad credentials");
    assert_eq!(error.details, Vec::<String>::new());
    let error = parse_error(fixture!("error-403-rate-limit.json")).unwrap();
    assert!(error.message.starts_with("API rate limit exceeded"));
    let error = parse_error(fixture!("error-422-own-pull.json")).unwrap();
    assert_eq!(
        error.details,
        ["Review Can not approve your own pull request"]
    );
    let error = parse_error(br#"{"message": "Validation Failed", "errors": [{"resource": "Issue", "code": "custom", "message": "too long"}, {"code": "missing"}]}"#).unwrap();
    assert_eq!(error.details, ["too long", r#"{"code":"missing"}"#]);
    assert!(matches!(parse_error(b"{}"), Err(ParseError::Json { .. })));
}
