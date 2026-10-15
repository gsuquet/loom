use jiff::ToSpan as _;
use loom_api::{Importance, Kind, Relations, Task, Timestamp as TaskTimestamp};
use loom_config::config::parse_config;
use loom_config::machine::MachineName;
use loom_store::port::{ContentHash, TaskLocation};
use std::path::Path;

use super::*;

fn date(text: &str) -> Date {
    text.parse().unwrap()
}

fn context() -> ContextName {
    "company-a".parse().unwrap()
}

/// The schedule `id` with `rrule` and `policy`, due the same day.
fn schedule(rrule: &str, policy: &str) -> Schedule {
    let text = format!(
        "[contexts.company-a]\n[[contexts.company-a.schedules]]\nid = \"timesheet\"\ntitle = \"Fill in the timesheet\"\nkind = \"admin\"\nrrule = \"{rrule}\"\npolicy = \"{policy}\"\ndue = \"same-day\"\n"
    );
    let config = parse_config(&text, Path::new("config.toml")).unwrap();
    config.contexts[&context()].schedules[0].clone()
}

/// A task of `context` created for occurrence `occurrence` of schedule `schedule`.
fn row(id: &str, context: &str, schedule: &str, occurrence: &str, status: Status) -> IndexedTask {
    let task = Task {
        id: id.parse().unwrap(),
        title: "Fill in the timesheet".to_owned(),
        kind: Kind::Admin,
        context: context.parse().unwrap(),
        status,
        importance: Importance::P2,
        due: None,
        pinned: false,
        links: vec![Link::Origin(ExternalRef {
            provider: SCHEDULE_PROVIDER.to_owned(),
            key: format!("{schedule}/{occurrence}"),
            url: None,
        })],
        relations: Relations::default(),
        created: TaskTimestamp::EPOCH,
        updated: TaskTimestamp::EPOCH,
    };
    IndexedTask {
        id: task.id.clone(),
        context: task.context.clone(),
        hash: ContentHash::of(b""),
        location: TaskLocation::Active,
        state: IndexedState::Valid {
            task: Box::new(task),
            body: String::new(),
        },
    }
}

fn created(outcome: &Outcome) -> Option<(Date, &[TaskId])> {
    match outcome {
        Outcome::Create {
            occurrence, cancel, ..
        } => Some((*occurrence, cancel)),
        Outcome::NotDue | Outcome::Skipped { .. } => None,
    }
}

