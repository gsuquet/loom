//! Hand edits of `task.md`: reconciled into the index and recorded as events of the actor
//! `file`, an invalid file flagged and never rewritten. Everything under a temporary directory.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::path::PathBuf;

use loom_core::{
    Actor, Change, ContextName, Event, FieldChange, Kind, MachineId, NewTask, Origin, Relations,
    Status, Task, TaskId, Timestamp,
};
use loom_store::Reader;
use loom_store::files::FileStore;
use loom_store::port::StorePort as _;
use loom_store::tasks::{IndexedState, Reconciled, TaskIndex, Tasks};
use tempfile::TempDir;

const fn at(secs: u64) -> Timestamp {
    Timestamp::from_unix_secs(secs).unwrap()
}

fn context(name: &str) -> ContextName {
    name.parse().unwrap()
}

fn origin(actor: Actor) -> Origin {
    Origin {
        actor,
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

    fn tasks(&self) -> Tasks<FileStore> {
        let store = FileStore::new(context("work"), self.dir.path().join("work"));
        Tasks::new(vec![store], TaskIndex::open(&self.db()).unwrap())
    }

    fn file(&self, id: &TaskId) -> PathBuf {
        self.dir
            .path()
            .join("work/tasks")
            .join(id.as_str())
            .join("task.md")
    }

    fn create(tasks: &mut Tasks<FileStore>, title: &str) -> Task {
        tasks
            .create(
                NewTask {
                    title: title.to_owned(),
                    kind: Kind::Chore,
                    context: context("work"),
                    status: Status::Ready,
                    importance: None,
                    due: None,
                    pinned: false,
                    links: Vec::new(),
                    relations: Relations::default(),
                },
                at(100),
                &origin(Actor::User),
                &mut random(1),
            )
            .unwrap()
    }

    /// Replaces `from` with `to` in the `task.md` of `id`, as an editor would.
    fn edit(&self, id: &TaskId, from: &str, to: &str) -> String {
        let path = self.file(id);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{text}");
        let edited = text.replace(from, to);
        fs::write(&path, &edited).unwrap();
        edited
    }
}

fn reconcile(tasks: &mut Tasks<FileStore>, id: &TaskId, now: u64) -> Reconciled {
    tasks
        .reconcile(
            &context("work"),
            id,
            at(now),
            &origin(Actor::File),
            &mut random(now),
        )
        .unwrap()
}

#[test]
fn a_hand_edit_of_the_title_is_indexed_and_recorded_once() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = Fixture::create(&mut tasks, "Before");
    let edited = fixture.edit(
        &task.id,
        "title = \"Before\"",
        "title = \"After\" # by hand",
    );

    let title = FieldChange::Title(Change {
        from: "Before".to_owned(),
        to: "After".to_owned(),
    });
    assert_eq!(
        reconcile(&mut tasks, &task.id, 200),
        Reconciled::Edited(vec![title.clone()])
    );
    // The file is kept byte for byte, comment included.
    assert_eq!(fs::read_to_string(fixture.file(&task.id)).unwrap(), edited);
    let log = tasks.stores()[&context("work")]
        .read_events(&task.id)
        .unwrap();
    let last = log.events.last().unwrap();
    assert_eq!(last.actor, Actor::File);
    assert_eq!(last.at, at(200));
    assert_eq!(
        last.event,
        Event::Edited {
            changes: vec![title]
        }
    );
    let indexed = Reader::open(&fixture.db())
        .unwrap()
        .task(&task.id)
        .unwrap()
        .unwrap();
    assert_eq!(indexed.title(), "After");
    let IndexedState::Valid { task: indexed, .. } = indexed.state else {
        panic!("invalid");
    };
    assert_eq!(indexed.updated, at(200));

    // Seen again (a second watcher event for the same save): nothing more.
    assert_eq!(reconcile(&mut tasks, &task.id, 300), Reconciled::Unchanged);
    let log = tasks.stores()[&context("work")]
        .read_events(&task.id)
        .unwrap();
    assert_eq!(log.events.len(), 2);
}

#[test]
fn a_write_by_loomd_is_not_a_hand_edit() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = Fixture::create(&mut tasks, "Mine");
    tasks
        .apply(
            &task.id,
            loom_core::Command::Pin,
            at(150),
            &origin(Actor::User),
            &mut random(2),
        )
        .unwrap();
    assert_eq!(reconcile(&mut tasks, &task.id, 200), Reconciled::Unchanged);
}

