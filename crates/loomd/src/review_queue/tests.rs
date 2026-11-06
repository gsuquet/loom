use std::path::PathBuf;

use loom_forge::github::parse_pull;
use loom_forge::gitlab::parse_merge_request;
use loom_forge::model::{Comment, ThreadSubject};

use super::*;

const V1: &str = "7ebf283bfee25ab48f95c0c55181a4ca66b4c990";
const V2: &str = "84280c47b6aa128c645b1fbea54aded8b32e8463";

fn fixture(path: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/forge")
        .join(path);
    std::fs::read(path).unwrap()
}

fn sha(text: &str) -> Sha {
    Sha::try_from(text.to_owned()).unwrap()
}

fn context(name: &str) -> ContextName {
    name.parse().unwrap()
}

fn id(text: &str) -> TaskId {
    text.parse().unwrap()
}

/// An open MR on V2, where I am a requested reviewer, without a task and without an entry.
fn seen() -> EntrySeen {
    EntrySeen {
        tracked: false,
        has_entry: false,
        state: MrState::Open,
        head: sha(V2),
        requested: true,
        verdict_on_head: false,
        requested_after_verdict: false,
        own: false,
        archived: None,
        other_reasons: Vec::new(),
        parked: false,
        has_seen_at: false,
    }
}

fn requested_reason() -> Vec<QueueReason> {
    vec![QueueReason::ReviewRequested]
}

#[test]
#[expect(clippy::too_many_lines, reason = "one row per rule")]
fn entry_decision_table() {
    use EntryDecision::{Absent, Add, Keep, Remove};
    let entry = || EntrySeen {
        has_entry: true,
        ..seen()
    };
    let archived = |head: Option<&str>, requested| {
        Some(ArchivedSeen {
            head: head.map(sha),
            requested,
        })
    };
    let table: Vec<(&str, EntrySeen, EntryDecision)> = vec![
        ("requested, open: a new entry", seen(), Add),
        ("requested, open, an entry: it stays", entry(), Keep),
        (
            "my verdict on the head: no entry",
            EntrySeen {
                verdict_on_head: true,
                ..seen()
            },
            Absent,
        ),
        (
            "my verdict on the head: the entry goes",
            EntrySeen {
                verdict_on_head: true,
                ..entry()
            },
            Remove,
        ),
        (
            "my verdict on the head, a newer request: an entry",
            EntrySeen {
                verdict_on_head: true,
                requested_after_verdict: true,
                ..seen()
            },
            Add,
        ),
        (
            "my verdict on the head, a newer request: the entry stays",
            EntrySeen {
                verdict_on_head: true,
                requested_after_verdict: true,
                ..entry()
            },
            Keep,
        ),
        (
            "a newer request does not count when the request is gone",
            EntrySeen {
                verdict_on_head: true,
                requested_after_verdict: true,
                requested: false,
                ..seen()
            },
            Absent,
        ),
        (
            "not requested: no entry",
            EntrySeen {
                requested: false,
                ..seen()
            },
            Absent,
        ),
        (
            "request removed: the entry goes",
            EntrySeen {
                requested: false,
                ..entry()
            },
            Remove,
        ),
        (
            "merged: no entry",
            EntrySeen {
                state: MrState::Merged,
                ..seen()
            },
            Absent,
        ),
        (
            "merged: the entry goes",
            EntrySeen {
                state: MrState::Merged,
                ..entry()
            },
            Remove,
        ),
        (
            "closed: no entry",
            EntrySeen {
                state: MrState::Closed,
                ..seen()
            },
            Absent,
        ),
        (
            "closed: the entry goes",
            EntrySeen {
                state: MrState::Closed,
                ..entry()
            },
            Remove,
        ),
        (
            "my own MR (GitLab lets me request myself): no entry",
            EntrySeen {
                own: true,
                ..seen()
            },
            Absent,
        ),
        (
            "my own MR: the entry goes",
            EntrySeen {
                own: true,
                ..entry()
            },
            Remove,
        ),
        (
            "a Review task: no entry, though I am requested",
            EntrySeen {
                tracked: true,
                ..seen()
            },
            Absent,
        ),
        (
            "a Review task and an entry (a crash in a start): the entry goes",
            EntrySeen {
                tracked: true,
                ..entry()
            },
            Remove,
        ),
        (
            "archived, same head, was requested: nothing is new",
            EntrySeen {
                archived: archived(Some(V2), Some(true)),
                ..seen()
            },
            Absent,
        ),
        (
            "archived, new head: an entry",
            EntrySeen {
                archived: archived(Some(V1), Some(true)),
                ..seen()
            },
            Add,
        ),
        (
            "archived, same head, new request: an entry",
            EntrySeen {
                archived: archived(Some(V2), Some(false)),
                ..seen()
            },
            Add,
        ),
        (
            "archived, a mirror that cannot be read: an entry",
            EntrySeen {
                archived: archived(None, None),
                ..seen()
            },
            Add,
        ),
        (
            "archived, no sync state, same head: nothing is new",
            EntrySeen {
                archived: archived(Some(V2), None),
                ..seen()
            },
            Absent,
        ),
        (
            "archived, an entry already added: it stays, whatever the mirror says",
            EntrySeen {
                archived: archived(Some(V2), Some(true)),
                ..entry()
            },
            Keep,
        ),
        (
            "archived, new request, my verdict on the head: no entry",
            EntrySeen {
                archived: archived(Some(V2), Some(false)),
                verdict_on_head: true,
                ..seen()
            },
            Absent,
        ),
        (
            "archived, new request, a new verdict on the head: the entry goes and stays gone",
            EntrySeen {
                archived: archived(Some(V2), Some(false)),
                verdict_on_head: true,
                ..entry()
            },
            Remove,
        ),
        (
            "archived, new head, not requested: no entry",
            EntrySeen {
                archived: archived(Some(V1), Some(true)),
                requested: false,
                ..seen()
            },
            Absent,
        ),
    ];
    for (name, seen, expected) in table {
        assert_eq!(decide_entry(&seen), expected, "{name}");
    }
}

