use crate::{
    Action, Actor, Change, Command, DomainError, Event, EventRecord, FieldChange, IdError,
    Importance, Kind, Link, MachineId, NewTask, Origin, ReadOnlyReason, Status, StatusName, Task,
    TaskEdit, Timestamp, apply, available_actions, create, is_legal_transition, undo,
};
use crate::{ReplayError, hand_edit_changes, replay};

fn at(secs: u64) -> Timestamp {
    Timestamp::from_unix_secs(1_760_000_000 + secs).unwrap()
}

fn origin() -> Origin {
    Origin {
        actor: Actor::User,
        machine_id: MachineId("laptop".to_owned()),
    }
}

fn status(name: StatusName) -> Status {
    match name {
        StatusName::Inbox => Status::Inbox,
        StatusName::Ready => Status::Ready,
        StatusName::Active => Status::Active,
        StatusName::Waiting => Status::Waiting {
            on: Some("Alice".to_owned()),
            until: None,
        },
        StatusName::Done => Status::Done,
        StatusName::Cancelled => Status::Cancelled,
    }
}

fn new_task(kind: Kind) -> NewTask {
    NewTask {
        title: "Login timeout on mobile checkout".to_owned(),
        kind,
        context: "company-a".parse().unwrap(),
        status: Status::Inbox,
        importance: None,
        due: None,
        pinned: false,
        links: Vec::new(),
        relations: crate::Relations::default(),
    }
}

/// A new task and its log.
fn task_with(status: Status) -> (Task, Vec<EventRecord>) {
    let mut new = new_task(Kind::Bug);
    new.status = status;
    create(new, &mut || 42, &[], at(0), &origin()).unwrap()
}

/// Applies `command` and appends its events to `log`.
fn run(task: Task, log: &mut Vec<EventRecord>, command: Command, now: u64) -> Task {
    // Distinct from the IDs drawn by `task_with` (42) and from each other.
    let mut next = u64::try_from(log.len()).unwrap();
    let mut random = || {
        next += 1;
        next
    };
    let (task, events) = apply(task, command, log, at(now), &origin(), &mut random).unwrap();
    log.extend(events);
    task
}

/// Applies the undo of the last change and appends its event to `log`.
fn undo_last(task: Task, log: &mut Vec<EventRecord>, now: u64) -> Task {
    let command = undo(&task, log).unwrap();
    run(task, log, command, now)
}

/// Applies `command` to a task with an empty log.
fn exec(task: Task, command: Command, now: u64) -> Result<(Task, Vec<EventRecord>), DomainError> {
    apply(task, command, &[], at(now), &origin(), &mut || 7)
}

fn set(name: StatusName) -> Command {
    Command::SetStatus {
        status: status(name),
    }
}

#[test]
fn create_draws_the_id_stamps_the_event_and_defaults_the_importance() {
    let (task, log) = create(new_task(Kind::Bug), &mut || 0, &[], at(5), &origin()).unwrap();
    assert_eq!(task.id.as_str(), "00000000");
    assert_eq!(task.importance, Importance::P1);
    assert_eq!((task.created, task.updated), (at(5), at(5)));
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].v, 1);
    assert_eq!(log[0].at, at(5));
    assert_eq!(log[0].machine_id, MachineId("laptop".to_owned()));
    assert_eq!(
        log[0].event,
        Event::Created {
            task: Box::new(task)
        }
    );
}

#[test]
fn create_refuses_an_empty_title_and_unknown_values() {
    let mut empty = new_task(Kind::Bug);
    empty.title = "  ".to_owned();
    assert_eq!(
        create(empty, &mut || 0, &[], at(0), &origin()).unwrap_err(),
        DomainError::EmptyTitle
    );
    let unknown = new_task(Kind::Unknown {
        name: "meeting".to_owned(),
        format: None,
    });
    assert_eq!(
        create(unknown, &mut || 0, &[], at(0), &origin()).unwrap_err(),
        DomainError::UnknownValue(ReadOnlyReason::UnknownKind("meeting".to_owned()))
    );
}

#[test]
fn every_status_pair_is_accepted_or_refused_as_the_table_says() {
    for from in StatusName::ALL {
        for to in StatusName::ALL {
            let (task, _) = task_with(status(from));
            let result = exec(task, set(to), 1);
            if is_legal_transition(from, to) {
                let (task, events) = result.unwrap();
                assert_eq!(task.status, status(to), "{from} -> {to}");
                // waiting -> the same waiting changes nothing.
                assert_eq!(events.len(), usize::from(from != to), "{from} -> {to}");
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    DomainError::IllegalTransition { from, to },
                    "{from} -> {to}"
                );
            }
        }
    }
}

