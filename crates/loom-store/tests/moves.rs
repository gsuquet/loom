//! Moving a task to the store of another context: the files and the history follow, and a
//! failure leaves one task, or two that the reconciliation flags. Everything under a temporary
//! directory.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use loom_core::{
    Actor, Change, Command, ContextName, DomainError, Event, EventLog, FieldChange, Kind,
    MachineId, NewTask, Origin, Relations, Status, Task, TaskEdit, TaskId, Timestamp,
};
use loom_store::files::FileStore;
use loom_store::port::{PortError, StorePort, StoredTask, TaskWrite};
use loom_store::tasks::{IndexedState, Reconciled, TaskError, TaskIndex, Tasks};
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

/// A store whose removal of a task can be made to fail.
struct Flaky {
    inner: FileStore,
    fail_remove: Arc<AtomicBool>,
}

impl StorePort for Flaky {
    fn context(&self) -> &ContextName {
        self.inner.context()
    }
    fn create(&self, write: &TaskWrite) -> Result<(), PortError> {
        self.inner.create(write)
    }
    fn read(&self, id: &TaskId) -> Result<StoredTask, PortError> {
        self.inner.read(id)
    }
    fn update<T, E: From<PortError>>(
        &self,
        id: &TaskId,
        change: impl FnOnce(StoredTask) -> Result<(TaskWrite, T), E>,
    ) -> Result<T, E> {
        self.inner.update(id, change)
    }
    fn contains(&self, id: &TaskId) -> Result<bool, PortError> {
        self.inner.contains(id)
    }
    fn list(&self) -> Result<Vec<TaskId>, PortError> {
        self.inner.list()
    }
    fn archive(&self, id: &TaskId, year: u16) -> Result<(), PortError> {
        self.inner.archive(id, year)
    }
    fn read_events(&self, id: &TaskId) -> Result<EventLog, PortError> {
        self.inner.read_events(id)
    }
    fn receive(&self, from: &Self, id: &TaskId, write: &TaskWrite) -> Result<(), PortError> {
        self.inner.receive(&from.inner, id, write)
    }
    fn remove(&self, id: &TaskId) -> Result<(), PortError> {
        if self.fail_remove.load(Ordering::SeqCst) {
            return Err(PortError::Io {
                path: PathBuf::from("/injected"),
                source: std::io::Error::other("injected failure"),
            });
        }
        self.inner.remove(id)
    }
}

struct Fixture {
    dir: TempDir,
    fail_a: Arc<AtomicBool>,
    fail_b: Arc<AtomicBool>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
            fail_a: Arc::new(AtomicBool::new(false)),
            fail_b: Arc::new(AtomicBool::new(false)),
        }
    }

    fn root(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn task_dir(&self, name: &str, id: &TaskId) -> PathBuf {
        self.root(name).join("tasks").join(id.as_str())
    }

    fn tasks(&self) -> Tasks<Flaky> {
        let stores = [("a", &self.fail_a), ("b", &self.fail_b)]
            .into_iter()
            .map(|(name, fail)| Flaky {
                inner: FileStore::new(context(name), self.root(name)),
                fail_remove: Arc::clone(fail),
            })
            .collect();
        Tasks::new(
            stores,
            TaskIndex::open(&self.dir.path().join("index.db")).unwrap(),
        )
    }
}

fn new_task(title: &str, relations: Relations) -> NewTask {
    NewTask {
        title: title.to_owned(),
        kind: Kind::Chore,
        context: context("a"),
        status: Status::Ready,
        importance: None,
        due: None,
        pinned: false,
        links: Vec::new(),
        relations,
    }
}

fn create(tasks: &mut Tasks<Flaky>, title: &str, relations: Relations, seed: u64) -> Task {
    tasks
        .create(
            new_task(title, relations),
            at(100),
            &origin(Actor::User),
            &mut random(seed),
        )
        .unwrap()
}

fn move_to(tasks: &mut Tasks<Flaky>, id: &TaskId, to: &str) -> Result<Task, TaskError> {
    tasks.move_to(
        id,
        &context(to),
        at(200),
        &origin(Actor::User),
        &mut random(9),
    )
}