/// (name, context of the request, tasks, entries, target)
type StartRow<'a> = (
    &'a str,
    Option<&'a ContextName>,
    Vec<(ContextName, TaskId, Option<String>)>,
    Vec<(ContextName, Option<String>)>,
    StartTarget,
);

#[test]
fn start_target_table() {
    let (a, b) = (context("a"), context("b"));
    let github = || Some("github.com".to_owned());
    let other = || Some("github.example.com".to_owned());
    let task = id("k3q7zm2a");
    let second = id("r1000000");
    let table: Vec<StartRow<'_>> = vec![
        ("nothing", None, vec![], vec![], StartTarget::NotFound),
        (
            "an entry",
            None,
            vec![],
            vec![(a.clone(), github())],
            StartTarget::Entry(a.clone()),
        ),
        (
            "a task",
            None,
            vec![(a.clone(), task.clone(), github())],
            vec![],
            StartTarget::Task(task.clone()),
        ),
        (
            "a task and its entry (a crash in a start): the task, never a second one",
            None,
            vec![(a.clone(), task.clone(), github())],
            vec![(a.clone(), github())],
            StartTarget::Task(task.clone()),
        ),
        (
            "a task of another host with this key, and an entry: the entry",
            None,
            vec![(a.clone(), task.clone(), other())],
            vec![(a.clone(), github())],
            StartTarget::Entry(a.clone()),
        ),
        (
            "a task in one context, an entry in another: the task, never a second one",
            None,
            vec![(b.clone(), task.clone(), github())],
            vec![(a.clone(), github())],
            StartTarget::Task(task.clone()),
        ),
        (
            "the request names the context of the entry: still the task",
            Some(&a),
            vec![(b.clone(), task.clone(), github())],
            vec![(a.clone(), github())],
            StartTarget::Task(task.clone()),
        ),
        (
            "two entries",
            None,
            vec![],
            vec![(a.clone(), github()), (b.clone(), github())],
            StartTarget::Ambiguous(vec![a.clone(), b.clone()]),
        ),
        (
            "two entries, the request names one",
            Some(&b),
            vec![],
            vec![(a.clone(), github()), (b.clone(), github())],
            StartTarget::Entry(b.clone()),
        ),
        (
            "two tasks",
            None,
            vec![
                (a.clone(), task.clone(), github()),
                (b.clone(), second.clone(), github()),
            ],
            vec![],
            StartTarget::Ambiguous(vec![a.clone(), b.clone()]),
        ),
        (
            "two tasks, the request names one",
            Some(&b),
            vec![
                (a.clone(), task, github()),
                (b.clone(), second.clone(), github()),
            ],
            vec![],
            StartTarget::Task(second),
        ),
        (
            "another context than the one named, and no task",
            Some(&b),
            vec![],
            vec![(context("a"), github())],
            StartTarget::NotFound,
        ),
    ];
    for (name, named, tasks, entries, expected) in table {
        assert_eq!(start_target(named, &tasks, &entries), expected, "{name}");
    }
}

#[test]
fn start_refuses_a_closed_mr_my_own_and_a_request_that_is_gone() {
    use MrState::{Closed, Merged, Open};
    let table = [
        (Open, false, false, Ok(())),
        (Merged, false, false, Err(StartRefusal::NotOpen(Merged))),
        (Closed, false, false, Err(StartRefusal::NotOpen(Closed))),
        (Open, true, false, Err(StartRefusal::Own)),
        (Open, false, true, Err(StartRefusal::RequestGone)),
        // What leaves first names the refusal.
        (Merged, true, true, Err(StartRefusal::NotOpen(Merged))),
        (Open, true, true, Err(StartRefusal::Own)),
    ];
    for (state, own, gone, expected) in table {
        assert_eq!(
            can_start(state, own, gone),
            expected,
            "{state:?} {own} {gone}"
        );
    }
}