#[test]
fn a_status_change_records_both_statuses_and_updates_the_task() {
    let (task, _) = task_with(Status::Ready);
    let (task, events) = exec(task, set(StatusName::Active), 7).unwrap();
    assert_eq!(task.updated, at(7));
    assert_eq!(
        events[0].event,
        Event::StatusChanged(Change {
            from: Status::Ready,
            to: Status::Active
        })
    );
    let unknown = Command::SetStatus {
        status: Status::Unknown("snoozed".to_owned()),
    };
    assert_eq!(
        exec(task, unknown, 8).unwrap_err(),
        DomainError::UnknownValue(ReadOnlyReason::UnknownStatus("snoozed".to_owned()))
    );
}

#[test]
fn apply_is_deterministic() {
    let command = Command::Edit(TaskEdit {
        title: Some("New title".to_owned()),
        due: Some(Some("2026-10-15".parse().unwrap())),
        ..TaskEdit::default()
    });
    let first = exec(task_with(Status::Ready).0, command.clone(), 3).unwrap();
    let second = exec(task_with(Status::Ready).0, command, 3).unwrap();
    assert_eq!(first, second);
}

#[test]
fn an_edit_records_only_the_fields_that_change() {
    let (task, _) = task_with(Status::Ready);
    let edit = TaskEdit {
        title: Some(task.title.clone()),
        importance: Some(Importance::P0),
        ..TaskEdit::default()
    };
    let (task, events) = exec(task, Command::Edit(edit), 1).unwrap();
    assert_eq!(
        events[0].event,
        Event::Edited {
            changes: vec![FieldChange::Importance(Change {
                from: Importance::P1,
                to: Importance::P0
            })]
        }
    );
    let (same, events) = exec(task.clone(), Command::Pin, 2).unwrap();
    assert_eq!(events.len(), 1);
    let (same, events) = exec(same, Command::Pin, 3).unwrap();
    assert_eq!(events, []);
    assert_eq!(same.updated, at(2));
    let empty = TaskEdit {
        title: Some(String::new()),
        ..TaskEdit::default()
    };
    assert_eq!(
        exec(task.clone(), Command::Edit(empty), 4).unwrap_err(),
        DomainError::EmptyTitle
    );
    let note = Command::LogNote {
        text: " ".to_owned(),
    };
    assert_eq!(exec(task, note, 4).unwrap_err(), DomainError::EmptyNote);
}

#[test]
fn undo_restores_a_status_change_even_when_the_table_forbids_the_reverse() {
    let (task, mut log) = task_with(Status::Active);
    let task = run(task, &mut log, set(StatusName::Done), 1);
    // done -> active is not a legal transition, but reverting active -> done restores active.
    let task = undo_last(task, &mut log, 2);
    assert_eq!(task.status, Status::Active);
    assert_eq!(
        log.last().unwrap().event,
        Event::Undone {
            of: log[1].id.clone(),
            changes: vec![FieldChange::Status(Change {
                from: Status::Done,
                to: Status::Active
            })]
        }
    );
}

#[test]
fn undo_restores_a_field_edit_and_a_pin() {
    let (original, mut log) = task_with(Status::Ready);
    let edit = TaskEdit {
        title: Some("Renamed".to_owned()),
        kind: Some(Kind::Story),
        due: Some(Some("2026-10-15".parse().unwrap())),
        links: Some(vec![Link::Repo {
            repo: "company-a/checkout".to_owned(),
        }]),
        ..TaskEdit::default()
    };
    let task = run(original.clone(), &mut log, Command::Edit(edit), 1);
    let task = undo_last(task, &mut log, 2);
    let restored = Task {
        updated: task.updated,
        ..original
    };
    assert_eq!(task, restored);

    let task = run(task, &mut log, Command::Pin, 3);
    assert!(task.pinned);
    let task = undo_last(task, &mut log, 4);
    assert!(!task.pinned);
}

