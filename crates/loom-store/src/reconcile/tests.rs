use loom_core::{
    Actor, Change, ContextName, Event, EventLog, EventRecord, FieldChange, Kind, MachineId,
    NewTask, NotesHash, Origin, Relations, SkipReason, SkippedLine, Status, Task, TaskDocument,
    Timestamp, create, parse_task_md, render_task_md,
};

use super::{HandEdit, Reconciliation, decide, duplicate_ids, notes_digest};
use crate::port::{ContentHash, StoredTask, TaskFile, TaskLocation};

fn new_task() -> (Task, Vec<EventRecord>) {
    let mut bits = 11_u64;
    let mut random = move || {
        bits = bits.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        bits
    };
    create(
        NewTask {
            title: "Before".to_owned(),
            kind: Kind::Chore,
            context: "work".parse::<ContextName>().unwrap(),
            status: Status::Ready,
            importance: None,
            due: None,
            pinned: false,
            links: Vec::new(),
            relations: Relations::default(),
        },
        &mut random,
        &[],
        Timestamp::from_unix_secs(100).unwrap(),
        &Origin {
            actor: Actor::User,
            machine_id: MachineId("laptop".to_owned()),
        },
    )
    .unwrap()
}

fn stored(
    task: &Task,
    text: &str,
    events: Vec<EventRecord>,
    skipped: Vec<SkippedLine>,
) -> StoredTask {
    StoredTask {
        id: task.id.clone(),
        context: task.context.clone(),
        location: TaskLocation::Active,
        file: TaskFile::parse(text.as_bytes().to_vec(), &task.id, &task.context),
        events: EventLog { events, skipped },
    }
}

/// The file of `task` with its title set to `title`, as a hand edit would leave it.
fn retitled(task: &Task, title: &str) -> String {
    let text = render_task_md(&TaskDocument::new(task));
    let mut document = parse_task_md(&text).unwrap();
    document
        .set_task(&Task {
            title: title.to_owned(),
            ..task.clone()
        })
        .unwrap();
    render_task_md(&document)
}

#[test]
fn the_file_loomd_last_indexed_is_unchanged() {
    let (task, events) = new_task();
    let text = retitled(&task, "After");
    let stored = stored(&task, &text, events, Vec::new());
    let hash = ContentHash::of(text.as_bytes());
    assert_eq!(
        decide(Some((&hash, TaskLocation::Active)), None, &stored),
        Reconciliation::Unchanged
    );
    // The same file moved to the archive, or back, is indexed again: its location changed.
    assert_eq!(
        decide(Some((&hash, TaskLocation::Archived(2026))), None, &stored),
        Reconciliation::Index
    );
    let archived = StoredTask {
        location: TaskLocation::Archived(2026),
        ..stored
    };
    assert_eq!(
        decide(Some((&hash, TaskLocation::Archived(2026))), None, &archived),
        Reconciliation::Unchanged
    );
    assert_eq!(
        decide(Some((&hash, TaskLocation::Archived(2025))), None, &archived),
        Reconciliation::Index
    );
}

#[test]
fn a_changed_field_the_log_does_not_explain_is_a_hand_edit() {
    let (task, events) = new_task();
    let text = retitled(&task, "After");
    let stored = stored(&task, &text, events, Vec::new());
    let stale = ContentHash::of(b"what loomd wrote");
    let expected = Reconciliation::Record(HandEdit {
        changes: vec![FieldChange::Title(Change {
            from: "Before".to_owned(),
            to: "After".to_owned(),
        })],
        notes: None,
    });
    assert_eq!(
        decide(Some((&stale, TaskLocation::Active)), None, &stored),
        expected
    );
    // Not indexed yet (a deleted index): the log still tells the edit apart.
    assert_eq!(decide(None, None, &stored), expected);
}

#[test]
fn a_file_the_log_explains_is_indexed_without_an_event() {
    let (task, events) = new_task();
    // Only the notes changed: no field to record.
    let text = render_task_md(&TaskDocument::new(&task)) + "more notes\n";
    let stored = stored(&task, &text, events, Vec::new());
    assert_eq!(decide(None, None, &stored), Reconciliation::Index);
}

#[test]
fn an_invalid_file_or_an_unreadable_log_is_indexed_without_an_event() {
    let (task, events) = new_task();
    let broken = stored(&task, "+++\ntitle = \n+++\n", events.clone(), Vec::new());
    assert_eq!(decide(None, None, &broken), Reconciliation::Index);

    let text = retitled(&task, "After");
    // A line written by a newer loom: the log cannot be replayed with certainty.
    let skipped = vec![SkippedLine {
        line: 2,
        raw: "{}".to_owned(),
        reason: SkipReason::NewerVersion { v: 9 },
    }];
    let newer = stored(&task, &text, events, skipped);
    assert_eq!(decide(None, None, &newer), Reconciliation::Withhold);
    // No creation event: nothing to compare with.
    let no_log = stored(&task, &text, Vec::new(), Vec::new());
    assert_eq!(decide(None, None, &no_log), Reconciliation::Index);
}

#[test]
fn a_damaged_or_unknown_line_does_not_stop_the_recording_of_a_hand_edit() {
    let (task, events) = new_task();
    let text = retitled(&task, "After");
    let expected = Reconciliation::Record(HandEdit {
        changes: vec![FieldChange::Title(Change {
            from: "Before".to_owned(),
            to: "After".to_owned(),
        })],
        notes: None,
    });
    // A line cut by a crash in the middle of an append, and a line of a type this version does
    // not know: the events that were read still describe the task.
    for reason in [
        SkipReason::Invalid {
            message: "EOF while parsing".to_owned(),
        },
        SkipReason::UnknownType {
            name: "teleported".to_owned(),
        },
    ] {
        let skipped = vec![SkippedLine {
            line: 2,
            raw: "{\"garbage\"".to_owned(),
            reason,
        }];
        let stored = stored(&task, &text, events.clone(), skipped);
        assert_eq!(decide(None, None, &stored), expected);
    }
}