#[test]
fn an_entry_holds_the_fields_of_the_mr_and_keeps_what_the_poll_does_not_read() {
    let mr = parse_pull(&fixture("github/pull-412.v1.json")).unwrap();
    let entry = entry_of(&mr, None, requested_reason());
    assert_eq!(entry.reasons, vec![QueueReason::ReviewRequested]);
    assert_eq!(entry.title, "Auth refactor");
    assert_eq!(entry.head_sha, sha(V1));
    assert_eq!(entry.seen_at, None);

    // The same MR gives the same bytes: a poll with no change writes nothing.
    let bytes = entry_bytes(&entry).unwrap();
    assert_eq!(
        entry_bytes(&entry_of(&mr, Some(&entry), requested_reason())).unwrap(),
        bytes
    );
    assert_eq!(read_entry_bytes(&bytes), Some(entry.clone()));
    assert_eq!(read_entry_bytes(b"{"), None);

    // `seen_at` stays, and the reason is not doubled.
    let seen_at = Timestamp::from_unix_secs(5).unwrap();
    let opened = QueueEntry {
        seen_at: Some(seen_at),
        ..entry
    };
    let kept = entry_of(&mr, Some(&opened), requested_reason());
    assert_eq!(kept.seen_at, Some(seen_at));
    assert_eq!(kept.reasons, vec![QueueReason::ReviewRequested]);
}

#[test]
fn a_gitlab_entry_keeps_the_project_id_and_the_file_name_is_the_mirror_name() {
    let mr = parse_merge_request(&fixture("gitlab/merge-request-88.v2.json")).unwrap();
    let entry = entry_of(&mr, None, requested_reason());
    assert!(matches!(
        entry.forge_ids,
        MrForgeIds::Gitlab { project_id: 77, .. }
    ));
    assert_eq!(entry_name(&mr.key), "gitlab-company-a%2Fcheckout%2188.json");
    let api = entry.to_api(&context("company-a"));
    assert_eq!(api.key, "company-a/checkout!88");
    assert_eq!(api.context.as_str(), "company-a");
    assert_eq!(api.reasons, vec![QueueReason::ReviewRequested]);
    assert_eq!(
        queue_dir(std::path::Path::new("/s")),
        PathBuf::from("/s/queue/mrs")
    );
}

/// (name, row, tracked, left, shown, step, head)
type ApprovalRow<'a> = (
    &'a str,
    Option<&'a Sha>,
    bool,
    bool,
    Option<Option<&'a SeenVerdict>>,
    ApprovalStep,
    Option<Sha>,
);

#[test]
fn approval_step_table() {
    use ApprovalStep::{Drop, Keep, Record};
    let (v1, v2) = (sha(V1), sha(V2));
    let approved = SeenVerdict {
        sha: sha(V2),
        id: "approved".to_owned(),
    };
    let shown = Some(Some(&approved));
    let old = Some(&v1);
    let table: Vec<ApprovalRow<'_>> = vec![
        (
            "first shown: recorded",
            None,
            false,
            false,
            shown,
            Record(v2.clone()),
            Some(v2),
        ),
        (
            "shown after a push: first head",
            old,
            false,
            false,
            shown,
            Keep,
            Some(sha(V1)),
        ),
        (
            "not read (a 304): no change",
            old,
            false,
            false,
            None,
            Keep,
            Some(sha(V1)),
        ),
        ("not read, no row", None, false, false, None, Keep, None),
        (
            "reset: forgotten",
            old,
            false,
            false,
            Some(None),
            Drop,
            None,
        ),
        (
            "no approval, no row",
            None,
            false,
            false,
            Some(None),
            Keep,
            None,
        ),
        (
            "the MR left: forgotten",
            old,
            false,
            true,
            shown,
            Drop,
            None,
        ),
        (
            "the MR left, no row: not recorded",
            None,
            false,
            true,
            shown,
            Keep,
            None,
        ),
        (
            "a Review task takes over",
            old,
            true,
            false,
            shown,
            Drop,
            None,
        ),
        (
            "a Review task, no row: not recorded",
            None,
            true,
            false,
            shown,
            Keep,
            None,
        ),
    ];
    for (name, row, tracked, left, shown, step, head) in table {
        assert_eq!(
            approval_step(row, tracked, left, shown),
            (step, head),
            "{name}"
        );
    }
}

#[test]
fn entry_seen_takes_each_fact_from_the_best_source() {
    let mr = parse_pull(&fixture("github/pull-412.v2.json")).unwrap();
    let v1_mr = parse_pull(&fixture("github/pull-412.v1.json")).unwrap();
    let entry = entry_of(&v1_mr, None, requested_reason());
    let base = EntryInput {
        read: None,
        stored: None,
        archived_mr: None,
        archived_state: None,
        archived: false,
        verdict: None,
        requested: None,
        tracked: false,
        own: false,
        requested_after_verdict: false,
        reasons: &ReasonsRead::UNKNOWN,
    };
    // Nothing to compare with: a 304 with neither an entry nor a mirror.
    assert_eq!(entry_seen(&base), None);

    // The read wins over the entry, and the entry over the archive.
    let seen = entry_seen(&EntryInput {
        read: Some(&mr),
        stored: Some(&entry),
        requested: Some(true),
        ..base
    })
    .unwrap();
    assert_eq!(seen.head, sha(V2));
    assert!(seen.has_entry);
    let seen = entry_seen(&EntryInput {
        stored: Some(&entry),
        archived_mr: Some(&mr),
        ..base
    })
    .unwrap();
    assert_eq!((seen.head.clone(), seen.state), (sha(V1), MrState::Open));
    // Unknown request: an entry stays requested.
    assert!(seen.requested);

    // Unknown request, no entry: as in the mirror of the archived task.
    let state = SyncState {
        v: SyncState::VERSION,
        requested: true,
        verdict: Some(SeenVerdict {
            sha: sha(V2),
            id: "PRR_1".to_owned(),
        }),
        shown: true,
    };
    let seen = entry_seen(&EntryInput {
        archived_mr: Some(&mr),
        archived_state: Some(&state),
        archived: true,
        ..base
    })
    .unwrap();
    assert!(seen.requested);
    // A verdict not read: the one that the mirror remembers.
    assert!(seen.verdict_on_head);
    assert_eq!(
        seen.archived,
        Some(ArchivedSeen {
            head: Some(sha(V2)),
            requested: Some(true)
        })
    );

    // A verdict read on another head is not on the head; none read at all, neither.
    let on_v1 = Some(SeenVerdict {
        sha: sha(V1),
        id: "PRR_2".to_owned(),
    });
    let seen = entry_seen(&EntryInput {
        read: Some(&mr),
        verdict: Some(&on_v1),
        requested: Some(false),
        requested_after_verdict: true,
        own: true,
        tracked: true,
        ..base
    })
    .unwrap();
    assert!(!seen.verdict_on_head);
    assert!(!seen.requested);
    assert!(seen.requested_after_verdict && seen.own && seen.tracked);
}

