//! The archive: closed tasks move to `archive/<yyyy>/<id>/` after a delay, stay indexed,
//! searched and readable, and refuse every change. Everything under a temporary directory.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use loom_core::{
    Actor, Command, ContextName, Kind, MachineId, NewTask, Origin, Relations, Status, Task, TaskId,
    Timestamp,
};
use loom_store::Reader;
use loom_store::files::FileStore;
use loom_store::port::{PortError, StorePort as _, TaskLocation};
use loom_store::tasks::{ArchivedTask, Reconciled, TaskError, TaskIndex, Tasks};
use rusqlite::Connection;
use tempfile::TempDir;

const DAY: u64 = 86_400;
const WEEK: Duration = Duration::from_secs(7 * DAY);
/// 2026-10-12T08:00:00Z.
const START: u64 = 1_791_792_000;

const fn at(secs: u64) -> Timestamp {
    Timestamp::from_unix_secs(START + secs).unwrap()
}

fn context(name: &str) -> ContextName {
    name.parse().unwrap()
}

fn origin() -> Origin {
    Origin {
        actor: Actor::User,
        machine_id: MachineId("laptop".to_owned()),
    }
}

fn random(seed: u64) -> impl FnMut() -> u64 {
    let mut bits = seed;
    move || {
        bits = bits
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        bits
    }
}

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
        }
    }

    fn db(&self) -> PathBuf {
        self.dir.path().join("index.db")
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("contexts/work")
    }

    fn store(&self) -> FileStore {
        FileStore::new(context("work"), self.root())
    }

    fn tasks(&self) -> Tasks<FileStore> {
        Tasks::new(vec![self.store()], TaskIndex::open(&self.db()).unwrap())
    }

    fn reader(&self) -> Reader {
        Reader::open(&self.db()).unwrap()
    }

    fn active_dir(&self, id: &TaskId) -> PathBuf {
        self.root().join("tasks").join(id.as_str())
    }

    fn archived_dir(&self, id: &TaskId, year: u16) -> PathBuf {
        self.root()
            .join("archive")
            .join(year.to_string())
            .join(id.as_str())
    }

    fn location(&self, id: &TaskId) -> TaskLocation {
        self.reader().task(id).unwrap().unwrap().location
    }
}

/// Creates a ready task titled `title` at `START`, and sets it to `status` at `START + secs`.
fn task(tasks: &mut Tasks<FileStore>, title: &str, status: Status, secs: u64, seed: u64) -> Task {
    let new = NewTask {
        title: title.to_owned(),
        kind: Kind::Chore,
        context: context("work"),
        status: Status::Ready,
        importance: None,
        due: None,
        pinned: false,
        links: Vec::new(),
        relations: Relations::default(),
    };
    let task = tasks
        .create(new, at(0), &origin(), &mut random(seed))
        .unwrap();
    if status == Status::Ready {
        return task;
    }
    tasks
        .apply(
            &task.id,
            Command::SetStatus { status },
            at(secs),
            &origin(),
            &mut random(seed + 1),
        )
        .unwrap()
}

/// Every task row, the archive year included, sorted by ID.
fn rows(db: &Path) -> Vec<String> {
    let conn = Connection::open(db).unwrap();
    let mut statement = conn
        .prepare("SELECT id, hash, task, status, archived FROM tasks ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok(format!(
                "{}|{}|{:?}|{:?}|{:?}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_task_done_8_days_ago_is_archived_6_days_ago_is_not() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let done = task(&mut tasks, "Rotate certificates", Status::Done, 0, 1);
    let open = task(&mut tasks, "Renew domain", Status::Ready, 0, 3);

    let report = tasks.archive_due(at(6 * DAY), WEEK).unwrap();
    assert_eq!(report.moved, []);
    assert!(fixture.active_dir(&done.id).is_dir());

    let report = tasks.archive_due(at(8 * DAY), WEEK).unwrap();
    assert_eq!(
        report.moved,
        [ArchivedTask {
            context: context("work"),
            id: done.id.clone(),
            year: 2026,
        }]
    );
    assert_eq!(report.failed, []);
    // Moved whole: in the archive only, with its files.
    assert!(!fixture.active_dir(&done.id).exists());
    let archived = fixture.archived_dir(&done.id, 2026);
    assert!(archived.join("task.md").is_file());
    assert!(archived.join("events.jsonl").is_file());
    assert_eq!(fixture.location(&done.id), TaskLocation::Archived(2026));
    // An open task stays.
    assert_eq!(fixture.location(&open.id), TaskLocation::Active);
    // Nothing more to move.
    assert_eq!(tasks.archive_due(at(9 * DAY), WEEK).unwrap().moved, []);
}

#[test]
fn an_archived_task_stays_readable_and_refuses_every_change() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let done = task(&mut tasks, "Rotate certificates", Status::Cancelled, 0, 1);
    tasks.archive_due(at(8 * DAY), WEEK).unwrap();
    let before = fs::read(fixture.archived_dir(&done.id, 2026).join("task.md")).unwrap();
    let log = tasks.events(&done.id).unwrap();

    let reopen = tasks.apply(
        &done.id,
        Command::SetStatus {
            status: Status::Ready,
        },
        at(9 * DAY),
        &origin(),
        &mut random(9),
    );
    assert!(
        matches!(&reopen, Err(TaskError::Archived { id, year: 2026 }) if *id == done.id),
        "{reopen:?}"
    );
    let message = reopen.unwrap_err().to_string();
    assert!(message.contains("is archived (archive/2026)"), "{message}");
    assert!(matches!(
        tasks.undo(&done.id, at(9 * DAY), &origin(), &mut random(9)),
        Err(TaskError::Archived { .. })
    ));
    // Untouched, and still read.
    let after = fs::read(fixture.archived_dir(&done.id, 2026).join("task.md")).unwrap();
    assert_eq!(after, before);
    assert_eq!(tasks.events(&done.id).unwrap(), log);
    let row = fixture.reader().task(&done.id).unwrap().unwrap();
    assert!(row.read_only());
    assert_eq!(row.title(), "Rotate certificates");
}