#[test]
fn a_move_carries_the_files_and_the_history_and_leaves_nothing_behind() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = create(&mut tasks, "Move me", Relations::default(), 1);
    tasks
        .apply(
            &task.id,
            Command::LogNote {
                text: "before".to_owned(),
            },
            at(150),
            &origin(Actor::User),
            &mut random(2),
        )
        .unwrap();
    let files = fixture.task_dir("a", &task.id).join("files");
    fs::create_dir_all(&files).unwrap();
    fs::write(files.join("design.md"), "kept").unwrap();

    let moved = move_to(&mut tasks, &task.id, "b").unwrap();

    assert_eq!(moved.context, context("b"));
    assert!(!fixture.task_dir("a", &task.id).exists());
    let target = fixture.task_dir("b", &task.id);
    assert_eq!(
        fs::read_to_string(target.join("files/design.md")).unwrap(),
        "kept"
    );
    let text = fs::read_to_string(target.join("task.md")).unwrap();
    assert!(text.contains("context = \"b\""), "{text}");
    // The history follows, and ends with the move.
    let log = tasks.events(&task.id).unwrap();
    assert_eq!(log.events.len(), 3);
    assert_eq!(
        log.events[2].event,
        Event::Edited {
            changes: vec![FieldChange::Context(Change {
                from: context("a"),
                to: context("b"),
            })]
        }
    );
    // Indexed in the target context, valid.
    let row = tasks.index().get(&task.id).unwrap().unwrap();
    assert_eq!(row.context, context("b"));
    assert!(matches!(row.state, IndexedState::Valid { .. }));
    // Nothing for the reconciliation to record afterwards.
    assert_eq!(
        tasks
            .reconcile(
                &context("b"),
                &task.id,
                at(300),
                &origin(Actor::File),
                &mut random(3)
            )
            .unwrap(),
        Reconciled::Unchanged
    );
}

#[test]
fn a_hand_edit_not_recorded_yet_is_recorded_before_the_move() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = create(&mut tasks, "Edited by hand", Relations::default(), 1);
    let path = fixture.task_dir("a", &task.id).join("task.md");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("Edited by hand", "Changed")).unwrap();

    move_to(&mut tasks, &task.id, "b").unwrap();

    let log = tasks.events(&task.id).unwrap();
    let actors: Vec<&Actor> = log.events.iter().map(|record| &record.actor).collect();
    assert_eq!(actors, [&Actor::User, &Actor::File, &Actor::User]);
    let moved = tasks.index().get(&task.id).unwrap().unwrap();
    let IndexedState::Valid { task: moved, .. } = moved.state else {
        panic!("invalid");
    };
    assert_eq!(moved.title, "Changed");
}

#[test]
fn a_refused_move_changes_nothing() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let parent = create(&mut tasks, "Parent", Relations::default(), 1);
    let child = create(
        &mut tasks,
        "Child",
        Relations {
            parent: Some(parent.id.clone()),
            ..Relations::default()
        },
        2,
    );
    let plain = create(&mut tasks, "Plain", Relations::default(), 3);

    assert!(matches!(
        move_to(&mut tasks, &plain.id, "a"),
        Err(TaskError::Domain(DomainError::SameContext(_)))
    ));
    assert!(matches!(
        move_to(&mut tasks, &plain.id, "elsewhere"),
        Err(TaskError::UnknownContext(_))
    ));
    assert!(matches!(
        move_to(&mut tasks, &parent.id, "b"),
        Err(TaskError::Domain(DomainError::MoveWithChildren(1)))
    ));
    assert!(matches!(
        move_to(&mut tasks, &child.id, "b"),
        Err(TaskError::Domain(DomainError::MoveWithParent(_)))
    ));
    let missing: TaskId = "zzzzzzzz".parse().unwrap();
    assert!(matches!(
        move_to(&mut tasks, &missing, "b"),
        Err(TaskError::NotFound(_))
    ));
    for id in [&parent.id, &child.id, &plain.id] {
        assert!(fixture.task_dir("a", id).is_dir());
        assert!(!fixture.task_dir("b", id).exists());
    }
}

#[test]
fn an_archived_or_invalid_task_is_not_moved() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let archived = create(&mut tasks, "Closed", Relations::default(), 1);
    tasks.archive(&archived.id, 2026).unwrap();
    assert!(matches!(
        move_to(&mut tasks, &archived.id, "b"),
        Err(TaskError::Archived { year: 2026, .. })
    ));
    let broken = create(&mut tasks, "Broken", Relations::default(), 2);
    fs::write(
        fixture.task_dir("a", &broken.id).join("task.md"),
        "not a task",
    )
    .unwrap();
    tasks
        .reconcile(
            &context("a"),
            &broken.id,
            at(300),
            &origin(Actor::File),
            &mut random(5),
        )
        .unwrap();
    assert!(matches!(
        move_to(&mut tasks, &broken.id, "b"),
        Err(TaskError::Invalid(_))
    ));
    assert!(fixture.task_dir("a", &broken.id).is_dir());
    assert!(!fixture.task_dir("b", &broken.id).exists());
}