#[test]
fn an_entry_of_another_format_version_is_not_read() {
    let mr = parse_pull(&fixture("github/pull-412.v1.json")).unwrap();
    let entry = QueueEntry {
        v: QueueEntry::VERSION + 1,
        ..entry_of(&mr, None, requested_reason())
    };
    assert_eq!(read_entry_bytes(&entry_bytes(&entry).unwrap()), None);
}

#[test]
fn the_queue_commit_says_what_changed() {
    assert_eq!(
        queue_message(1, 2, 3),
        "queue: 1 added, 2 updated, 3 removed"
    );
}

fn time(text: &str) -> Timestamp {
    serde_json::from_value(serde_json::json!(text)).unwrap()
}

fn thread(comments: &[(&str, &str)]) -> Thread {
    Thread {
        id: "d1".to_owned(),
        subject: ThreadSubject::General,
        outdated: false,
        resolved: false,
        comments: comments
            .iter()
            .enumerate()
            .map(|(index, (author, at))| Comment {
                id: index.to_string(),
                author: (*author).to_owned(),
                body: "text".to_owned(),
                created_at: time(at),
            })
            .collect(),
    }
}

#[test]
fn assigned_table() {
    let of = |names: &[&str]| -> Vec<String> { names.iter().map(|&n| n.to_owned()).collect() };
    let table = [
        ("assigned to me", of(&["carol"]), false, true),
        ("another case of my login", of(&["Carol"]), false, true),
        ("among others", of(&["alice", "carol"]), false, true),
        ("assigned to another", of(&["alice"]), false, false),
        ("no assignee", of(&[]), false, false),
        ("my own MR, assigned to me", of(&["carol"]), true, false),
    ];
    for (name, assignees, own, expected) in table {
        assert_eq!(is_assigned(&assignees, "carol", own), expected, "{name}");
    }
}

#[test]
fn new_comment_table() {
    const T1: &str = "2026-10-21T10:00:00Z";
    const T2: &str = "2026-10-21T11:00:00Z";
    const T3: &str = "2026-10-21T12:00:00Z";
    let table: Vec<(&str, Vec<Thread>, Option<&str>)> = vec![
        ("no note", vec![], None),
        ("a note of mine only", vec![thread(&[("carol", T1)])], None),
        (
            "my login in another case",
            vec![thread(&[("Carol", T1)])],
            None,
        ),
        (
            "a note from someone else",
            vec![thread(&[("bob", T1)])],
            Some(T1),
        ),
        (
            "I replied after it",
            vec![thread(&[("bob", T1), ("carol", T2)])],
            None,
        ),
        (
            "someone wrote after my reply",
            vec![thread(&[("bob", T1), ("carol", T2), ("bob", T3)])],
            Some(T3),
        ),
        (
            "the newest of two people",
            vec![thread(&[("bob", T1), ("alice", T2)])],
            Some(T2),
        ),
        (
            "my reply in another thread counts",
            vec![thread(&[("bob", T1)]), thread(&[("carol", T2)])],
            None,
        ),
        (
            "a note in the same second as mine is not newer",
            vec![thread(&[("carol", T1), ("bob", T1)])],
            None,
        ),
    ];
    for (name, threads, expected) in table {
        assert_eq!(new_comment(&threads, "carol"), expected.map(time), "{name}");
    }
}

#[test]
fn a_note_counts_only_when_newer_than_seen_at() {
    let (t1, t2) = (time("2026-10-21T10:00:00Z"), time("2026-10-21T11:00:00Z"));
    let table = [
        ("no note, never seen", None, None, false),
        ("no note, seen", None, Some(t1), false),
        ("a note, never seen", Some(t1), None, true),
        ("a note older than seen_at", Some(t1), Some(t2), false),
        ("a note in the same second", Some(t1), Some(t1), false),
        ("a note newer than seen_at", Some(t2), Some(t1), true),
    ];
    for (name, newest, seen_at, expected) in table {
        assert_eq!(has_new_comment(newest, seen_at), expected, "{name}");
    }
}