#[test]
fn undo_of_an_undo_redoes_and_notes_are_skipped() {
    let (task, mut log) = task_with(Status::Ready);
    let task = run(task, &mut log, set(StatusName::Active), 1);
    let task = undo_last(task, &mut log, 2);
    assert_eq!(task.status, Status::Ready);
    let note = Command::LogNote {
        text: "tried again".to_owned(),
    };
    let task = run(task, &mut log, note, 3);
    let redo = undo(&task, &log).unwrap();
    assert!(matches!(&redo, Command::Undo { of, .. } if *of == log[2].id));
    let task = run(task, &mut log, redo, 4);
    assert_eq!(task.status, Status::Active);
}

#[test]
fn undo_with_nothing_to_undo_or_a_stale_value_is_refused() {
    let (task, mut log) = task_with(Status::Ready);
    assert_eq!(undo(&task, &log).unwrap_err(), DomainError::NothingToUndo);
    let task = run(task, &mut log, set(StatusName::Active), 1);
    let stale = undo(&task, &log).unwrap();
    let task = run(task, &mut log, set(StatusName::Done), 2);
    assert_eq!(
        exec(task, stale, 3).unwrap_err(),
        DomainError::Conflict("status")
    );
}

#[test]
fn a_task_of_unknown_kind_accepts_no_command_and_reports_why() {
    let (mut task, log) = task_with(Status::Ready);
    task.kind = Kind::Unknown {
        name: "meeting".to_owned(),
        format: None,
    };
    let reason = ReadOnlyReason::UnknownKind("meeting".to_owned());
    assert_eq!(available_actions(&task), []);
    let commands = [
        set(StatusName::Active),
        Command::Edit(TaskEdit::default()),
        Command::Pin,
        Command::Unpin,
        Command::LogNote {
            text: "note".to_owned(),
        },
    ];
    for command in commands {
        assert_eq!(
            exec(task.clone(), command, 1).unwrap_err(),
            DomainError::ReadOnly(reason.clone())
        );
    }
    assert_eq!(
        undo(&task, &log).unwrap_err(),
        DomainError::ReadOnly(reason)
    );
}

#[test]
fn available_actions_follow_the_status_and_the_pin() {
    let (task, _) = task_with(Status::Done);
    assert_eq!(
        available_actions(&task),
        [
            Action::SetStatus(StatusName::Ready),
            Action::Edit,
            Action::Pin,
            Action::LogNote
        ]
    );
    let (mut task, _) = task_with(Status::Ready);
    task.pinned = true;
    let actions = available_actions(&task);
    assert!(actions.contains(&Action::Unpin));
    assert!(!actions.contains(&Action::SetStatus(StatusName::Ready)));
    assert_eq!(actions.len(), 5 + 3);
    task.status = Status::Unknown("snoozed".to_owned());
    assert_eq!(available_actions(&task), []);
}

#[test]
fn events_serialize_with_version_actor_and_machine() {
    let (task, mut log) = task_with(Status::Ready);
    run(task, &mut log, set(StatusName::Active), 1);
    let value = serde_json::to_value(&log[1]).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "v": 1,
            "id": "00000002",
            "at": "2025-10-09T08:53:21Z",
            "actor": { "type": "user" },
            "machine_id": "laptop",
            "type": "status_changed",
            "from": { "status": "ready" },
            "to": { "status": "active" }
        })
    );
    assert_eq!(
        serde_json::from_value::<EventRecord>(value).unwrap(),
        log[1]
    );
    let created = serde_json::to_value(&log[0]).unwrap();
    assert_eq!(
        serde_json::from_value::<EventRecord>(created).unwrap(),
        log[0]
    );
}

#[test]
fn boundary_types_have_json_schemas() {
    for schema in [
        schemars::schema_for!(Task),
        schemars::schema_for!(EventRecord),
        schemars::schema_for!(Command),
        schemars::schema_for!(Action),
        schemars::schema_for!(NewTask),
    ] {
        assert!(schema.as_object().is_some());
    }
}

#[test]
fn event_ids_are_unique_within_the_log() {
    let (task, log) = task_with(Status::Ready);
    // A source that first repeats the ID of the creation event, then gives a new one.
    let mut draws = [42, 42, 9].into_iter();
    let (_, events) = apply(
        task.clone(),
        Command::Pin,
        &log,
        at(1),
        &origin(),
        &mut || draws.next().unwrap(),
    )
    .unwrap();
    assert_ne!(events[0].id, log[0].id);
    assert_eq!(events[0].id.as_str(), "00000009");
    let always_taken = apply(task, Command::Pin, &log, at(1), &origin(), &mut || 42);
    assert_eq!(
        always_taken.unwrap_err(),
        DomainError::Id(IdError::Exhausted)
    );
}

