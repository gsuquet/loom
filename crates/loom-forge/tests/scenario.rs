//! The scenario of `fixtures/forge/`: the same MR on both forges gives the same model.

#![allow(clippy::unwrap_used, reason = "a failing helper must fail the test")]

mod common;

use common::{V2, sha};
use loom_forge::model::{FileDiff, Thread};
use loom_forge::{github, gitlab, sort_threads};

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("../../../fixtures/forge/", $name)).as_slice()
    };
}

/// A thread without the IDs of its forge.
fn neutral(mut thread: Thread) -> Thread {
    thread.id = String::new();
    for comment in &mut thread.comments {
        comment.id = String::new();
    }
    thread
}

#[test]
fn both_forges_give_equal_threads() {
    let review_threads =
        github::parse_review_threads(fixture!("github/graphql-review-threads.json")).unwrap();
    let comments =
        github::parse_issue_comments(fixture!("github/issue-412-comments.json")).unwrap();
    let reviews = github::parse_reviews(fixture!("github/pull-412-reviews.json")).unwrap();
    let from_github: Vec<Thread> = github::threads(review_threads.threads, comments, &reviews)
        .into_iter()
        .map(neutral)
        .collect();

    let mut from_gitlab = gitlab::parse_discussions(
        fixture!("gitlab/merge-request-88-discussions.page-1.json"),
        &sha(V2),
    )
    .unwrap();
    from_gitlab.extend(
        gitlab::parse_discussions(
            fixture!("gitlab/merge-request-88-discussions.page-2.json"),
            &sha(V2),
        )
        .unwrap(),
    );
    sort_threads(&mut from_gitlab);
    let from_gitlab: Vec<Thread> = from_gitlab.into_iter().map(neutral).collect();

    assert_eq!(from_github.len(), 3);
    assert_eq!(from_github, from_gitlab);
}

#[test]
fn both_forges_give_equal_file_diffs() {
    let from_github: Vec<FileDiff> = github::parse_files(fixture!("github/pull-412-files.v2.json"))
        .unwrap()
        .into_iter()
        .map(|mut diff| {
            // Only GitLab marks generated files.
            diff.generated = diff.path == "Cargo.lock";
            diff
        })
        .collect();
    let from_gitlab = gitlab::parse_diffs(fixture!("gitlab/merge-request-88-diffs.json")).unwrap();
    assert_eq!(from_github, from_gitlab);
}

#[test]
fn both_forges_give_the_same_mr_fields() {
    let pull = github::parse_pull(fixture!("github/pull-412.v2.json")).unwrap();
    let mr = gitlab::parse_merge_request(fixture!("gitlab/merge-request-88.v2.json")).unwrap();
    assert_eq!(
        (&pull.title, &pull.author, pull.state, pull.draft),
        (&mr.title, &mr.author, mr.state, mr.draft)
    );
    assert_eq!(
        (&pull.base_ref, &pull.head_ref, &pull.labels),
        (&mr.base_ref, &mr.head_ref, &mr.labels)
    );
    assert_eq!(
        (&pull.version.head_sha, &pull.version.base_sha),
        (&mr.version.head_sha, &mr.version.base_sha)
    );
    assert_eq!(
        (pull.created_at, pull.updated_at),
        (mr.created_at, mr.updated_at)
    );
}