#[test]
fn pipeline_failed_table() {
    use PipelineOutcome::{Failed, Other};
    let (before, since, after) = (
        time("2026-10-21T09:00:00Z"),
        time("2026-10-21T10:00:00Z"),
        time("2026-10-21T11:00:00Z"),
    );
    let on = |commit: PipelineCommit, outcome, created_at| HeadPipeline {
        commit,
        outcome,
        created_at,
    };
    let commit = |text: &str| PipelineCommit::Commit(Some(sha(text)));
    let merged = PipelineCommit::MergedResult;
    let table = [
        ("no pipeline", None, None, Some(false)),
        (
            "failed on the head",
            Some(on(commit(V2), Failed, None)),
            None,
            Some(true),
        ),
        (
            "failed on an older head",
            Some(on(commit(V1), Failed, None)),
            None,
            Some(false),
        ),
        (
            "failed, with no SHA to compare",
            Some(on(PipelineCommit::Commit(None), Failed, None)),
            None,
            Some(false),
        ),
        (
            "another state on the head",
            Some(on(commit(V2), Other, None)),
            None,
            Some(false),
        ),
        (
            "a merged result after the head",
            Some(on(merged.clone(), Failed, Some(after))),
            Some(since),
            Some(true),
        ),
        (
            "a merged result in the second of the head",
            Some(on(merged.clone(), Failed, Some(since))),
            Some(since),
            Some(true),
        ),
        (
            "a merged result before the head",
            Some(on(merged.clone(), Failed, Some(before))),
            Some(since),
            Some(false),
        ),
        (
            "a merged result, the head has no date",
            Some(on(merged.clone(), Failed, Some(after))),
            None,
            None,
        ),
        (
            "a merged result with no date",
            Some(on(merged.clone(), Failed, None)),
            Some(since),
            None,
        ),
        (
            "a merged result in another state",
            Some(on(merged, Other, None)),
            None,
            Some(false),
        ),
    ];
    for (name, pipeline, head_date, expected) in table {
        assert_eq!(
            pipeline_failed(pipeline.as_ref(), &sha(V2), head_date),
            expected,
            "{name}"
        );
    }
}

#[test]
fn only_a_failed_merged_result_needs_the_time_of_the_head() {
    use PipelineOutcome::{Failed, Other};
    let on = |commit, outcome| {
        Some(HeadPipeline {
            commit,
            outcome,
            created_at: None,
        })
    };
    assert!(needs_head_time(
        on(PipelineCommit::MergedResult, Failed).as_ref()
    ));
    assert!(!needs_head_time(
        on(PipelineCommit::MergedResult, Other).as_ref()
    ));
    assert!(!needs_head_time(
        on(PipelineCommit::Commit(None), Failed).as_ref()
    ));
    assert!(!needs_head_time(None));
}

#[test]
fn the_time_of_a_head_is_its_first_version() {
    // No mirror version: a null in its other SHAs does not stop it from dating the head.
    let version = |head: &str, at: Option<&str>| VersionRead {
        head_sha: sha(head),
        created_at: at.map(time),
        version: None,
    };
    let versions = [
        version(V2, Some("2026-10-21T11:00:00Z")),
        version(V1, Some("2026-10-21T08:00:00Z")),
        version(V2, Some("2026-10-21T10:00:00Z")),
        version(V2, None),
    ];
    assert_eq!(
        head_since(&versions, &sha(V2)),
        Some(time("2026-10-21T10:00:00Z"))
    );
    assert_eq!(head_since(&versions[..1], &sha(V1)), None);
    assert_eq!(head_since(&[], &sha(V2)), None);
}

#[test]
fn a_move_onto_a_file_keeps_the_reasons_and_the_later_seen_at() {
    use QueueReason::{Assigned, Mentioned};
    let mr = parse_merge_request(&fixture("gitlab/merge-request-88.v2.json")).unwrap();
    let (t1, t2) = (time("2026-10-21T10:00:00Z"), time("2026-10-21T11:00:00Z"));
    let entry = |reasons: Vec<QueueReason>, seen_at| QueueEntry {
        seen_at,
        ..entry_of(&mr, None, reasons)
    };
    let table = [
        (
            "parked here, reasons there",
            entry(vec![], Some(t1)),
            entry(vec![Assigned], None),
            vec![Assigned],
            Some(t1),
        ),
        (
            "reasons here, parked there",
            entry(vec![Mentioned], None),
            entry(vec![], Some(t2)),
            vec![Mentioned],
            Some(t2),
        ),
        (
            "both, in the fixed order",
            entry(vec![Mentioned], Some(t2)),
            entry(vec![Assigned], Some(t1)),
            vec![Assigned, Mentioned],
            Some(t2),
        ),
        (
            "both parked",
            entry(vec![], Some(t1)),
            entry(vec![], Some(t2)),
            vec![],
            Some(t2),
        ),
    ];
    for (name, target, source, reasons, seen_at) in table {
        let merged = merged_entry(&target, &source);
        assert_eq!(
            (merged.reasons, merged.seen_at),
            (reasons, seen_at),
            "{name}"
        );
    }
}

#[test]
fn ready_to_merge_table() {
    use MergeStatus::{Computing, Mergeable, NotMergeable};
    let table = [
        ("mergeable, one approval", Mergeable, false, 1, Some(true)),
        ("mergeable, two approvals", Mergeable, false, 2, Some(true)),
        ("mergeable, no approval", Mergeable, false, 0, Some(false)),
        ("a draft", Mergeable, true, 1, Some(false)),
        ("not mergeable", NotMergeable, false, 1, Some(false)),
        ("the forge computes it", Computing, false, 1, None),
        (
            "a draft while the forge computes it",
            Computing,
            true,
            1,
            Some(false),
        ),
    ];
    for (name, status, draft, approvals, expected) in table {
        assert_eq!(ready_to_merge(status, draft, approvals), expected, "{name}");
    }
}

