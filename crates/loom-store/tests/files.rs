//! The files adapter of the task store, on a temporary directory: layout, atomic writes, the
//! per-task lock, crash leftovers.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;
use std::thread;

use loom_core::{
    Actor, Command, ContextName, Kind, MachineId, NewTask, Origin, Relations, Status, Task,
    TaskDocument, TaskEdit, TaskId, Timestamp, apply, create, render_task_md,
};
use loom_store::atomic::Staged;
use loom_store::files::{FileStore, TASK_DIR_MODE, TASK_FILE_MODE};
use loom_store::port::{InvalidTask, PortError, StorePort as _, TaskWrite};
use tempfile::TempDir;

const fn at(secs: u64) -> Timestamp {
    Timestamp::from_unix_secs(secs).unwrap()
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

/// A deterministic random source: each test draws the same IDs.
fn random(seed: u64) -> impl FnMut() -> u64 {
    let mut bits = seed;
    move || {
        bits = bits
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        bits
    }
}

fn new_task(title: &str, context_name: &str, seed: u64) -> (Task, TaskWrite) {
    let (task, events) = create(
        NewTask {
            title: title.to_owned(),
            kind: Kind::Chore,
            context: context(context_name),
            status: Status::Inbox,
            importance: None,
            due: None,
            pinned: false,
            links: Vec::new(),
            relations: Relations::default(),
        },
        &mut random(seed),
        &[],
        at(100),
        &origin(),
    )
    .unwrap();
    let write = TaskWrite {
        document: Some(TaskDocument::new(&task)),
        events,
    };
    (task, write)
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// A store of context `work` at `<dir>/work`.
fn store(dir: &TempDir) -> FileStore {
    FileStore::new(context("work"), dir.path().join("work"))
}

/// Applies `command` to task `id` at `now`, through `update`.
fn change(store: &FileStore, id: &TaskId, command: Command, now: Timestamp) -> Task {
    store
        .update(id, |stored| {
            let task = stored.task().unwrap();
            let mut document = stored.file.document.unwrap();
            let (task, events) = apply(
                task,
                command,
                &stored.events.events,
                now,
                &origin(),
                &mut random(now.unix_secs()),
            )
            .unwrap();
            document.set_task(&task).unwrap();
            Ok::<_, PortError>((
                TaskWrite {
                    document: Some(document),
                    events,
                },
                task,
            ))
        })
        .unwrap()
}

#[test]
fn a_created_task_has_its_directory_files_and_modes() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (task, write) = new_task("Water the plants", "work", 1);
    store.create(&write).unwrap();

    let task_dir = dir.path().join("work/tasks").join(task.id.as_str());
    let text = fs::read_to_string(task_dir.join("task.md")).unwrap();
    assert_eq!(text, render_task_md(&TaskDocument::new(&task)));
    let events = fs::read_to_string(task_dir.join("events.jsonl")).unwrap();
    assert_eq!(events.lines().count(), 1);
    assert_eq!(mode(&task_dir), TASK_DIR_MODE);
    assert_eq!(mode(&task_dir.join("task.md")), TASK_FILE_MODE);
    assert_eq!(mode(&task_dir.join("events.jsonl")), TASK_FILE_MODE);
    // Nothing else: the temporary directory was renamed into place.
    let entries: Vec<_> = fs::read_dir(dir.path().join("work/tasks"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, [task.id.as_str()]);

    let stored = store.read(&task.id).unwrap();
    assert_eq!(stored.task().unwrap(), task);
    assert_eq!(stored.events.events, write.events);
    assert_eq!(store.list().unwrap(), std::slice::from_ref(&task.id));
    assert_eq!(store.read_events(&task.id).unwrap().events, write.events);
}

#[test]
fn creation_refuses_a_taken_id_another_context_or_no_document() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (task, write) = new_task("Once", "work", 1);
    store.create(&write).unwrap();
    assert!(matches!(
        store.create(&write),
        Err(PortError::AlreadyExists { id, .. }) if id == task.id
    ));
    let (_, other) = new_task("Elsewhere", "home", 2);
    assert!(matches!(
        store.create(&other),
        Err(PortError::WrongContext { found, .. }) if found == context("home")
    ));
    let events_only = TaskWrite {
        document: None,
        events: write.events,
    };
    assert!(matches!(
        store.create(&events_only),
        Err(PortError::MissingDocument)
    ));
}

#[test]
fn an_update_rewrites_task_md_and_appends_its_events() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (task, write) = new_task("Draft", "work", 1);
    store.create(&write).unwrap();
    let edit = Command::Edit(TaskEdit {
        title: Some("Final".to_owned()),
        ..TaskEdit::default()
    });
    let changed = change(&store, &task.id, edit, at(200));
    assert_eq!(changed.title, "Final");

    let stored = store.read(&task.id).unwrap();
    assert_eq!(stored.task().unwrap(), changed);
    assert_eq!(stored.task().unwrap().updated, at(200));
    assert_eq!(stored.events.events.len(), 2);
    assert_eq!(stored.events.skipped, []);
    assert!(stored.file.text.contains("title = \"Final\""));
}