#[test]
fn an_id_held_by_two_contexts_is_a_duplicate() {
    let id = |text: &str| text.parse::<loom_core::TaskId>().unwrap();
    let context = |text: &str| text.parse::<ContextName>().unwrap();
    let (a, b, c) = (context("a"), context("b"), context("c"));
    let (one, two, three) = (id("k3q7zm2a"), id("h8w2n4pt"), id("m5x9d2rv"));
    let held = [
        (&a, &one),
        (&a, &two),
        (&b, &one),
        (&c, &three),
        (&c, &one),
        (&b, &two),
    ];
    let duplicates = duplicate_ids(held);
    assert_eq!(duplicates.len(), 2);
    // Contexts in the order they were given, each once.
    assert_eq!(duplicates[&one], [a.clone(), b.clone(), c]);
    assert_eq!(duplicates[&two], [a, b]);
    assert!(!duplicates.contains_key(&three));
    assert!(duplicate_ids([]).is_empty());
}

/// The notes of the task file `text`.
fn notes_of(text: &str) -> String {
    parse_task_md(text).unwrap().body().to_owned()
}

/// The hash of `notes` as the ledger records it.
fn notes_hash(notes: &str) -> NotesHash {
    NotesHash::from_hex(notes_digest(notes).as_str().to_owned()).unwrap()
}

#[test]
fn a_notes_only_hand_edit_is_recorded_when_the_previous_notes_are_known() {
    let (task, events) = new_task();
    let text = render_task_md(&TaskDocument::new(&task)) + "more notes\n";
    let stored = stored(&task, &text, events, Vec::new());
    let before = ContentHash::of(b"");
    let stale = ContentHash::of(b"what loomd wrote");
    let expected = Reconciliation::Record(HandEdit {
        changes: Vec::new(),
        notes: Some(Change {
            from: notes_hash(""),
            to: notes_hash(&notes_of(&text)),
        }),
    });
    assert_eq!(
        decide(Some((&stale, TaskLocation::Active)), Some(&before), &stored),
        expected
    );
    // The same notes as indexed: only the file differs in what the ledger does not record.
    let same = notes_digest(&notes_of(&text));
    assert_eq!(
        decide(Some((&stale, TaskLocation::Active)), Some(&same), &stored),
        Reconciliation::Index
    );
    // Without an index row, the previous notes are unknown: no event rather than a guess.
    assert_eq!(decide(None, None, &stored), Reconciliation::Index);
}

#[test]
fn a_hand_edit_of_a_field_and_the_notes_records_both() {
    let (task, events) = new_task();
    let text = retitled(&task, "After") + "more notes\n";
    let stored = stored(&task, &text, events, Vec::new());
    let before = ContentHash::of(b"");
    let stale = ContentHash::of(b"what loomd wrote");
    let Reconciliation::Record(edit) =
        decide(Some((&stale, TaskLocation::Active)), Some(&before), &stored)
    else {
        panic!("a hand edit is recorded");
    };
    assert_eq!(edit.changes.len(), 1);
    assert!(edit.notes.is_some());
    let events = edit.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], Event::Edited { .. }));
    assert!(matches!(events[1], Event::NotesChanged(_)));
}

#[test]
fn the_notes_the_log_last_recorded_are_the_previous_notes_without_an_index_row() {
    let (task, mut events) = new_task();
    let text_first = render_task_md(&TaskDocument::new(&task)) + "first notes\n";
    let first = notes_of(&text_first);
    events.push(EventRecord::new(
        "n0000001".parse().unwrap(),
        Event::NotesChanged(Change {
            from: notes_hash(""),
            to: notes_hash(&first),
        }),
        Timestamp::from_unix_secs(200).unwrap(),
        &Origin {
            actor: Actor::File,
            machine_id: MachineId("laptop".to_owned()),
        },
    ));
    let text = render_task_md(&TaskDocument::new(&task)) + "second notes\n";
    let changed = stored(&task, &text, events.clone(), Vec::new());
    assert_eq!(
        decide(None, None, &changed),
        Reconciliation::Record(HandEdit {
            changes: Vec::new(),
            notes: Some(Change {
                from: notes_hash(&first),
                to: notes_hash(&notes_of(&text)),
            }),
        })
    );
    // The notes the log recorded last: nothing new.
    let same = stored(&task, &text_first, events, Vec::new());
    assert_eq!(decide(None, None, &same), Reconciliation::Index);
}

#[test]
fn line_endings_and_blank_lines_at_the_end_of_the_notes_are_no_edit() {
    let (task, events) = new_task();
    let base = render_task_md(&TaskDocument::new(&task)) + "same words\n";
    let before = notes_digest(&notes_of(&base));
    let stale = ContentHash::of(b"what loomd wrote");
    for variant in [
        base.replace('\n', "\r\n"),
        base.clone() + "\n\n",
        base.trim_end().to_owned(),
    ] {
        let stored = stored(&task, &variant, events.clone(), Vec::new());
        assert_eq!(
            decide(Some((&stale, TaskLocation::Active)), Some(&before), &stored),
            Reconciliation::Index,
            "{variant:?}"
        );
    }
    // A change of the words is still an edit.
    let changed = stored(&task, &(base + "more\n"), events, Vec::new());
    assert!(matches!(
        decide(
            Some((&stale, TaskLocation::Active)),
            Some(&before),
            &changed
        ),
        Reconciliation::Record(_)
    ));
}
