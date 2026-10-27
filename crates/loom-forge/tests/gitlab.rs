//! GitLab parsers over `fixtures/forge/gitlab/`.

#![allow(clippy::unwrap_used, reason = "a failing helper must fail the test")]

mod common;

use common::{M1, M2, V1, V2, edit, sha, time};
use loom_api::ForgeKind;
use loom_forge::ParseError;
use loom_forge::gitlab::{
    parse_approvals, parse_diffs, parse_discussions, parse_draft_note, parse_draft_notes,
    parse_error, parse_merge_request, parse_merge_requests, parse_token, parse_user,
    parse_versions,
};
use loom_forge::model::{
    Anchor, FileChange, MrForgeIds, MrKey, MrState, MrVersion, Side, ThreadSubject,
};

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("../../../fixtures/forge/gitlab/", $name)).as_slice()
    };
}

#[test]
fn user_and_token() {
    let user = parse_user(fixture!("user.json")).unwrap();
    assert_eq!((user.id, user.login.as_str()), (13, "carol"));
    let token = parse_token(fixture!("personal-access-token-self.json")).unwrap();
    assert_eq!(token.scopes, ["api"]);
    assert!(token.active);
    assert_eq!(token.expires_at.as_deref(), Some("2027-10-01"));
    let token = parse_token(fixture!("personal-access-token-self.read-api.json")).unwrap();
    assert_eq!(token.scopes, ["read_api"]);
}

#[test]
fn merge_request_of_each_version() {
    let v1 = parse_merge_request(fixture!("merge-request-88.v1.json")).unwrap();
    assert_eq!(v1.key, MrKey::parse("company-a/checkout!88").unwrap());
    assert_eq!(v1.key.forge(), ForgeKind::Gitlab);
    assert_eq!(
        v1.forge_ids,
        MrForgeIds::Gitlab {
            id: 3088,
            project_id: 77
        }
    );
    assert_eq!(v1.title, "Auth refactor");
    assert_eq!(v1.author, "alice");
    assert_eq!(v1.state, MrState::Open);
    assert_eq!(v1.labels, ["security"]);
    assert_eq!(v1.reviewers, ["bob", "carol"]);
    assert_eq!(
        (v1.base_ref.as_str(), v1.head_ref.as_str()),
        ("main", "auth-refactor")
    );
    assert_eq!(
        v1.version,
        MrVersion {
            head_sha: sha(V1),
            base_sha: sha(M1),
            merge_base: Some(sha(M1)),
            forge_version_id: None,
            created_at: None,
        }
    );
    assert_eq!(v1.created_at, time("2026-10-21T10:05:00Z"));

    let v2 = parse_merge_request(fixture!("merge-request-88.v2.json")).unwrap();
    assert_eq!(
        (v2.version.head_sha, v2.version.base_sha),
        (sha(V2), sha(M2))
    );
    assert_eq!(v2.updated_at, time("2026-10-23T11:06:00Z"));
}

#[test]
fn merge_request_states_and_failures() {
    let body = fixture!("merge-request-88.v2.json");
    for (state, expected) in [
        ("merged", MrState::Merged),
        ("closed", MrState::Closed),
        ("locked", MrState::Open),
    ] {
        let body = edit(body, |mr| mr["state"] = state.into());
        assert_eq!(
            parse_merge_request(&body).unwrap().state,
            expected,
            "{state}"
        );
    }
    let wip = edit(body, |mr| mr["work_in_progress"] = true.into());
    assert!(parse_merge_request(&wip).unwrap().draft);
    let extra = edit(body, |mr| {
        mr["unknown"] = serde_json::json!([{"a": 1}]);
        mr["description"] = serde_json::Value::Null;
        mr.as_object_mut().unwrap().remove("reviewers");
    });
    let mr = parse_merge_request(&extra).unwrap();
    assert_eq!(mr.description, "");
    assert_eq!(mr.reviewers, Vec::<String>::new());

    for (case, body) in [
        (
            "missing iid",
            edit(body, |mr| drop(mr.as_object_mut().unwrap().remove("iid"))),
        ),
        (
            "bad SHA",
            edit(body, |mr| mr["diff_refs"]["head_sha"] = "HEAD".into()),
        ),
        ("bad time", edit(body, |mr| mr["updated_at"] = 5.into())),
        ("not JSON", b"502 Bad Gateway".to_vec()),
        ("truncated", body[..100].to_vec()),
    ] {
        assert!(
            matches!(parse_merge_request(&body), Err(ParseError::Json { .. })),
            "{case}"
        );
    }
    for (case, body) in [
        (
            "no diff_refs yet",
            edit(body, |mr| mr["diff_refs"] = serde_json::Value::Null),
        ),
        (
            "unknown state",
            edit(body, |mr| mr["state"] = "reopened".into()),
        ),
        (
            "GitHub reference",
            edit(body, |mr| {
                mr["references"]["full"] = "company-a/checkout#88".into();
            }),
        ),
        (
            "other number",
            edit(body, |mr| {
                mr["references"]["full"] = "company-a/checkout!89".into();
            }),
        ),
    ] {
        assert!(
            matches!(parse_merge_request(&body), Err(ParseError::Invalid { .. })),
            "{case}"
        );
    }
}

