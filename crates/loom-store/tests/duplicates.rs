//! The same task ID in two context stores (a task directory copied by hand): the index keeps one
//! row for it, flagged invalid, and no command writes either file. Everything under a temporary
//! directory.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use loom_core::{
    Actor, Command, ContextName, Kind, MachineId, NewTask, Origin, Relations, Status, Task, TaskId,
    Timestamp,
};
use loom_store::Reader;
use loom_store::files::FileStore;
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

    fn root(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn task_dir(&self, name: &str, id: &TaskId) -> PathBuf {
        self.root(name).join("tasks").join(id.as_str())
    }

    fn tasks(&self) -> Tasks<FileStore> {
        let stores = ["a", "b"]
            .into_iter()
            .map(|name| FileStore::new(context(name), self.root(name)))
            .collect();
        Tasks::new(stores, TaskIndex::open(&self.db()).unwrap())
    }

    /// A task created in `a`.
    fn created(tasks: &mut Tasks<FileStore>) -> Task {
        tasks
            .create(
                NewTask {
                    title: "Twice".to_owned(),
                    kind: Kind::Chore,
                    context: context("a"),
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

    /// The directory of `task` copied to `b`, as `cp -r` leaves it: the `context` of the copy
    /// is still `a`.
    fn copy_to_b(&self, task: &Task) -> PathBuf {
        let copy = self.task_dir("b", &task.id);
        fs::create_dir_all(&copy).unwrap();
        let source = self.task_dir("a", &task.id);
        for file in ["task.md", "events.jsonl"] {
            fs::copy(source.join(file), copy.join(file)).unwrap();
        }
        copy
    }

    /// A task created in `a`, then its directory copied to `b` with the `context` of its file
    /// changed: what a careful hand copy between two stores leaves.
    fn duplicated(&self, tasks: &mut Tasks<FileStore>) -> Task {
        let task = Self::created(tasks);
        let copy = self.copy_to_b(&task);
        let text = fs::read_to_string(copy.join("task.md")).unwrap();
        assert!(text.contains("context = \"a\""), "{text}");
        fs::write(
            copy.join("task.md"),
            text.replace("context = \"a\"", "context = \"b\""),
        )
        .unwrap();
        task
    }
}

/// Sets the mode of `path`; restores `0o755` on drop so that the temporary directory can be
/// removed even when the test fails.
struct Mode(PathBuf);

impl Mode {
    fn set(path: PathBuf, mode: u32) -> Self {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        Self(path)
    }
}

impl Drop for Mode {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

fn reconcile(tasks: &mut Tasks<FileStore>, name: &str, id: &TaskId, now: u64) -> Reconciled {
    tasks
        .reconcile(
            &context(name),
            id,
            at(now),
            &origin(Actor::File),
            &mut random(now),
        )
        .unwrap()
}

/// The `task.md` and `events.jsonl` of both copies, to check that nothing wrote them.
fn files(fixture: &Fixture, id: &TaskId) -> Vec<String> {
    ["a", "b"]
        .into_iter()
        .flat_map(|name| {
            let dir = fixture.task_dir(name, id);
            ["task.md", "events.jsonl"].map(|file| read(&dir.join(file)))
        })
        .collect()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

/// The error of the indexed row of `id`, `None` when the row is valid.
fn row_error(fixture: &Fixture, id: &TaskId) -> Option<String> {
    let row = Reader::open(&fixture.db())
        .unwrap()
        .task(id)
        .unwrap()
        .unwrap();
    match row.state {
        IndexedState::Invalid { error, .. } => Some(error),
        IndexedState::Valid { .. } => None,
    }
}

#[test]
fn a_reindex_flags_the_id_once_and_never_picks_a_copy() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = fixture.duplicated(&mut tasks);
    let before = files(&fixture, &task.id);

    let reindexed = tasks.reindex().unwrap();
    assert_eq!(reindexed.rows.len(), 1);
    assert_eq!(reindexed.failed, Vec::new());
    let error = row_error(&fixture, &task.id).unwrap();
    assert!(error.contains("duplicate ID across contexts"), "{error}");
    assert!(error.contains("a, b"), "{error}");
    // Two reindexes give the same row: nothing flip-flops.
    let first = tasks.index().get(&task.id).unwrap();
    tasks.reindex().unwrap();
    assert_eq!(tasks.index().get(&task.id).unwrap(), first);
    assert_eq!(files(&fixture, &task.id), before);
}

#[test]
fn reconciling_either_copy_keeps_the_flagged_row() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = fixture.duplicated(&mut tasks);
    let before = files(&fixture, &task.id);

    let contexts = vec![context("a"), context("b")];
    assert_eq!(
        reconcile(&mut tasks, "b", &task.id, 200),
        Reconciled::Duplicate(contexts)
    );
    let flagged = tasks.index().get(&task.id).unwrap();
    assert_eq!(
        reconcile(&mut tasks, "a", &task.id, 201),
        Reconciled::Unchanged
    );
    assert_eq!(
        reconcile(&mut tasks, "b", &task.id, 202),
        Reconciled::Unchanged
    );
    assert_eq!(tasks.index().get(&task.id).unwrap(), flagged);
    assert!(row_error(&fixture, &task.id).is_some());
    // No `edited` event for the copy that differs: nothing is written.
    assert_eq!(files(&fixture, &task.id), before);

    let all = tasks
        .reconcile_all(at(300), &origin(Actor::File), &mut random(3))
        .unwrap();
    assert_eq!(all.failed, Vec::new());
    assert!(
        all.results
            .iter()
            .all(|(_, reconciled)| *reconciled == Reconciled::Unchanged)
    );
    assert_eq!(tasks.index().get(&task.id).unwrap(), flagged);
    assert_eq!(files(&fixture, &task.id), before);
}

#[test]
fn a_command_on_a_duplicated_id_is_refused_and_writes_neither_file() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = fixture.duplicated(&mut tasks);
    reconcile(&mut tasks, "a", &task.id, 200);
    let before = files(&fixture, &task.id);

    let refused = tasks.apply(
        &task.id,
        Command::SetStatus {
            status: Status::Done,
        },
        at(300),
        &origin(Actor::User),
        &mut random(4),
    );
    assert!(matches!(refused, Err(TaskError::Invalid(_))), "{refused:?}");
    assert_eq!(files(&fixture, &task.id), before);
}

#[test]
fn removing_one_copy_makes_the_task_valid_again() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = fixture.duplicated(&mut tasks);
    reconcile(&mut tasks, "a", &task.id, 200);
    assert!(row_error(&fixture, &task.id).is_some());

    // The watcher reports the removed copy; the surviving one is indexed.
    fs::remove_dir_all(fixture.task_dir("a", &task.id)).unwrap();
    assert_eq!(
        reconcile(&mut tasks, "a", &task.id, 300),
        Reconciled::Indexed
    );
    let row = tasks.index().get(&task.id).unwrap().unwrap();
    assert_eq!(row.context, context("b"));
    assert_eq!(row.title(), "Twice");
    assert!(row_error(&fixture, &task.id).is_none());

    // And the command works, on the file that is left.
    tasks
        .apply(
            &task.id,
            Command::SetStatus {
                status: Status::Done,
            },
            at(400),
            &origin(Actor::User),
            &mut random(5),
        )
        .unwrap();
    assert!(read(&fixture.task_dir("b", &task.id).join("task.md")).contains("done"));
}

#[test]
fn an_unreadable_second_store_does_not_block_commands_in_the_first() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = Fixture::created(&mut tasks);
    let archive = fixture.root("b").join("archive");
    fs::create_dir_all(&archive).unwrap();
    let _locked = Mode::set(archive.clone(), 0o000);
    if fs::read_dir(&archive).is_ok() {
        // Root reads everything: the store cannot be made unreadable.
        return;
    }

    // `a` cannot tell whether `b` holds the ID: it goes on, as `reindex` goes on without `b`.
    assert_eq!(
        reconcile(&mut tasks, "a", &task.id, 200),
        Reconciled::Unchanged
    );
    tasks
        .apply(
            &task.id,
            Command::SetStatus {
                status: Status::Done,
            },
            at(300),
            &origin(Actor::User),
            &mut random(6),
        )
        .unwrap();
    assert!(read(&fixture.task_dir("a", &task.id).join("task.md")).contains("done"));
}