#[test]
fn an_undo_command_cannot_be_read_from_input() {
    let (task, mut log) = task_with(Status::Ready);
    let task = run(task, &mut log, Command::Pin, 1);
    let command = undo(&task, &log).unwrap();
    let json = serde_json::to_value(&command).unwrap();
    assert_eq!(json["command"], "undo");
    assert!(serde_json::from_value::<Command>(json).is_err());
    let pin = serde_json::json!({ "command": "pin" });
    assert_eq!(
        serde_json::from_value::<Command>(pin).unwrap(),
        Command::Pin
    );
}

#[test]
fn replaying_the_log_gives_the_task_it_describes() {
    let (task, mut log) = task_with(Status::Inbox);
    let task = run(task, &mut log, set(StatusName::Active), 1);
    let edit = TaskEdit {
        title: Some("Renamed".to_owned()),
        due: Some(Some("2026-10-15".parse().unwrap())),
        ..TaskEdit::default()
    };
    let task = run(task, &mut log, Command::Edit(edit), 2);
    let task = run(task, &mut log, Command::Pin, 3);
    let task = undo_last(task, &mut log, 4);
    let note = Command::LogNote {
        text: "called".to_owned(),
    };
    let task = run(task, &mut log, note, 5);
    assert_eq!(replay(&log), Ok(task.clone()));
    assert_eq!(task.updated, at(5));
    // The creation alone.
    assert_eq!(replay(&log[..1]).unwrap().updated, at(0));
}

#[test]
fn a_log_without_its_creation_or_with_a_stale_change_does_not_replay() {
    let (task, mut log) = task_with(Status::Ready);
    run(task, &mut log, set(StatusName::Active), 1);
    assert_eq!(replay(&[]), Err(ReplayError::NoCreation));
    assert_eq!(replay(&log[1..]), Err(ReplayError::NoCreation));
    // The same change twice: the second expects `ready`, finds `active`.
    let doubled = [log[0].clone(), log[1].clone(), log[1].clone()];
    assert_eq!(replay(&doubled), Err(ReplayError::Conflict("status")));
    // A second creation is not a change: the log is not one task's.
    let twice = [log[0].clone(), log[0].clone()];
    assert_eq!(replay(&twice), Err(ReplayError::CreatedTwice));
}

#[test]
fn a_hand_edit_is_the_changes_of_every_field_that_differs() {
    let (before, _) = task_with(Status::Ready);
    assert_eq!(hand_edit_changes(&before, &before), []);
    let after = Task {
        title: "By hand".to_owned(),
        status: Status::Unknown("snoozed".to_owned()),
        pinned: true,
        links: vec![Link::Repo {
            repo: "company-a/checkout".to_owned(),
        }],
        // Not a field a hand edit records.
        updated: at(99),
        ..before.clone()
    };
    assert_eq!(
        hand_edit_changes(&before, &after),
        [
            FieldChange::Title(Change {
                from: before.title.clone(),
                to: "By hand".to_owned(),
            }),
            FieldChange::Status(Change {
                from: Status::Ready,
                to: Status::Unknown("snoozed".to_owned()),
            }),
            FieldChange::Pinned(Change {
                from: false,
                to: true,
            }),
            FieldChange::Links(Change {
                from: Vec::new(),
                to: after.links.clone(),
            }),
        ]
    );
    // Recorded, then replayed: the log describes the file again.
    let (task, mut log) = task_with(Status::Ready);
    let edited = Task {
        title: "By hand".to_owned(),
        importance: Importance::P3,
        ..task.clone()
    };
    let file = Origin {
        actor: Actor::File,
        machine_id: MachineId("laptop".to_owned()),
    };
    log.push(EventRecord::new(
        "zzzzzzzz".parse().unwrap(),
        Event::Edited {
            changes: hand_edit_changes(&task, &edited),
        },
        at(7),
        &file,
    ));
    assert_eq!(
        replay(&log),
        Ok(Task {
            updated: at(7),
            ..edited
        })
    );
    assert_eq!(
        serde_json::to_value(&log[1].actor).unwrap(),
        serde_json::json!({ "type": "file" })
    );
}
