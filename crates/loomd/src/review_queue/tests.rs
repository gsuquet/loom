use std::path::PathBuf;

use loom_forge::github::parse_pull;
use loom_forge::gitlab::parse_merge_request;

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
        own: false,
        archived: None,
    }
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
            "two contexts",
            None,
            vec![(b.clone(), task.clone(), github())],
            vec![(a.clone(), github())],
            StartTarget::Ambiguous(vec![a.clone(), b.clone()]),
        ),
        (
            "two contexts, the request names one",
            Some(&a),
            vec![(b.clone(), task, github())],
            vec![(a.clone(), github())],
            StartTarget::Entry(a.clone()),
        ),
        (
            "another context than the one named",
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
fn start_refuses_a_closed_mr_and_my_own() {
    assert_eq!(can_start(MrState::Open, false), Ok(()));
    assert_eq!(
        can_start(MrState::Merged, false),
        Err(StartRefusal::NotOpen(MrState::Merged))
    );
    assert_eq!(
        can_start(MrState::Closed, false),
        Err(StartRefusal::NotOpen(MrState::Closed))
    );
    assert_eq!(can_start(MrState::Open, true), Err(StartRefusal::Own));
}

#[test]
fn an_entry_holds_the_fields_of_the_mr_and_keeps_what_the_poll_does_not_read() {
    let mr = parse_pull(&fixture("github/pull-412.v1.json")).unwrap();
    let entry = entry_of(&mr, None);
    assert_eq!(entry.reasons, vec![QueueReason::ReviewRequested]);
    assert_eq!(entry.title, "Auth refactor");
    assert_eq!(entry.head_sha, sha(V1));
    assert_eq!(entry.seen_at, None);

    // The same MR gives the same bytes: a poll with no change writes nothing.
    let bytes = entry_bytes(&entry).unwrap();
    assert_eq!(entry_bytes(&entry_of(&mr, Some(&entry))).unwrap(), bytes);
    assert_eq!(read_entry_bytes(&bytes), Some(entry.clone()));
    assert_eq!(read_entry_bytes(b"{"), None);

    // `seen_at` stays, and the reason is not doubled.
    let seen_at = Timestamp::from_unix_secs(5).unwrap();
    let opened = QueueEntry {
        seen_at: Some(seen_at),
        ..entry
    };
    let kept = entry_of(&mr, Some(&opened));
    assert_eq!(kept.seen_at, Some(seen_at));
    assert_eq!(kept.reasons, vec![QueueReason::ReviewRequested]);
}

#[test]
fn a_gitlab_entry_keeps_the_project_id_and_the_file_name_is_the_mirror_name() {
    let mr = parse_merge_request(&fixture("gitlab/merge-request-88.v2.json")).unwrap();
    let entry = entry_of(&mr, None);
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

#[test]
fn the_queue_commit_says_what_changed() {
    assert_eq!(
        queue_message(1, 2, 3),
        "queue: 1 added, 2 updated, 3 removed"
    );
}