#[test]
fn a_full_read_tells_each_reason_and_only_my_own_mr_has_own_reasons() {
    let mr = parse_merge_request(&fixture("gitlab/merge-request-88.v2.json")).unwrap();
    let failed = MrSignals {
        assignees: vec!["carol".to_owned()],
        head_pipeline: Some(HeadPipeline {
            commit: PipelineCommit::Commit(Some(mr.version.head_sha.clone())),
            outcome: PipelineOutcome::Failed,
            created_at: None,
        }),
        merge_status: MergeStatus::Mergeable,
    };
    let threads = [thread(&[("bob", "2026-10-21T10:00:00Z")])];
    let facts = |author: &str| {
        let mut mr = mr.clone();
        mr.author = author.to_owned();
        mr
    };
    let theirs = facts("alice");
    let read = reasons_read(&MrFacts {
        mr: &theirs,
        signals: &failed,
        login: "carol",
        threads: None,
        approvals: 1,
        mentioned: Some(true),
        head_since: None,
    });
    assert_eq!(
        read,
        ReasonsRead {
            assigned: Some(true),
            mentioned: Some(true),
            new_comment: Some(None),
            pipeline_failed: Some(false),
            ready_to_merge: Some(false),
        }
    );
    let mine = facts("carol");
    let read = reasons_read(&MrFacts {
        mr: &mine,
        signals: &failed,
        login: "carol",
        threads: Some(&threads),
        approvals: 1,
        mentioned: None,
        head_since: None,
    });
    assert_eq!(
        read,
        ReasonsRead {
            assigned: Some(false),
            mentioned: None,
            new_comment: Some(Some(time("2026-10-21T10:00:00Z"))),
            pipeline_failed: Some(true),
            ready_to_merge: Some(true),
        }
    );
    // Discussions not read: the reason keeps its value.
    let read = reasons_read(&MrFacts {
        mr: &mine,
        signals: &failed,
        login: "carol",
        threads: None,
        approvals: 1,
        mentioned: None,
        head_since: None,
    });
    assert_eq!(read.new_comment, None);
}

/// Name, the reasons in the file, what the poll read, `seen_at`, the reasons after.
type OtherReasonsRow = (
    &'static str,
    Vec<QueueReason>,
    ReasonsRead,
    Option<Timestamp>,
    Vec<QueueReason>,
);