#[test]
fn a_copy_that_cannot_be_read_still_flags_the_id_in_a_reindex() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = fixture.duplicated(&mut tasks);
    let file = fixture.task_dir("b", &task.id).join("task.md");
    let _locked = Mode::set(file.clone(), 0o000);
    if fs::read(&file).is_ok() {
        return;
    }

    let reindexed = tasks.reindex().unwrap();
    // The copy that cannot be read is reported, and the ID is flagged all the same: the row
    // does not turn into the readable copy.
    assert_eq!(reindexed.failed.len(), 1);
    let error = row_error(&fixture, &task.id).unwrap();
    assert!(error.contains("duplicate ID across contexts"), "{error}");
}

#[test]
fn a_plain_copy_of_the_directory_is_flagged_too() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = Fixture::created(&mut tasks);
    // `cp -r`: the file of the copy still says `context = "a"`, which is also invalid in `b`.
    fixture.copy_to_b(&task);
    let before = files(&fixture, &task.id);

    assert!(matches!(
        reconcile(&mut tasks, "b", &task.id, 200),
        Reconciled::Duplicate(contexts) if contexts == [context("a"), context("b")]
    ));
    tasks.reindex().unwrap();
    let error = row_error(&fixture, &task.id).unwrap();
    assert!(error.starts_with("duplicate ID across contexts"), "{error}");
    assert_eq!(files(&fixture, &task.id), before);
}
