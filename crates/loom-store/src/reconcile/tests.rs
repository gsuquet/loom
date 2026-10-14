use loom_core::{
    Actor, Change, ContextName, EventLog, EventRecord, FieldChange, Kind, MachineId, NewTask,
    Origin, Relations, SkipReason, SkippedLine, Status, Task, TaskDocument, Timestamp, create,
    parse_task_md, render_task_md,
};

use super::{Reconciliation, decide};
use crate::port::{ContentHash, StoredTask, TaskFile};

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
    assert_eq!(decide(Some(&hash), &stored), Reconciliation::Unchanged);
}

#[test]
fn a_changed_field_the_log_does_not_explain_is_a_hand_edit() {
    let (task, events) = new_task();
    let text = retitled(&task, "After");
    let stored = stored(&task, &text, events, Vec::new());
    let stale = ContentHash::of(b"what loomd wrote");
    let expected = Reconciliation::Record(vec![FieldChange::Title(Change {
        from: "Before".to_owned(),
        to: "After".to_owned(),
    })]);
    assert_eq!(decide(Some(&stale), &stored), expected);
    // Not indexed yet (a deleted index): the log still tells the edit apart.
    assert_eq!(decide(None, &stored), expected);
}

#[test]
fn a_file_the_log_explains_is_indexed_without_an_event() {
    let (task, events) = new_task();
    // Only the notes changed: no field to record.
    let text = render_task_md(&TaskDocument::new(&task)) + "more notes\n";
    let stored = stored(&task, &text, events, Vec::new());
    assert_eq!(decide(None, &stored), Reconciliation::Index);
}

#[test]
fn an_invalid_file_or_an_unreadable_log_is_indexed_without_an_event() {
    let (task, events) = new_task();
    let broken = stored(&task, "+++\ntitle = \n+++\n", events.clone(), Vec::new());
    assert_eq!(decide(None, &broken), Reconciliation::Index);

    let text = retitled(&task, "After");
    // A line written by a newer loom: the log cannot be replayed with certainty.
    let skipped = vec![SkippedLine {
        line: 2,
        raw: "{}".to_owned(),
        reason: SkipReason::NewerVersion { v: 9 },
    }];
    let newer = stored(&task, &text, events, skipped);
    assert_eq!(decide(None, &newer), Reconciliation::Index);
    // No creation event: nothing to compare with.
    let no_log = stored(&task, &text, Vec::new(), Vec::new());
    assert_eq!(decide(None, &no_log), Reconciliation::Index);
}