#[test]
fn an_update_without_a_document_leaves_task_md_byte_for_byte() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (task, write) = new_task("Keep me", "work", 1);
    store.create(&write).unwrap();
    let path = dir
        .path()
        .join("work/tasks")
        .join(task.id.as_str())
        .join("task.md");
    // A hand edit with a comment: an append of events must not touch it.
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("+++\n", "+++\n# mine\n");
    fs::write(&path, &edited).unwrap();
    let note = Command::LogNote {
        text: "a note".to_owned(),
    };
    store
        .update(&task.id, |stored| {
            let (_, events) = apply(
                stored.task().unwrap(),
                note,
                &stored.events.events,
                at(300),
                &origin(),
                &mut random(3),
            )
            .unwrap();
            Ok::<_, PortError>((
                TaskWrite {
                    document: None,
                    events,
                },
                (),
            ))
        })
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), edited);
    assert_eq!(store.read_events(&task.id).unwrap().events.len(), 2);
}

#[test]
fn a_missing_task_is_not_found() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let id: TaskId = "k3q7zm2a".parse().unwrap();
    assert!(matches!(store.read(&id), Err(PortError::NotFound { .. })));
    assert!(matches!(
        store.read_events(&id),
        Err(PortError::NotFound { .. })
    ));
    let result = store.update(&id, |_| -> Result<(TaskWrite, ()), PortError> {
        panic!("never called for a missing task")
    });
    assert!(matches!(result, Err(PortError::NotFound { .. })));
    // No `tasks/` yet: no task.
    assert_eq!(store.list().unwrap(), [] as [TaskId; 0]);
}

#[test]
fn a_crash_between_the_temporary_write_and_the_rename_leaves_the_previous_file() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (task, write) = new_task("Before the crash", "work", 1);
    store.create(&write).unwrap();
    let path = dir
        .path()
        .join("work/tasks")
        .join(task.id.as_str())
        .join("task.md");
    let before = fs::read(&path).unwrap();

    // The new content is written and synced to the temporary file, then the process dies
    // before the rename: `commit` never runs.
    let staged = Staged::write(&path, b"+++\ntitle = \"half\"\n", TASK_FILE_MODE).unwrap();
    drop(staged);

    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(store.read(&task.id).unwrap().task().unwrap(), task);
    assert_eq!(store.list().unwrap(), std::slice::from_ref(&task.id));
    // The leftover temporary file is replaced by the next write.
    let edit = Command::Edit(TaskEdit {
        title: Some("After the crash".to_owned()),
        ..TaskEdit::default()
    });
    change(&store, &task.id, edit, at(200));
    let stored = store.read(&task.id).unwrap();
    assert_eq!(stored.task().unwrap().title, "After the crash");
}

#[test]
fn a_crash_during_a_creation_leaves_no_task() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (task, write) = new_task("Half created", "work", 1);
    // A creation stages the whole task in a hidden directory, then renames it into place.
    let staging = dir
        .path()
        .join("work/tasks")
        .join(format!(".{}.tmp", task.id));
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("task.md"), "+++\n").unwrap();
    assert_eq!(store.list().unwrap(), [] as [TaskId; 0]);
    assert!(matches!(
        store.read(&task.id),
        Err(PortError::NotFound { .. })
    ));
    // Created again after the restart: the leftover is replaced.
    store.create(&write).unwrap();
    assert_eq!(store.read(&task.id).unwrap().task().unwrap(), task);
    assert!(!staging.exists());
}