#[test]
fn merge_requests_to_review() {
    let mrs = parse_merge_requests(fixture!("merge-requests-reviews-for-me.json")).unwrap();
    let [mr] = mrs.as_slice() else {
        panic!("{mrs:?}")
    };
    assert_eq!(mr.key.to_string(), "company-a/checkout!88");
    assert_eq!(mr.head_sha, Some(sha(V2)));
    assert_eq!(mr.state, MrState::Open);
    assert_eq!(
        mr.web_url,
        "https://gitlab.example.com/company-a/checkout/-/merge_requests/88"
    );
    assert_eq!(
        parse_merge_requests(fixture!("merge-requests-reviews-for-me.empty.json")).unwrap(),
        Vec::<loom_forge::model::MrSummary>::new()
    );
}

#[test]
fn versions_newest_first() {
    let versions = parse_versions(fixture!("merge-request-88-versions.json")).unwrap();
    assert_eq!(
        versions,
        [
            MrVersion {
                head_sha: sha(V2),
                base_sha: sha(M2),
                merge_base: Some(sha(M2)),
                forge_version_id: Some(202),
                created_at: Some(time("2026-10-23T11:06:00Z")),
            },
            MrVersion {
                head_sha: sha(V1),
                base_sha: sha(M1),
                merge_base: Some(sha(M1)),
                forge_version_id: Some(201),
                created_at: Some(time("2026-10-21T10:05:00Z")),
            },
        ]
    );
}

#[test]
fn diffs() {
    let diffs = parse_diffs(fixture!("merge-request-88-diffs.json")).unwrap();
    let paths: Vec<&str> = diffs.iter().map(|diff| diff.path.as_str()).collect();
    assert_eq!(paths, ["Cargo.lock", "src/api/login.rs", "src/auth/jwt.rs"]);
    assert!(diffs[0].generated && !diffs[1].generated);
    assert_eq!(
        diffs[0].patch.as_deref(),
        Some("@@ -1 +1 @@\n-# lock v1\n+# lock v2")
    );
    assert!(
        diffs
            .iter()
            .all(|diff| diff.change == FileChange::Modified && diff.old_path.is_none())
    );

    let body = edit(fixture!("merge-request-88-diffs.json"), |diffs| {
        diffs[0]["too_large"] = true.into();
        diffs[0]["diff"] = "".into();
        diffs[1]["renamed_file"] = true.into();
        diffs[1]["old_path"] = "src/login.rs".into();
        diffs[2]["new_file"] = true.into();
    });
    let diffs = parse_diffs(&body).unwrap();
    assert_eq!(
        (diffs[0].patch.as_deref(), diffs[0].too_large),
        (None, true)
    );
    assert_eq!(
        (diffs[1].change, diffs[1].old_path.as_deref()),
        (FileChange::Renamed, Some("src/login.rs"))
    );
    assert_eq!(diffs[2].change, FileChange::Added);

    let mode = edit(fixture!("merge-request-88-diffs.json"), |diffs| {
        diffs[0]["diff"] = "".into();
        diffs[0]["b_mode"] = "100755".into();
    });
    assert_eq!(parse_diffs(&mode).unwrap()[0].change, FileChange::Changed);
}

fn discussions() -> Vec<serde_json::Value> {
    let mut all: Vec<serde_json::Value> =
        serde_json::from_slice(fixture!("merge-request-88-discussions.page-1.json")).unwrap();
    let page_2: Vec<serde_json::Value> =
        serde_json::from_slice(fixture!("merge-request-88-discussions.page-2.json")).unwrap();
    all.extend(page_2);
    all
}

#[test]
fn discussions_drop_system_notes_and_keep_flags() {
    let threads = parse_discussions(
        fixture!("merge-request-88-discussions.page-1.json"),
        &sha(V2),
    )
    .unwrap();
    let [question, nitpick] = threads.as_slice() else {
        panic!("{threads:?}")
    };
    assert_eq!(question.id, "6a9c1750b37d513a43987b574953fceb50b03ce7");
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
    assert_eq!(question.comments.len(), 2);
    assert_eq!(question.comments[1].id, "502");
    assert!(!nitpick.outdated && nitpick.resolved);

    let page_2 = parse_discussions(
        fixture!("merge-request-88-discussions.page-2.json"),
        &sha(V2),
    )
    .unwrap();
    let [general] = page_2.as_slice() else {
        panic!("{page_2:?}")
    };
    assert_eq!(general.subject, ThreadSubject::General);
    assert_eq!(general.comments[0].body, "I will look at the login part.");

    // On the first version, nothing is outdated yet... except the nitpick, moved to version 2.
    let threads = parse_discussions(
        fixture!("merge-request-88-discussions.page-1.json"),
        &sha(V1),
    )
    .unwrap();
    assert!(!threads[0].outdated && threads[1].outdated);
}