#[test]
fn a_failed_removal_rolls_the_copy_back_and_the_task_stays() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = create(&mut tasks, "Stay", Relations::default(), 1);
    fixture.fail_a.store(true, Ordering::SeqCst);

    let error = move_to(&mut tasks, &task.id, "b").unwrap_err();

    assert!(matches!(error, TaskError::MoveFailed { .. }), "{error}");
    assert!(
        error.to_string().contains("stays in context `a`"),
        "{error}"
    );
    assert!(fixture.task_dir("a", &task.id).is_dir());
    assert!(!fixture.task_dir("b", &task.id).exists());
    // One valid task, still in `a`, and it can be changed and moved once the cause is gone.
    let row = tasks.index().get(&task.id).unwrap().unwrap();
    assert_eq!(row.context, context("a"));
    fixture.fail_a.store(false, Ordering::SeqCst);
    assert_eq!(
        move_to(&mut tasks, &task.id, "b").unwrap().context,
        context("b")
    );
}

#[test]
fn a_failed_rollback_leaves_two_copies_that_are_flagged_and_told() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = create(&mut tasks, "Both", Relations::default(), 1);
    fixture.fail_a.store(true, Ordering::SeqCst);
    fixture.fail_b.store(true, Ordering::SeqCst);

    let error = move_to(&mut tasks, &task.id, "b").unwrap_err();

    assert!(matches!(error, TaskError::MoveIncomplete { .. }), "{error}");
    let text = error.to_string();
    assert!(text.contains("remove the task directory"), "{text}");
    assert!(fixture.task_dir("a", &task.id).is_dir());
    assert!(fixture.task_dir("b", &task.id).is_dir());
    // The reconciliation refuses to choose: the task is flagged as a duplicate.
    let reconciled = tasks
        .reconcile(
            &context("a"),
            &task.id,
            at(300),
            &origin(Actor::File),
            &mut random(4),
        )
        .unwrap();
    assert!(matches!(reconciled, Reconciled::Duplicate(_)));
}

#[test]
fn a_move_is_not_undone_and_other_changes_after_it_are() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = create(&mut tasks, "Back", Relations::default(), 1);
    move_to(&mut tasks, &task.id, "b").unwrap();
    let error = tasks
        .undo(&task.id, at(300), &origin(Actor::User), &mut random(6))
        .unwrap_err();
    assert!(
        matches!(error, TaskError::Domain(DomainError::UndoMove(ref back)) if *back == context("a")),
        "{error}"
    );
    // A change after the move is the last one, and is undone as usual.
    tasks
        .apply(
            &task.id,
            Command::Edit(TaskEdit {
                title: Some("Renamed".to_owned()),
                ..TaskEdit::default()
            }),
            at(310),
            &origin(Actor::User),
            &mut random(7),
        )
        .unwrap();
    let undone = tasks
        .undo(&task.id, at(320), &origin(Actor::User), &mut random(8))
        .unwrap();
    assert_eq!(undone.title, "Back");
    // And back where it came from.
    assert_eq!(
        move_to(&mut tasks, &task.id, "a").unwrap().context,
        context("a")
    );
}

#[test]
fn the_focus_of_the_source_context_does_not_follow_the_task() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = create(&mut tasks, "Focused", Relations::default(), 1);
    tasks
        .go(&task.id, at(150), &origin(Actor::User), &mut random(2))
        .unwrap();
    assert_eq!(tasks.index().focus(None).unwrap().len(), 1);
    move_to(&mut tasks, &task.id, "b").unwrap();
    assert_eq!(tasks.index().focus(None).unwrap().len(), 0);
}

#[test]
fn a_document_is_needed_to_receive() {
    // The port refuses a write without a document, before it touches a file.
    let fixture = Fixture::new();
    let tasks = fixture.tasks();
    let from = tasks.stores().get(&context("a")).unwrap();
    let to = tasks.stores().get(&context("b")).unwrap();
    let id: TaskId = "k3q7zm2a".parse().unwrap();
    assert!(matches!(
        to.receive(from, &id, &TaskWrite::default()),
        Err(PortError::MissingDocument)
    ));
}