#[test]
fn an_append_after_a_torn_line_keeps_the_new_events_whole() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (task, write) = new_task("Torn", "work", 1);
    store.create(&write).unwrap();
    let events = dir
        .path()
        .join("work/tasks")
        .join(task.id.as_str())
        .join("events.jsonl");
    let mut text = fs::read_to_string(&events).unwrap();
    text.push_str("{\"v\":1,\"id\":\"half");
    fs::write(&events, text).unwrap();

    let note = Command::LogNote {
        text: "after".to_owned(),
    };
    change(&store, &task.id, note, at(200));
    let log = store.read_events(&task.id).unwrap();
    assert_eq!(log.events.len(), 2);
    assert_eq!(log.skipped.len(), 1);
    assert_eq!(log.skipped[0].line, 2);
}

#[test]
fn concurrent_updates_of_one_task_never_interleave() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(store(&dir));
    let (task, write) = new_task("Busy", "work", 1);
    store.create(&write).unwrap();
    let threads: Vec<_> = (0..8_u64)
        .map(|index| {
            let store = Arc::clone(&store);
            let id = task.id.clone();
            thread::spawn(move || {
                let note = Command::LogNote {
                    text: format!("note {index}"),
                };
                change(&store, &id, note, at(1_000 + index));
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let log = store.read_events(&task.id).unwrap();
    // Each update read the log under the lock: every note is there, every event ID distinct.
    assert_eq!(log.events.len(), 9);
    assert_eq!(log.skipped, []);
    let mut ids: Vec<_> = log.events.iter().map(|event| event.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 9);
}

#[test]
fn listing_skips_what_is_not_a_task_directory() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (first, write) = new_task("First", "work", 1);
    store.create(&write).unwrap();
    let (second, write) = new_task("Second", "work", 2);
    store.create(&write).unwrap();
    let tasks = dir.path().join("work/tasks");
    fs::create_dir(tasks.join("notes")).unwrap();
    fs::write(tasks.join("abcdefgh"), "a file, not a directory").unwrap();
    fs::write(tasks.join(".DS_Store"), "").unwrap();
    let mut expected = vec![first.id, second.id];
    expected.sort();
    assert_eq!(store.list().unwrap(), expected);
}

#[test]
fn a_hand_broken_file_reads_as_invalid() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (task, write) = new_task("Fragile", "work", 1);
    store.create(&write).unwrap();
    let path = dir
        .path()
        .join("work/tasks")
        .join(task.id.as_str())
        .join("task.md");
    fs::write(&path, "+++\ntitle = \n+++\n").unwrap();
    let stored = store.read(&task.id).unwrap();
    assert!(matches!(stored.task(), Err(InvalidTask::Format(_))));
    assert_eq!(stored.events.events.len(), 1);
}

#[test]
fn temporary_files_cleanup_removes_loomd_temporary_files_only() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (task, write) = new_task("Clean", "work", 1);
    store.create(&write).unwrap();

    let tasks_root = dir.path().join("work/tasks");
    let task_dir = tasks_root.join(task.id.as_str());

    // Loomd temporary files:
    let task_tmp = task_dir.join(".task.md.tmp");
    fs::write(&task_tmp, "staged write").unwrap();

    let staged_new_task_dir = tasks_root.join(".sykc8yyx.tmp");
    fs::create_dir(&staged_new_task_dir).unwrap();
    fs::write(staged_new_task_dir.join("task.md"), "staged new task").unwrap();

    // User files and non-matching entries that must not be deleted:
    let user_artifact = task_dir.join("artifact.tmp");
    fs::write(&user_artifact, "user artifact").unwrap();

    let user_dot_tmp = task_dir.join(".user.tmp");
    fs::write(&user_dot_tmp, "user dot tmp").unwrap();

    let non_id_dir = tasks_root.join(".not_an_id.tmp");
    fs::create_dir(&non_id_dir).unwrap();

    let non_id_file = tasks_root.join("other.tmp");
    fs::write(&non_id_file, "other").unwrap();

    // Store initialization triggers clean_temporary_files:
    store.init().unwrap();

    // Loomd temporary files are cleaned:
    assert!(!task_tmp.exists());
    assert!(!staged_new_task_dir.exists());

    // User artifacts and non-matching entries are preserved:
    assert!(user_artifact.exists());
    assert!(user_dot_tmp.exists());
    assert!(non_id_dir.exists());
    assert!(non_id_file.exists());
}