#[test]
#[expect(clippy::too_many_lines, reason = "one row per rule")]
fn other_reasons_table() {
    use QueueReason::{
        Assigned, Mentioned, OwnNewComments, OwnPipelineFailed, OwnReadyToMerge, ReviewRequested,
    };
    let (t1, t2) = (time("2026-10-21T10:00:00Z"), time("2026-10-21T11:00:00Z"));
    let unknown = ReasonsRead::UNKNOWN;
    let table: Vec<OtherReasonsRow> = vec![
        (
            "nothing known, nothing stored",
            vec![],
            unknown,
            None,
            vec![],
        ),
        (
            "review_requested is not its business",
            vec![ReviewRequested],
            unknown,
            None,
            vec![],
        ),
        (
            "nothing known: each stays",
            vec![
                Assigned,
                Mentioned,
                OwnNewComments,
                OwnPipelineFailed,
                OwnReadyToMerge,
            ],
            unknown,
            None,
            vec![
                Assigned,
                Mentioned,
                OwnNewComments,
                OwnPipelineFailed,
                OwnReadyToMerge,
            ],
        ),
        (
            "assigned comes",
            vec![],
            ReasonsRead {
                assigned: Some(true),
                ..unknown
            },
            None,
            vec![Assigned],
        ),
        (
            "assigned goes",
            vec![Assigned],
            ReasonsRead {
                assigned: Some(false),
                ..unknown
            },
            None,
            vec![],
        ),
        (
            "a list that failed keeps the mention",
            vec![Mentioned],
            ReasonsRead {
                assigned: Some(false),
                ..unknown
            },
            None,
            vec![Mentioned],
        ),
        (
            "the to-do is done",
            vec![Mentioned],
            ReasonsRead {
                mentioned: Some(false),
                ..unknown
            },
            None,
            vec![],
        ),
        (
            "one goes, the other stays",
            vec![Assigned, Mentioned],
            ReasonsRead {
                mentioned: Some(false),
                ..unknown
            },
            None,
            vec![Assigned],
        ),
        (
            "a note newer than seen_at",
            vec![],
            ReasonsRead {
                new_comment: Some(Some(t2)),
                ..unknown
            },
            Some(t1),
            vec![OwnNewComments],
        ),
        (
            "a note older than seen_at",
            vec![],
            ReasonsRead {
                new_comment: Some(Some(t1)),
                ..unknown
            },
            Some(t2),
            vec![],
        ),
        (
            "I replied",
            vec![OwnNewComments],
            ReasonsRead {
                new_comment: Some(None),
                ..unknown
            },
            None,
            vec![],
        ),
        (
            "the pipeline fails",
            vec![],
            ReasonsRead {
                pipeline_failed: Some(true),
                ..unknown
            },
            None,
            vec![OwnPipelineFailed],
        ),
        (
            "a new pipeline is not failed",
            vec![OwnPipelineFailed],
            ReasonsRead {
                pipeline_failed: Some(false),
                ..unknown
            },
            None,
            vec![],
        ),
        (
            "ready",
            vec![],
            ReasonsRead {
                ready_to_merge: Some(true),
                ..unknown
            },
            None,
            vec![OwnReadyToMerge],
        ),
        (
            "while GitLab computes, ready stays",
            vec![OwnReadyToMerge],
            ReasonsRead {
                ready_to_merge: None,
                pipeline_failed: Some(false),
                ..unknown
            },
            None,
            vec![OwnReadyToMerge],
        ),
        (
            "not ready any more",
            vec![OwnReadyToMerge],
            ReasonsRead {
                ready_to_merge: Some(false),
                ..unknown
            },
            None,
            vec![],
        ),
        (
            "the MR is gone",
            vec![
                Assigned,
                Mentioned,
                OwnNewComments,
                OwnPipelineFailed,
                OwnReadyToMerge,
            ],
            ReasonsRead::NONE_HOLD,
            None,
            vec![],
        ),
        (
            "the order is fixed",
            vec![OwnReadyToMerge, Assigned],
            unknown,
            None,
            vec![Assigned, OwnReadyToMerge],
        ),
    ];
    for (name, stored, read, seen_at, expected) in table {
        assert_eq!(other_reasons(&stored, &read, seen_at), expected, "{name}");
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "one row per rule")]
fn entry_decision_with_other_reasons() {
    use EntryDecision::{Absent, Add, DropParked, Keep, Park, Remove};
    use QueueReason::{Assigned, OwnPipelineFailed};
    let assigned = || EntrySeen {
        requested: false,
        other_reasons: vec![Assigned],
        ..seen()
    };
    let mine = || EntrySeen {
        requested: false,
        own: true,
        other_reasons: vec![OwnPipelineFailed],
        ..seen()
    };
    let archived = |head: &str, requested: Option<bool>| {
        Some(ArchivedSeen {
            head: Some(sha(head)),
            requested,
        })
    };
    let table: Vec<(&str, EntrySeen, EntryDecision)> = vec![
        ("assigned, not requested: a new entry", assigned(), Add),
        (
            "the request goes, assigned stays: the entry stays",
            EntrySeen {
                has_entry: true,
                ..assigned()
            },
            Keep,
        ),
        (
            "my verdict on the head, assigned stays: the entry stays",
            EntrySeen {
                has_entry: true,
                requested: true,
                verdict_on_head: true,
                ..assigned()
            },
            Keep,
        ),
        (
            "the last reason goes: the entry goes",
            EntrySeen {
                has_entry: true,
                other_reasons: vec![],
                ..assigned()
            },
            Remove,
        ),
        (
            "merged: the entry goes with its reasons",
            EntrySeen {
                has_entry: true,
                state: MrState::Merged,
                ..assigned()
            },
            Remove,
        ),
        (
            "a task: no entry for any reason",
            EntrySeen {
                tracked: true,
                ..assigned()
            },
            Absent,
        ),
        (
            "a task: the entry goes for any reason",
            EntrySeen {
                tracked: true,
                has_entry: true,
                ..assigned()
            },
            Remove,
        ),
        ("my own MR with a reason: an entry", mine(), Add),
        (
            "my own MR requested of me, no reason: none",
            EntrySeen {
                requested: true,
                other_reasons: vec![],
                ..mine()
            },
            Absent,
        ),
        (
            "archived, assigned, same head, not requested: none",
            EntrySeen {
                archived: archived(V2, Some(false)),
                ..assigned()
            },
            Absent,
        ),
        (
            "archived, assigned, new head: an entry",
            EntrySeen {
                archived: archived(V1, Some(false)),
                ..assigned()
            },
            Add,
        ),
        (
            "archived, my own MR, same head: an entry",
            EntrySeen {
                archived: archived(V2, Some(false)),
                ..mine()
            },
            Add,
        ),
        (
            "seen, no reason left, my open MR: the file stays",
            EntrySeen {
                has_entry: true,
                has_seen_at: true,
                other_reasons: vec![],
                ..mine()
            },
            Park,
        ),
        (
            "no reason left, never seen: the file goes",
            EntrySeen {
                has_entry: true,
                other_reasons: vec![],
                ..mine()
            },
            Remove,
        ),
        (
            "seen, no reason left, not my MR: the file goes",
            EntrySeen {
                has_entry: true,
                has_seen_at: true,
                other_reasons: vec![],
                ..assigned()
            },
            Remove,
        ),
        (
            "seen, merged: the file goes",
            EntrySeen {
                has_entry: true,
                has_seen_at: true,
                state: MrState::Merged,
                other_reasons: vec![],
                ..mine()
            },
            Remove,
        ),
        (
            "parked, nothing new: the file stays",
            EntrySeen {
                parked: true,
                has_seen_at: true,
                other_reasons: vec![],
                ..mine()
            },
            Absent,
        ),
        (
            "parked, a reason again: an entry",
            EntrySeen {
                parked: true,
                has_seen_at: true,
                ..mine()
            },
            Add,
        ),
        (
            "parked, merged: the file goes",
            EntrySeen {
                parked: true,
                has_seen_at: true,
                state: MrState::Merged,
                other_reasons: vec![],
                ..mine()
            },
            DropParked,
        ),
        (
            "parked, gone: the file goes",
            EntrySeen {
                parked: true,
                has_seen_at: true,
                own: false,
                other_reasons: vec![],
                ..mine()
            },
            DropParked,
        ),
    ];
    for (name, seen, expected) in table {
        assert_eq!(decide_entry(&seen), expected, "{name}");
    }
}