#[test]
fn archived_tasks_are_searched_after_closed_ones() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let archived = task(&mut tasks, "Certificates for staging", Status::Done, 0, 1);
    tasks.archive_due(at(8 * DAY), WEEK).unwrap();
    let closed = task(
        &mut tasks,
        "Certificates for production",
        Status::Done,
        0,
        3,
    );
    let found = fixture
        .reader()
        .search_tasks("certificates", None, 10)
        .unwrap();
    assert_eq!(found, [closed.id, archived.id]);
}

#[test]
fn a_reindex_gives_the_same_rows_for_archived_tasks() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    task(&mut tasks, "Archived", Status::Done, 0, 1);
    tasks.archive_due(at(8 * DAY), WEEK).unwrap();
    task(&mut tasks, "Active", Status::Ready, 0, 3);
    let before = rows(&fixture.db());
    assert_eq!(before.len(), 2);
    assert!(
        before.iter().any(|row| row.ends_with("|Some(2026)")),
        "{before:?}"
    );

    tasks.reindex().unwrap();
    assert_eq!(rows(&fixture.db()), before);

    drop(tasks);
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", fixture.db().display()));
    }
    let mut tasks = fixture.tasks();
    assert_eq!(tasks.reindex().unwrap().rows.len(), 2);
    assert_eq!(rows(&fixture.db()), before);
}

#[test]
fn a_crash_after_the_move_leaves_the_task_archived_after_the_reconciliation() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let done = task(&mut tasks, "Rotate certificates", Status::Done, 0, 1);
    // The rename happened, the index was not written: as after a crash between the two.
    let target = fixture.archived_dir(&done.id, 2026);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::rename(fixture.active_dir(&done.id), &target).unwrap();
    assert_eq!(fixture.location(&done.id), TaskLocation::Active);

    let report = tasks
        .reconcile_all(at(8 * DAY), &origin(), &mut random(5))
        .unwrap();
    assert_eq!(report.results, [(done.id.clone(), Reconciled::Indexed)]);
    assert_eq!(fixture.location(&done.id), TaskLocation::Archived(2026));
    assert_eq!(
        fixture.store().list().unwrap(),
        std::slice::from_ref(&done.id)
    );

    // Moved back by hand: active again.
    fs::rename(&target, fixture.active_dir(&done.id)).unwrap();
    tasks
        .reconcile(
            &context("work"),
            &done.id,
            at(9 * DAY),
            &origin(),
            &mut random(6),
        )
        .unwrap();
    assert_eq!(fixture.location(&done.id), TaskLocation::Active);
}

#[test]
fn the_move_never_overwrites_a_task_in_the_archive() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let done = task(&mut tasks, "Rotate certificates", Status::Done, 0, 1);
    // A copy already in the archive (restored by hand): the active one wins, nothing moves.
    let copy = fixture.archived_dir(&done.id, 2026);
    fs::create_dir_all(&copy).unwrap();
    fs::write(copy.join("task.md"), "copy").unwrap();

    let report = tasks.archive_due(at(8 * DAY), WEEK).unwrap();
    assert_eq!(report.moved, []);
    assert_eq!(report.failed.len(), 1);
    assert!(
        report.failed[0].error.contains("already exists"),
        "{:?}",
        report.failed
    );
    assert!(fixture.active_dir(&done.id).join("task.md").is_file());
    assert_eq!(fs::read_to_string(copy.join("task.md")).unwrap(), "copy");
    assert_eq!(
        fixture.store().list().unwrap(),
        std::slice::from_ref(&done.id)
    );
    assert_eq!(fixture.location(&done.id), TaskLocation::Active);
}

#[test]
fn the_store_moves_an_active_task_only() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let done = task(&mut tasks, "Rotate certificates", Status::Done, 0, 1);
    let store = fixture.store();
    let missing: TaskId = "zzzzzzzz".parse().unwrap();
    assert!(matches!(
        store.archive(&missing, 2026),
        Err(PortError::NotFound { .. })
    ));
    store.archive(&done.id, 2026).unwrap();
    assert!(matches!(
        store.archive(&done.id, 2027),
        Err(PortError::Archived { year: 2026, .. })
    ));
    // Read, listed and logged where it is.
    let stored = store.read(&done.id).unwrap();
    assert_eq!(stored.location, TaskLocation::Archived(2026));
    assert_eq!(stored.task().unwrap().status, Status::Done);
    assert_eq!(store.list().unwrap(), std::slice::from_ref(&done.id));
    assert_eq!(store.read_events(&done.id).unwrap().events.len(), 2);
}