#[test]
fn an_invalid_hand_edit_is_flagged_and_the_file_never_rewritten() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = Fixture::create(&mut tasks, "Fragile");
    let broken = fixture.edit(&task.id, "title = \"Fragile\"", "title = ");

    assert_eq!(reconcile(&mut tasks, &task.id, 200), Reconciled::Indexed);
    assert_eq!(fs::read_to_string(fixture.file(&task.id)).unwrap(), broken);
    let indexed = Reader::open(&fixture.db())
        .unwrap()
        .task(&task.id)
        .unwrap()
        .unwrap();
    assert!(indexed.read_only());
    assert!(matches!(indexed.state, IndexedState::Invalid { ref text, .. } if *text == broken));
    // No event for a file that cannot be read.
    let log = tasks.stores()[&context("work")]
        .read_events(&task.id)
        .unwrap();
    assert_eq!(log.events.len(), 1);

    // Fixed by hand: the change since the last valid state is recorded.
    fixture.edit(&task.id, "title = \n", "title = \"Sturdy\"\n");
    assert!(matches!(
        reconcile(&mut tasks, &task.id, 300),
        Reconciled::Edited(changes) if changes.len() == 1
    ));
    let indexed = Reader::open(&fixture.db())
        .unwrap()
        .task(&task.id)
        .unwrap()
        .unwrap();
    assert_eq!(indexed.title(), "Sturdy");
    assert!(!indexed.read_only());
}

#[test]
fn a_notes_only_edit_is_indexed_without_an_event() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = Fixture::create(&mut tasks, "Notes");
    let path = fixture.file(&task.id);
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("Called the client: answer Monday.\n");
    fs::write(&path, &text).unwrap();
    assert_eq!(reconcile(&mut tasks, &task.id, 200), Reconciled::Indexed);
    let reader = Reader::open(&fixture.db()).unwrap();
    assert_eq!(
        reader.search_tasks("monday", None, 5).unwrap(),
        std::slice::from_ref(&task.id)
    );
    let log = tasks.stores()[&context("work")]
        .read_events(&task.id)
        .unwrap();
    assert_eq!(log.events.len(), 1);
}

#[test]
fn reconciling_every_store_catches_edits_made_while_loomd_was_stopped() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let edited = Fixture::create(&mut tasks, "Edited");
    let removed = Fixture::create(&mut tasks, "Removed");
    let untouched = Fixture::create(&mut tasks, "Untouched");
    drop(tasks);
    // While loomd is stopped: an edit, a removal, and the index deleted.
    fixture.edit(&edited.id, "status = \"ready\"", "status = \"done\"");
    fs::remove_dir_all(fixture.file(&removed.id).parent().unwrap()).unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", fixture.db().display()));
    }

    let mut tasks = fixture.tasks();
    let mut results = tasks
        .reconcile_all(at(500), &origin(Actor::File), &mut random(5))
        .unwrap()
        .results;
    results.sort_by(|a, b| a.0.cmp(&b.0));
    let mut expected = vec![
        (
            edited.id,
            Reconciled::Edited(vec![FieldChange::Status(Change {
                from: Status::Ready,
                to: Status::Done,
            })]),
        ),
        (untouched.id, Reconciled::Indexed),
    ];
    // The index was deleted: the removed task was not in it, nothing to report.
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(results, expected);
    let ids: Vec<TaskId> = Reader::open(&fixture.db())
        .unwrap()
        .tasks(None)
        .unwrap()
        .into_iter()
        .map(|task| task.id)
        .collect();
    assert!(!ids.contains(&removed.id));
    assert_eq!(ids.len(), 2);

    // Indexed now: a second pass changes nothing.
    let again = tasks
        .reconcile_all(at(600), &origin(Actor::File), &mut random(6))
        .unwrap()
        .results;
    assert!(
        again
            .iter()
            .all(|(_, result)| *result == Reconciled::Unchanged)
    );
}

#[test]
fn a_task_removed_by_hand_leaves_the_index() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = Fixture::create(&mut tasks, "Gone");
    fs::remove_dir_all(fixture.file(&task.id).parent().unwrap()).unwrap();
    assert_eq!(reconcile(&mut tasks, &task.id, 200), Reconciled::Removed);
    assert_eq!(
        Reader::open(&fixture.db()).unwrap().task(&task.id).unwrap(),
        None
    );
}

#[test]
fn an_unreadable_task_does_not_stop_the_reconciliation_of_the_others() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let fixture = Fixture::new();
    if fs::metadata(fixture.dir.path()).unwrap().uid() == 0 {
        eprintln!("skipped: root reads a file of mode 000");
        return;
    }
    let mut tasks = fixture.tasks();
    let first = Fixture::create(&mut tasks, "First");
    let locked = Fixture::create(&mut tasks, "Locked");
    let last = Fixture::create(&mut tasks, "Last");
    for task in [&first, &last] {
        fixture.edit(&task.id, "status = \"ready\"", "status = \"active\"");
    }
    let path = fixture.file(&locked.id);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();

    let report = tasks
        .reconcile_all(at(500), &origin(Actor::File), &mut random(5))
        .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    // Both readable tasks got their edit recorded, whatever their order around the locked one.
    let edited: Vec<&TaskId> = report
        .results
        .iter()
        .filter(|(_, result)| matches!(result, Reconciled::Edited(_)))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(edited.len(), 2, "{report:?}");
    assert!(edited.contains(&&first.id) && edited.contains(&&last.id));
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].id.as_ref(), Some(&locked.id));
    assert!(report.failed[0].error.contains("task.md"), "{report:?}");
    // The unreadable task keeps its row: it is not "removed".
    let reader = Reader::open(&fixture.db()).unwrap();
    assert_eq!(reader.task(&locked.id).unwrap().unwrap().title(), "Locked");
}