#[test]
fn entry_reasons_table() {
    use QueueReason::{Assigned, OwnPipelineFailed, ReviewRequested};
    let table: Vec<(&str, EntrySeen, Vec<QueueReason>)> = vec![
        ("requested", seen(), vec![ReviewRequested]),
        (
            "requested and assigned, in order",
            EntrySeen {
                other_reasons: vec![Assigned],
                ..seen()
            },
            vec![ReviewRequested, Assigned],
        ),
        (
            "my verdict: only assigned",
            EntrySeen {
                verdict_on_head: true,
                other_reasons: vec![Assigned],
                ..seen()
            },
            vec![Assigned],
        ),
        (
            "my own MR: never requested",
            EntrySeen {
                own: true,
                other_reasons: vec![OwnPipelineFailed],
                ..seen()
            },
            vec![OwnPipelineFailed],
        ),
        (
            "no reason",
            EntrySeen {
                requested: false,
                ..seen()
            },
            vec![],
        ),
    ];
    for (name, seen, expected) in table {
        assert_eq!(entry_reasons(&seen), expected, "{name}");
    }
}

#[test]
fn a_304_keeps_the_review_reason_as_the_file_holds_it() {
    let mr = parse_merge_request(&fixture("gitlab/merge-request-88.v2.json")).unwrap();
    let held = |reasons: Vec<QueueReason>| entry_of(&mr, None, reasons);
    let input = |stored| EntryInput {
        read: None,
        stored,
        archived_mr: None,
        archived_state: None,
        archived: false,
        verdict: None,
        requested: Some(true),
        tracked: false,
        own: false,
        requested_after_verdict: false,
        reasons: &ReasonsRead::UNKNOWN,
    };
    // Still in the reviewers, and my verdict took this reason away at an earlier read.
    let assigned = held(vec![QueueReason::Assigned]);
    let seen = entry_seen(&input(Some(&assigned))).unwrap();
    assert_eq!(entry_reasons(&seen), [QueueReason::Assigned]);
    let requested = held(requested_reason());
    let seen = entry_seen(&input(Some(&requested))).unwrap();
    assert_eq!(entry_reasons(&seen), requested_reason());
    // An incomplete search with another reason stored does not invent a request.
    let seen = entry_seen(&EntryInput {
        requested: None,
        ..input(Some(&assigned))
    })
    .unwrap();
    assert_eq!(entry_reasons(&seen), [QueueReason::Assigned]);
    // A file with no reason is parked, not an entry.
    let parked = EntryInput {
        own: true,
        requested: Some(false),
        ..input(None)
    };
    let mut file = held(vec![]);
    file.seen_at = Some(time("2026-10-21T10:00:00Z"));
    let seen = entry_seen(&EntryInput {
        stored: Some(&file),
        ..parked
    })
    .unwrap();
    assert!(seen.parked && seen.has_seen_at && !seen.has_entry);
    assert_eq!(decide_entry(&seen), EntryDecision::Absent);
}

#[test]
fn an_open_entry_loses_only_its_comment_reason() {
    use QueueReason::{Assigned, OwnNewComments, OwnPipelineFailed};
    let mr = parse_merge_request(&fixture("gitlab/merge-request-88.v2.json")).unwrap();
    let at = time("2026-10-21T10:00:00Z");
    let entry = entry_of(&mr, None, vec![OwnNewComments, OwnPipelineFailed]);
    let opened = mark_seen(&entry, at);
    assert_eq!(opened.reasons, [OwnPipelineFailed]);
    assert_eq!(opened.seen_at, Some(at));
    assert_eq!(opened.head_sha, entry.head_sha);
    let only = entry_of(&mr, None, vec![OwnNewComments]);
    assert_eq!(mark_seen(&only, at).reasons, []);
    let other = entry_of(&mr, None, vec![Assigned]);
    assert_eq!(mark_seen(&other, at).reasons, [Assigned]);
    // The file keeps its `seen_at` when the poll rewrites it.
    let kept = entry_of(&mr, Some(&opened), vec![OwnPipelineFailed]);
    assert_eq!(kept.seen_at, Some(at));
}

#[test]
fn a_start_is_refused_only_when_no_reason_is_left() {
    let table = [
        ("my request is there", true, true, false, false),
        ("my request is gone", true, false, false, true),
        (
            "my request is gone, another reason holds",
            true,
            false,
            true,
            false,
        ),
        (
            "a team request: a read cannot tell",
            false,
            false,
            false,
            false,
        ),
    ];
    for (name, was_requested, reviewer, other_reason, expected) in table {
        assert_eq!(
            waits_no_more(was_requested, reviewer, other_reason),
            expected,
            "{name}"
        );
    }
}