#[test]
fn a_due_occurrence_creates_a_ready_task_with_its_dedup_key() {
    let fridays = schedule("FREQ=WEEKLY;BYDAY=FR", "skip");
    let outcome = plan(&context(), &fridays, &[], date("2026-10-16"));
    let Outcome::Create {
        occurrence,
        new,
        cancel,
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(occurrence, date("2026-10-16"));
    assert_eq!(cancel, Vec::<TaskId>::new());
    assert_eq!(
        *new,
        NewTask {
            title: "Fill in the timesheet".to_owned(),
            kind: Kind::Admin,
            context: context(),
            status: Status::Ready,
            importance: None,
            due: Some(date("2026-10-16")),
            pinned: false,
            links: vec![Link::Origin(ExternalRef {
                provider: "schedule".to_owned(),
                key: "timesheet/2026-10-16".to_owned(),
                url: None,
            })],
            relations: Relations::default(),
        }
    );
}

#[test]
fn an_occurrence_with_a_task_is_never_created_again() {
    let fridays = schedule("FREQ=WEEKLY;BYDAY=FR", "stack");
    let rows = [row(
        "0000000a",
        "company-a",
        "timesheet",
        "2026-10-16",
        Status::Done,
    )];
    for today in ["2026-10-16", "2026-10-17", "2026-10-22"] {
        assert_eq!(
            plan(&context(), &fridays, &rows, date(today)),
            Outcome::NotDue
        );
    }
    // The tasks of another schedule, or of another context, are not this schedule's.
    let others = [
        row(
            "0000000b",
            "company-a",
            "standup",
            "2026-10-16",
            Status::Done,
        ),
        row(
            "0000000c",
            "personal",
            "timesheet",
            "2026-10-16",
            Status::Done,
        ),
    ];
    assert_eq!(
        created(&plan(&context(), &fridays, &others, date("2026-10-16"))),
        Some((date("2026-10-16"), &[][..]))
    );
}

#[test]
fn a_week_offline_creates_one_task() {
    let daily = schedule("FREQ=DAILY", "stack");
    let rows = [row(
        "0000000a",
        "company-a",
        "timesheet",
        "2026-10-05",
        Status::Done,
    )];
    assert_eq!(
        created(&plan(&context(), &daily, &rows, date("2026-10-12"))),
        Some((date("2026-10-12"), &[][..]))
    );
}

#[test]
fn policies_with_the_previous_occurrence_open() {
    let open = row(
        "0000000a",
        "company-a",
        "timesheet",
        "2026-10-09",
        Status::Ready,
    );
    let today = date("2026-10-16");
    let skip = schedule("FREQ=WEEKLY;BYDAY=FR", "skip");
    assert_eq!(
        plan(&context(), &skip, std::slice::from_ref(&open), today),
        Outcome::Skipped {
            occurrence: today,
            open: vec![open.id.clone()],
        }
    );
    let stack = schedule("FREQ=WEEKLY;BYDAY=FR", "stack");
    assert_eq!(
        created(&plan(
            &context(),
            &stack,
            std::slice::from_ref(&open),
            today
        )),
        Some((today, &[][..]))
    );
    let replace = schedule("FREQ=WEEKLY;BYDAY=FR", "replace");
    assert_eq!(
        created(&plan(
            &context(),
            &replace,
            std::slice::from_ref(&open),
            today
        )),
        Some((today, std::slice::from_ref(&open.id)))
    );
    // Done and cancelled tasks are not open; a status of a newer loom is, but read-only.
    let closed = [
        row(
            "0000000b",
            "company-a",
            "timesheet",
            "2026-10-02",
            Status::Done,
        ),
        row(
            "0000000c",
            "company-a",
            "timesheet",
            "2026-09-25",
            Status::Cancelled,
        ),
    ];
    assert_eq!(
        created(&plan(&context(), &skip, &closed, today)),
        Some((today, &[][..]))
    );
    let newer = row(
        "0000000d",
        "company-a",
        "timesheet",
        "2026-10-09",
        Status::Unknown("blocked".to_owned()),
    );
    assert!(matches!(
        plan(&context(), &skip, std::slice::from_ref(&newer), today),
        Outcome::Skipped { .. }
    ));
    assert_eq!(
        created(&plan(&context(), &replace, &[newer], today)),
        Some((today, &[][..]))
    );
    // Reopened by hand in the archive: not open, never cancelled, its occurrence kept.
    let archived = IndexedTask {
        location: TaskLocation::Archived(2026),
        ..open
    };
    assert_eq!(
        created(&plan(
            &context(),
            &skip,
            std::slice::from_ref(&archived),
            today
        )),
        Some((today, &[][..]))
    );
    let same_day = IndexedTask {
        location: TaskLocation::Archived(2026),
        ..row(
            "0000000e",
            "company-a",
            "timesheet",
            "2026-10-16",
            Status::Done,
        )
    };
    assert_eq!(plan(&context(), &skip, &[same_day], today), Outcome::NotDue);
}

/// The occurrences a daily schedule creates when checked every 15 minutes from `from` to `to`
/// (UTC) in `tz`, each creation recorded as a task.
fn created_every_quarter(tz: &TimeZone, from: &str, to: &str) -> Vec<Date> {
    let daily = schedule("FREQ=DAILY", "stack");
    let (mut now, to): (Timestamp, Timestamp) = (from.parse().unwrap(), to.parse().unwrap());
    let mut rows = Vec::new();
    let mut dates = Vec::new();
    while now <= to {
        if let Outcome::Create { occurrence, .. } =
            plan(&context(), &daily, &rows, local_date(now, tz))
        {
            let id = format!("{:08}", rows.len());
            rows.push(row(
                &id,
                "company-a",
                "timesheet",
                &occurrence.to_string(),
                Status::Done,
            ));
            dates.push(occurrence);
        }
        now = now.checked_add(15.minutes()).unwrap();
    }
    dates
}

#[test]
fn daylight_saving_changes_in_paris_neither_shift_nor_repeat_occurrences() {
    // Europe/Paris rules, without the system time zone database.
    let paris = TimeZone::posix("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
    // Spring forward on 2026-03-29 at 02:00, fall back on 2026-10-25 at 03:00.
    for (from, to, first, last) in [
        (
            "2026-03-27T22:00:00Z",
            "2026-03-31T21:59:00Z",
            "2026-03-27",
            "2026-03-31",
        ),
        (
            "2026-10-23T21:00:00Z",
            "2026-10-27T22:59:00Z",
            "2026-10-23",
            "2026-10-27",
        ),
    ] {
        let dates = created_every_quarter(&paris, from, to);
        let mut expected = Vec::new();
        let mut day = date(first);
        while day <= date(last) {
            expected.push(day);
            day = day.tomorrow().unwrap();
        }
        assert_eq!(dates, expected, "{from}");
    }
    // Local midnight, not UTC midnight, starts the day.
    assert_eq!(
        local_date("2026-10-24T22:30:00Z".parse().unwrap(), &paris),
        date("2026-10-25")
    );
    assert_eq!(
        local_date("2026-10-25T22:30:00Z".parse().unwrap(), &paris),
        date("2026-10-25")
    );
}

/// A task of `context` whose `task.md` is invalid, with `text`.
fn invalid(id: &str, text: &str) -> IndexedTask {
    IndexedTask {
        id: id.parse().unwrap(),
        context: context(),
        hash: ContentHash::of(text.as_bytes()),
        location: TaskLocation::Active,
        state: IndexedState::Invalid {
            error: "line 2: invalid TOML".to_owned(),
            text: text.to_owned(),
        },
    }
}

#[test]
fn an_invalid_task_file_keeps_its_occurrence_and_counts_as_open() {
    // A hand edit broke the task of today's occurrence: it is not created again.
    let broken = invalid(
        "0000000e",
        "+++\ntitle = \"Fill in\nkind = \"admin\"\n[[links]]\nrole = \"origin\"\nprovider = \"schedule\"\nkey = \"timesheet/2026-10-16\"\n+++\n",
    );
    let done = row(
        "0000000a",
        "company-a",
        "timesheet",
        "2026-10-09",
        Status::Done,
    );
    let stack = schedule("FREQ=WEEKLY;BYDAY=FR", "stack");
    let rows = [done, broken.clone()];
    assert_eq!(
        plan(&context(), &stack, &rows, date("2026-10-16")),
        Outcome::NotDue
    );
    // Open for `skip`, never cancelled by `replace`.
    let skip = schedule("FREQ=WEEKLY;BYDAY=FR", "skip");
    assert_eq!(
        plan(&context(), &skip, &rows, date("2026-10-23")),
        Outcome::Skipped {
            occurrence: date("2026-10-23"),
            open: vec![broken.id.clone()],
        }
    );
    let replace = schedule("FREQ=WEEKLY;BYDAY=FR", "replace");
    assert_eq!(
        created(&plan(&context(), &replace, &rows, date("2026-10-23"))),
        Some((date("2026-10-23"), &[][..]))
    );
    // In the archive: its occurrence still counts, but it is not open for `skip`.
    let archived = IndexedTask {
        location: TaskLocation::Archived(2026),
        ..broken
    };
    let archived_rows = [rows[0].clone(), archived];
    assert_eq!(
        plan(&context(), &skip, &archived_rows, date("2026-10-16")),
        Outcome::NotDue
    );
    assert_eq!(
        created(&plan(&context(), &skip, &archived_rows, date("2026-10-23"))),
        Some((date("2026-10-23"), &[][..]))
    );
    // Another schedule's key, or no key, is not this schedule's.
    let other = invalid(
        "0000000f",
        "key = \"standup/2026-10-16\"\nkey = \"timesheet/2026-13-01\"\n",
    );
    assert_eq!(
        created(&plan(&context(), &stack, &[other], date("2026-10-16"))),
        Some((date("2026-10-16"), &[][..]))
    );
}

fn machine(name: &str) -> MachineName {
    name.parse().unwrap()
}

#[test]
fn schedules_run_on_the_first_machine_of_the_context() {
    let both = [machine("laptop"), machine("tower")];
    assert_eq!(runner(Some(&both), &machine("laptop")), Runner::This);
    assert_eq!(
        runner(Some(&both), &machine("tower")),
        Runner::Other(machine("laptop"))
    );
    // Without `machines`, every machine holds the context: each one runs its schedules.
    assert_eq!(runner(None, &machine("tower")), Runner::Unpinned);
}
