use std::time::Duration;

use crate::{
    Actor, Command, EventRecord, Kind, MachineId, NewTask, Origin, Relations, Status, Task,
    Timestamp, apply, archive_year, closed_at, create, undo,
};

const DAY: u64 = 86_400;
const WEEK: Duration = Duration::from_secs(7 * DAY);
/// 2026-10-12T08:00:00Z.
const START: u64 = 1_791_792_000;

fn at(secs: u64) -> Timestamp {
    Timestamp::from_unix_secs(START + secs).unwrap()
}

fn origin() -> Origin {
    Origin {
        actor: Actor::User,
        machine_id: MachineId("laptop".to_owned()),
    }
}

/// A task created at `START` with `status`, and its log.
fn created(status: Status) -> (Task, Vec<EventRecord>) {
    let new = NewTask {
        title: "Rotate the staging certificates".to_owned(),
        kind: Kind::Chore,
        context: "company-a".parse().unwrap(),
        status,
        importance: None,
        due: None,
        pinned: false,
        links: Vec::new(),
        relations: Relations::default(),
    };
    create(new, &mut || 42, &[], at(0), &origin()).unwrap()
}

/// Applies `command` at `secs` and appends its events to `log`.
fn run(task: Task, log: &mut Vec<EventRecord>, command: Command, secs: u64) -> Task {
    let mut next = u64::try_from(log.len()).unwrap() + 100;
    let mut random = || {
        next += 1;
        next
    };
    let (task, events) = apply(task, command, log, at(secs), &origin(), &mut random).unwrap();
    log.extend(events);
    task
}

fn set(status: Status) -> Command {
    Command::SetStatus { status }
}

/// A task created at `START`, ready, then done at `START + done`.
fn done_at(done: u64) -> (Task, Vec<EventRecord>) {
    let (task, mut log) = created(Status::Ready);
    let task = run(task, &mut log, set(Status::Done), done);
    (task, log)
}

#[test]
fn a_task_done_8_days_ago_is_archived_6_days_ago_is_not() {
    let (task, log) = done_at(0);
    assert_eq!(archive_year(&task, &log, at(8 * DAY), WEEK), Some(2026));
    assert_eq!(archive_year(&task, &log, at(6 * DAY), WEEK), None);
}

#[test]
fn a_task_is_archived_once_the_whole_delay_has_passed() {
    let (task, log) = done_at(0);
    assert_eq!(archive_year(&task, &log, at(7 * DAY - 1), WEEK), None);
    assert_eq!(archive_year(&task, &log, at(7 * DAY), WEEK), Some(2026));
}

#[test]
fn a_cancelled_task_is_archived_as_a_done_one() {
    let (task, mut log) = created(Status::Inbox);
    let task = run(task, &mut log, set(Status::Cancelled), DAY);
    assert_eq!(closed_at(&task, &log), Some(at(DAY)));
    assert_eq!(archive_year(&task, &log, at(8 * DAY), WEEK), Some(2026));
}

#[test]
fn open_tasks_are_never_archived() {
    for status in [
        Status::Inbox,
        Status::Ready,
        Status::Active,
        Status::Waiting {
            on: None,
            until: None,
        },
        Status::Unknown("blocked".to_owned()),
    ] {
        let (mut task, log) = created(Status::Inbox);
        task.status = status.clone();
        assert_eq!(closed_at(&task, &log), None, "{status:?}");
        assert_eq!(
            archive_year(&task, &log, at(1000 * DAY), Duration::ZERO),
            None
        );
    }
}

#[test]
fn the_archive_year_is_the_year_the_task_was_closed() {
    // Done on 2026-12-31T23:00:00Z, archived in 2027.
    let new_year_eve = 1_798_758_000 - START;
    let (task, log) = done_at(new_year_eve);
    assert_eq!(
        archive_year(&task, &log, at(new_year_eve + 8 * DAY), WEEK),
        Some(2026)
    );
}

#[test]
fn the_delay_runs_from_the_last_closing_not_the_last_change() {
    let (task, mut log) = done_at(0);
    // Reopened, then done again two days later.
    let task = run(task, &mut log, set(Status::Ready), DAY);
    let task = run(task, &mut log, set(Status::Done), 2 * DAY);
    // A note after the closing does not delay the archive.
    let task = run(
        task,
        &mut log,
        Command::LogNote {
            text: "deployed".to_owned(),
        },
        3 * DAY,
    );
    assert_eq!(closed_at(&task, &log), Some(at(2 * DAY)));
    assert_eq!(archive_year(&task, &log, at(8 * DAY), WEEK), None);
    assert_eq!(archive_year(&task, &log, at(9 * DAY), WEEK), Some(2026));
}

#[test]
fn an_undo_that_closes_the_task_again_closes_it_at_the_undo() {
    let (task, mut log) = done_at(0);
    let task = run(task, &mut log, set(Status::Ready), DAY);
    let command = undo(&task, &log).unwrap();
    let task = run(task, &mut log, command, 2 * DAY);
    assert_eq!(task.status, Status::Done);
    assert_eq!(closed_at(&task, &log), Some(at(2 * DAY)));
}

#[test]
fn a_task_created_closed_is_closed_at_its_creation() {
    let (task, log) = created(Status::Done);
    assert_eq!(closed_at(&task, &log), Some(at(0)));
}

#[test]
fn without_a_closing_event_a_task_is_never_archived() {
    // A status set by hand and not recorded yet, or a lost log.
    let (mut task, log) = created(Status::Ready);
    task.status = Status::Done;
    assert_eq!(closed_at(&task, &log), None);
    assert_eq!(closed_at(&task, &[]), None);
    assert_eq!(
        archive_year(&task, &[], at(1000 * DAY), Duration::ZERO),
        None
    );
}

#[test]
fn a_notes_edit_after_the_closing_neither_delays_nor_moves_the_archive() {
    let (task, mut log) = done_at(0);
    let hash = |c: char| crate::NotesHash::from_hex(c.to_string().repeat(64)).unwrap();
    log.push(EventRecord::new(
        "n0000001".parse().unwrap(),
        crate::Event::NotesChanged(crate::Change {
            from: hash('a'),
            to: hash('b'),
        }),
        at(3 * DAY),
        &Origin {
            actor: Actor::File,
            machine_id: MachineId("laptop".to_owned()),
        },
    ));
    assert_eq!(closed_at(&task, &log), Some(at(0)));
    assert_eq!(archive_year(&task, &log, at(8 * DAY), WEEK), Some(2026));
    assert_eq!(archive_year(&task, &log, at(6 * DAY), WEEK), None);
}