#[test]
fn discussion_positions() {
    let mut all = discussions();
    // A removed line.
    all[0]["notes"][0]["position"]["new_line"] = serde_json::Value::Null;
    all[0]["notes"][0]["position"]["old_line"] = 9.into();
    // A range of lines.
    all[0]["notes"][0]["position"]["line_range"] = serde_json::json!({
        "start": {"line_code": "x_7_", "type": "old", "old_line": 7, "new_line": null},
        "end": {"line_code": "x_9_", "type": "old", "old_line": 9, "new_line": null}
    });
    // A file position and an unchanged line.
    all[1]["notes"][0]["position"]["position_type"] = "file".into();
    let unchanged = {
        let mut discussion = all[0].clone();
        discussion["id"] = "unchanged".into();
        discussion["notes"][0]["position"] = serde_json::json!({
            "base_sha": M2, "start_sha": M2, "head_sha": V2,
            "old_path": "src/auth/jwt.rs", "new_path": "src/auth/jwt.rs",
            "position_type": "text", "old_line": 8, "new_line": 8, "line_range": null
        });
        discussion
    };
    all.push(unchanged);
    let threads = parse_discussions(&serde_json::to_vec(&all).unwrap(), &sha(V2)).unwrap();
    let ThreadSubject::Lines { anchor } = &threads[0].subject else {
        panic!("{:?}", threads[0])
    };
    assert_eq!(
        (anchor.side, anchor.start_line, anchor.end_line),
        (Side::Old, 7, 9)
    );
    assert_eq!(
        threads[1].subject,
        ThreadSubject::File {
            path: "src/api/login.rs".to_owned(),
            sha: sha(V2),
        }
    );
    // The system note of page 2 is dropped.
    assert_eq!(threads.len(), 4);
    let ThreadSubject::Lines { anchor } = &threads[3].subject else {
        panic!("{:?}", threads[3])
    };
    assert_eq!((anchor.side, anchor.start_line), (Side::New, 8));
    assert!(!threads[3].outdated);

    let bad = edit(&serde_json::to_vec(&discussions()).unwrap(), |all| {
        all[0]["notes"][0]["created_at"] = serde_json::Value::Null;
    });
    assert!(matches!(
        parse_discussions(&bad, &sha(V2)),
        Err(ParseError::Json { .. })
    ));
}

#[test]
fn draft_notes() {
    assert_eq!(
        parse_draft_notes(fixture!("draft-notes.empty.json")).unwrap(),
        Vec::<loom_forge::gitlab::DraftNote>::new()
    );
    let notes = parse_draft_notes(fixture!("draft-notes.foreign.json")).unwrap();
    let [note] = notes.as_slice() else {
        panic!("{notes:?}")
    };
    assert_eq!((note.id, note.author_id), (880, 13));
    assert_eq!(note.body, "A comment started in the web UI.");
    assert_eq!(
        (note.discussion_id.as_deref(), note.subject.as_ref()),
        (None, None)
    );

    let note = parse_draft_note(fixture!("draft-note-create.new-line.response.json")).unwrap();
    assert_eq!(note.id, 901);
    assert_eq!(
        note.subject,
        Some(ThreadSubject::Lines {
            anchor: Anchor {
                path: "src/auth/jwt.rs".to_owned(),
                side: Side::New,
                start_line: 14,
                end_line: 14,
                sha: sha(V2),
            }
        })
    );
    assert!(matches!(
        parse_draft_note(b"[]"),
        Err(ParseError::Json { .. })
    ));
}

#[test]
fn approvals() {
    let approvals = parse_approvals(fixture!("approve.response.json")).unwrap();
    assert!(approvals.approved && approvals.user_has_approved);
    assert_eq!(approvals.approved_by, ["carol"]);
}

#[test]
fn error_bodies() {
    let error = parse_error(fixture!("error-401.json")).unwrap();
    assert_eq!(error.message, "401 Unauthorized");
    let error = parse_error(fixture!("error-409-sha-mismatch.json")).unwrap();
    assert!(error.message.starts_with("SHA does not match HEAD"));
    let error = parse_error(fixture!("error-400-line-not-in-diff.json")).unwrap();
    assert_eq!(error.details.len(), 1);
    assert!(error.message.starts_with("base: Note"));
    let error = parse_error(fixture!("error-403-insufficient-scope.json")).unwrap();
    assert_eq!(error.message, "insufficient_scope");
    assert_eq!(error.details.len(), 1);
    assert!(matches!(
        parse_error(b"{}"),
        Err(ParseError::Invalid { .. })
    ));
    assert!(matches!(parse_error(b"oops"), Err(ParseError::Json { .. })));
}
