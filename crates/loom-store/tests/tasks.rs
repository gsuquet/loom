//! The tasks index: written with each task write, rebuilt from the files by a reindex, searched
//! by title, notes and link keys. Everything under a temporary directory.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use loom_api::{ContextAssignment, RepoIdentity};
use loom_core::{
    Actor, Change, Command, ContextName, DomainError, Event, ExternalRef, FieldChange, Kind, Link,
    MachineId, NewTask, Origin, ReadOnlyReason, Relations, Status, StatusName, Task, TaskEdit,
    TaskId, Timestamp,
};
use loom_store::files::FileStore;
use loom_store::tasks::{FocusRow, GoOutcome, IndexedState, TaskError, TaskIndex, Tasks};
use loom_store::{Reader, Store};
use rusqlite::Connection;
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

fn random(seed: u64) -> impl FnMut() -> u64 {
    let mut bits = seed;
    move || {
        bits = bits
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        bits
    }
}

fn new_task(title: &str, context_name: &str, links: Vec<Link>) -> NewTask {
    NewTask {
        title: title.to_owned(),
        kind: Kind::Bug,
        context: context(context_name),
        status: Status::Ready,
        importance: None,
        due: None,
        pinned: false,
        links,
        relations: Relations::default(),
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

    fn store_root(&self, name: &str) -> PathBuf {
        self.dir.path().join("contexts").join(name)
    }

    /// The task service over the stores of `work` and `home`, on `index.db`.
    fn tasks(&self) -> Tasks<FileStore> {
        let stores = ["work", "home"]
            .into_iter()
            .map(|name| FileStore::new(context(name), self.store_root(name)))
            .collect();
        Tasks::new(stores, TaskIndex::open(&self.db()).unwrap())
    }

    fn reader(&self) -> Reader {
        Reader::open(&self.db()).unwrap()
    }
}

/// Every column of `tasks` but the internal row key, and its search row, sorted by ID.
fn rows(db: &Path) -> Vec<String> {
    let conn = Connection::open(db).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT t.id, t.context, t.hash, t.task, t.error, t.title, t.status, t.importance, \
                    t.due, t.pinned, t.updated, t.body, t.link_keys, t.archived, \
                    f.title, f.body, f.link_keys \
             FROM tasks t JOIN tasks_fts f ON f.rowid = t.key ORDER BY t.id",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            let values: Vec<String> = (0..17)
                .map(|index| {
                    let value: rusqlite::types::Value = row.get(index)?;
                    Ok(format!("{value:?}"))
                })
                .collect::<rusqlite::Result<_>>()?;
            Ok(values.join("|"))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn search(reader: &Reader, query: &str, context_name: Option<&str>) -> Vec<TaskId> {
    reader
        .search_tasks(query, context_name.map(context).as_ref(), 10)
        .unwrap()
}

#[test]
fn a_created_or_changed_task_is_indexed_at_once() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = tasks
        .create(
            new_task("Login timeout", "work", Vec::new()),
            at(100),
            &origin(),
            &mut random(1),
        )
        .unwrap();
    let reader = fixture.reader();
    let indexed = reader.task(&task.id).unwrap().unwrap();
    assert_eq!(
        indexed.state,
        IndexedState::Valid {
            task: Box::new(task.clone()),
            body: loom_core::body_template(&Kind::Bug).to_owned(),
        }
    );
    // The hash is the hash of the file on disk.
    let file = fixture
        .store_root("work")
        .join("tasks")
        .join(task.id.as_str())
        .join("task.md");
    assert_eq!(
        indexed.hash,
        loom_store::port::ContentHash::of(&fs::read(file).unwrap())
    );

    let edit = Command::Edit(TaskEdit {
        title: Some("Checkout timeout".to_owned()),
        ..TaskEdit::default()
    });
    let changed = tasks
        .apply(&task.id, edit, at(200), &origin(), &mut random(2))
        .unwrap();
    let indexed = reader.task(&task.id).unwrap().unwrap();
    assert_eq!(indexed.title(), "Checkout timeout");
    assert!(matches!(indexed.state, IndexedState::Valid { task, .. } if *task == changed));
    assert_eq!(
        search(&reader, "checkout", None),
        std::slice::from_ref(&task.id)
    );
    assert_eq!(search(&reader, "login", None), [] as [TaskId; 0]);
}

#[test]
fn tasks_are_listed_by_context() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let mut created = Vec::new();
    for (index, name) in ["work", "home", "work"].into_iter().enumerate() {
        let task = tasks
            .create(
                new_task(&format!("Task {index}"), name, Vec::new()),
                at(100),
                &origin(),
                &mut random(10 + index as u64),
            )
            .unwrap();
        created.push(task);
    }
    let reader = fixture.reader();
    let ids = |context_name: Option<&str>| -> Vec<TaskId> {
        reader
            .tasks(context_name.map(context).as_ref())
            .unwrap()
            .into_iter()
            .map(|task| task.id)
            .collect()
    };
    let mut work = vec![created[0].id.clone(), created[2].id.clone()];
    work.sort();
    assert_eq!(ids(Some("work")), work);
    assert_eq!(ids(Some("home")), [created[1].id.clone()]);
    assert_eq!(ids(None).len(), 3);
}

#[test]
fn creation_in_an_unknown_context_is_refused() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let refused = tasks.create(
        new_task("Nowhere", "client", Vec::new()),
        at(100),
        &origin(),
        &mut random(1),
    );
    assert!(matches!(refused, Err(TaskError::UnknownContext(name)) if name == context("client")));
    let missing: TaskId = "k3q7zm2a".parse().unwrap();
    let refused = tasks.apply(&missing, Command::Pin, at(100), &origin(), &mut random(1));
    assert!(matches!(refused, Err(TaskError::NotFound(id)) if id == missing));
}

#[test]
fn search_matches_title_notes_and_link_keys() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let origin_link = Link::Origin(ExternalRef {
        provider: "jira".to_owned(),
        key: "CORE-1042".to_owned(),
        url: None,
    });
    let jira = tasks
        .create(
            new_task("Paiement refusé", "work", vec![origin_link]),
            at(100),
            &origin(),
            &mut random(1),
        )
        .unwrap();
    let other = tasks
        .create(
            new_task("Plants", "home", Vec::new()),
            at(100),
            &origin(),
            &mut random(2),
        )
        .unwrap();
    let reader = fixture.reader();
    assert_eq!(
        search(&reader, "core-1042", None),
        std::slice::from_ref(&jira.id)
    );
    // Diacritics folded, as for repositories.
    assert_eq!(
        search(&reader, "refuse", None),
        std::slice::from_ref(&jira.id)
    );
    // The notes: every bug starts with its template sections.
    let mut both = vec![jira.id, other.id.clone()];
    both.sort();
    let mut found = search(&reader, "symptoms", None);
    found.sort();
    assert_eq!(found, both);
    assert_eq!(search(&reader, "symptoms", Some("home")), [other.id]);
    assert_eq!(search(&reader, "\"", None), [] as [TaskId; 0]);
}

#[test]
fn a_reindex_after_deleting_the_index_yields_the_same_rows() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let first = tasks
        .create(
            new_task("First", "work", Vec::new()),
            at(100),
            &origin(),
            &mut random(1),
        )
        .unwrap();
    tasks
        .create(
            new_task("Second", "home", Vec::new()),
            at(110),
            &origin(),
            &mut random(2),
        )
        .unwrap();
    tasks
        .apply(&first.id, Command::Pin, at(300), &origin(), &mut random(3))
        .unwrap();
    // A file broken by hand is indexed as invalid by both paths.
    let broken = tasks
        .create(
            new_task("Broken", "work", Vec::new()),
            at(120),
            &origin(),
            &mut random(4),
        )
        .unwrap();
    let path = fixture
        .store_root("work")
        .join("tasks")
        .join(broken.id.as_str())
        .join("task.md");
    fs::write(&path, "+++\nbroken\n+++\n").unwrap();
    tasks.reindex().unwrap();
    let before = rows(&fixture.db());
    assert_eq!(before.len(), 3);
    drop(tasks);

    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", fixture.db().display()));
    }
    let mut tasks = fixture.tasks();
    assert_eq!(rows(&fixture.db()), [] as [String; 0]);
    let summary = tasks.reindex().unwrap();
    assert_eq!(summary.rows.len(), 3);
    assert_eq!(rows(&fixture.db()), before);
}

#[test]
fn a_reindex_drops_vanished_tasks_and_leaves_the_repositories() {
    let fixture = Fixture::new();
    let mut store = Store::open(&fixture.db()).unwrap();
    let repo = RepoIdentity {
        path: PathBuf::from("/w/checkout"),
        name: "checkout".to_owned(),
        slug: "work/checkout".to_owned(),
        context: ContextAssignment::Default {
            context: context("work"),
        },
        remotes: Vec::new(),
        primary_remote: None,
        forge: None,
    };
    store.register(repo, at(50), false).unwrap();
    let mut tasks = fixture.tasks();
    let task = tasks
        .create(
            new_task("Gone soon", "work", Vec::new()),
            at(100),
            &origin(),
            &mut random(1),
        )
        .unwrap();
    fs::remove_dir_all(
        fixture
            .store_root("work")
            .join("tasks")
            .join(task.id.as_str()),
    )
    .unwrap();
    assert_eq!(tasks.reindex().unwrap().rows.len(), 0);
    let reader = fixture.reader();
    assert_eq!(reader.task(&task.id).unwrap(), None);
    assert_eq!(store.repos().unwrap().len(), 1);
}

#[test]
fn a_change_of_an_invalid_task_is_refused_and_the_file_kept() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = tasks
        .create(
            new_task("Fragile", "work", Vec::new()),
            at(100),
            &origin(),
            &mut random(1),
        )
        .unwrap();
    let path = fixture
        .store_root("work")
        .join("tasks")
        .join(task.id.as_str())
        .join("task.md");
    fs::write(&path, "+++\nbroken\n+++\n").unwrap();
    let refused = tasks.apply(&task.id, Command::Pin, at(200), &origin(), &mut random(2));
    assert!(matches!(refused, Err(TaskError::Invalid(_))), "{refused:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "+++\nbroken\n+++\n");
}

/// Whether the tests run as root, who reads a file of mode `000` anyway.
fn root(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    fs::metadata(dir).unwrap().uid() == 0
}

#[test]
fn a_reindex_skips_an_unreadable_task_and_keeps_its_row() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new();
    if root(fixture.dir.path()) {
        eprintln!("skipped: root reads a file of mode 000");
        return;
    }
    let mut tasks = fixture.tasks();
    let mut created = Vec::new();
    for (index, title) in ["First", "Locked", "Third"].into_iter().enumerate() {
        let task = tasks
            .create(
                new_task(title, "work", Vec::new()),
                at(100),
                &origin(),
                &mut random(20 + index as u64),
            )
            .unwrap();
        created.push(task);
    }
    let task_md = |task: &Task| {
        fixture
            .store_root("work")
            .join("tasks")
            .join(task.id.as_str())
            .join("task.md")
    };
    let locked = task_md(&created[1]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    // The two readable files are edited: the reindex reads them again.
    for task in [&created[0], &created[2]] {
        let text = fs::read_to_string(task_md(task))
            .unwrap()
            .replace("title = \"", "title = \"New ");
        fs::write(task_md(task), text).unwrap();
    }

    let reindexed = tasks.reindex().unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(reindexed.rows.len(), 2);
    assert_eq!(reindexed.failed.len(), 1);
    assert_eq!(reindexed.failed[0].context, context("work"));
    assert_eq!(reindexed.failed[0].id.as_ref(), Some(&created[1].id));
    assert!(
        reindexed.failed[0].error.contains("task.md"),
        "{}",
        reindexed.failed[0].error
    );
    let reader = fixture.reader();
    let titles: Vec<String> = created
        .iter()
        .map(|task| reader.task(&task.id).unwrap().unwrap().title().to_owned())
        .collect();
    // The unreadable task keeps the row it had.
    assert_eq!(titles, ["New First", "Locked", "New Third"]);
}

#[test]
fn the_repository_and_task_writers_wait_for_each_other() {
    let fixture = Fixture::new();
    let mut store = Store::open(&fixture.db()).unwrap();
    let mut tasks = fixture.tasks();
    // A third connection holds the write lock for a while, as a long scan transaction would:
    // both writers wait for it (`busy_timeout`) instead of failing with `SQLITE_BUSY`.
    let blocker = Connection::open(fixture.db()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        blocker.execute_batch("COMMIT").unwrap();
    });
    let repos = thread::spawn(move || {
        for index in 0..20 {
            let repo = RepoIdentity {
                path: PathBuf::from(format!("/w/repo-{index}")),
                name: format!("repo-{index}"),
                slug: format!("work/repo-{index}"),
                context: ContextAssignment::Default {
                    context: context("work"),
                },
                remotes: Vec::new(),
                primary_remote: None,
                forge: None,
            };
            store.register(repo, at(50), false).unwrap();
        }
        store
    });
    for index in 0..20 {
        tasks
            .create(
                new_task(&format!("Task {index}"), "work", Vec::new()),
                at(100),
                &origin(),
                &mut random(100 + index),
            )
            .unwrap();
    }
    release.join().unwrap();
    let store = repos.join().unwrap();
    assert_eq!(store.repos().unwrap().len(), 20);
    assert_eq!(fixture.reader().tasks(None).unwrap().len(), 20);
}

#[test]
fn undo_reverts_the_last_change_from_the_log_and_indexes_it() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = tasks
        .create(
            new_task("Undo me", "work", Vec::new()),
            at(100),
            &origin(),
            &mut random(1),
        )
        .unwrap();
    let refused = tasks.undo(&task.id, at(150), &origin(), &mut random(2));
    assert!(
        matches!(refused, Err(TaskError::Domain(DomainError::NothingToUndo))),
        "{refused:?}"
    );
    tasks
        .apply(&task.id, Command::Pin, at(200), &origin(), &mut random(3))
        .unwrap();
    let undone = tasks
        .undo(&task.id, at(300), &origin(), &mut random(4))
        .unwrap();
    assert!(!undone.pinned);
    assert_eq!(undone.updated, at(300));
    let indexed = fixture.reader().task(&task.id).unwrap().unwrap();
    assert!(matches!(indexed.state, IndexedState::Valid { task, .. } if *task == undone));
    let log = tasks.events(&task.id).unwrap();
    assert_eq!(log.events.len(), 3);
    assert!(matches!(
        log.events.last().map(|record| &record.event),
        Some(Event::Undone { .. })
    ));
    // Undoing the undo redoes the pin.
    let redone = tasks
        .undo(&task.id, at(400), &origin(), &mut random(5))
        .unwrap();
    assert!(redone.pinned);

    let missing: TaskId = "k3q7zm2a".parse().unwrap();
    assert!(matches!(
        tasks.undo(&missing, at(500), &origin(), &mut random(6)),
        Err(TaskError::NotFound(id)) if id == missing
    ));
    assert!(matches!(
        tasks.events(&missing),
        Err(TaskError::NotFound(id)) if id == missing
    ));
}

#[test]
fn undo_of_an_invalid_task_is_refused() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = tasks
        .create(
            new_task("Fragile", "work", Vec::new()),
            at(100),
            &origin(),
            &mut random(1),
        )
        .unwrap();
    tasks
        .apply(&task.id, Command::Pin, at(200), &origin(), &mut random(2))
        .unwrap();
    let path = fixture
        .store_root("work")
        .join("tasks")
        .join(task.id.as_str())
        .join("task.md");
    fs::write(&path, "+++\nbroken\n+++\n").unwrap();
    let refused = tasks.undo(&task.id, at(300), &origin(), &mut random(3));
    assert!(matches!(refused, Err(TaskError::Invalid(_))), "{refused:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "+++\nbroken\n+++\n");
}

#[test]
fn a_command_records_an_unreconciled_hand_edit_before_its_own_event() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = tasks
        .create(
            new_task("Original title", "work", Vec::new()),
            at(100),
            &origin(),
            &mut random(1),
        )
        .unwrap();

    let path = fixture
        .store_root("work")
        .join("tasks")
        .join(task.id.as_str())
        .join("task.md");
    let original = fs::read_to_string(&path).unwrap();
    let edited = original.replace("title = \"Original title\"", "title = \"Hand title\"");
    fs::write(&path, edited).unwrap();

    // Directly apply a command with no reconciliation in between:
    let updated = tasks
        .apply(
            &task.id,
            Command::LogNote {
                text: "Command note".to_owned(),
            },
            at(200),
            &origin(),
            &mut random(2),
        )
        .unwrap();
    assert_eq!(updated.title, "Hand title");

    let log = tasks.events(&task.id).unwrap();
    assert_eq!(log.events.len(), 3);
    assert!(matches!(log.events[0].event, Event::Created { .. }));

    // The log holds edited (actor file) first:
    assert_eq!(log.events[1].actor, Actor::File);
    assert!(
        matches!(&log.events[1].event, Event::Edited { changes } if changes.iter().any(|c| matches!(c, FieldChange::Title(_))))
    );

    // Then the command's own event (actor user):
    assert_eq!(log.events[2].actor, Actor::User);
    assert!(matches!(&log.events[2].event, Event::NoteLogged { text } if text == "Command note"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn go_activates_the_task_and_keeps_one_focus_per_context() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let first = tasks
        .create(
            new_task("First work task", "work", Vec::new()),
            at(100),
            &origin(),
            &mut random(1),
        )
        .unwrap();
    let second = tasks
        .create(
            new_task("Second work task", "work", Vec::new()),
            at(110),
            &origin(),
            &mut random(2),
        )
        .unwrap();
    let home_task = tasks
        .create(
            new_task("Home task", "home", Vec::new()),
            at(120),
            &origin(),
            &mut random(3),
        )
        .unwrap();

    let outcome = tasks
        .go(&first.id, at(200), &origin(), &mut random(10))
        .unwrap();
    assert_eq!(
        outcome,
        GoOutcome {
            task: Task {
                status: Status::Active,
                updated: at(200),
                ..first.clone()
            },
            switched_from: None,
            since: at(200),
        }
    );
    let log = tasks.events(&first.id).unwrap();
    assert_eq!(
        log.events.last().map(|record| &record.event),
        Some(&Event::StatusChanged(Change {
            from: Status::Ready,
            to: Status::Active,
        }))
    );

    // Going to the already-focused task keeps `since` and writes no new event.
    let again = tasks
        .go(&first.id, at(250), &origin(), &mut random(11))
        .unwrap();
    assert_eq!(again.since, at(200));
    assert_eq!(again.switched_from, None);
    assert_eq!(tasks.events(&first.id).unwrap().events.len(), 2);

    // Focusing a task in another context keeps both contexts focused.
    let home_outcome = tasks
        .go(&home_task.id, at(300), &origin(), &mut random(12))
        .unwrap();
    assert_eq!(home_outcome.task.status, Status::Active);
    assert_eq!(home_outcome.switched_from, None);
    assert_eq!(home_outcome.since, at(300));

    let reader = fixture.reader();
    let focused: Vec<FocusRow> = reader
        .focus(None)
        .unwrap()
        .into_iter()
        .map(|(row, _)| row)
        .collect();
    assert_eq!(
        focused,
        [
            FocusRow {
                context: context("home"),
                task_id: home_task.id.clone(),
                since: at(300),
            },
            FocusRow {
                context: context("work"),
                task_id: first.id.clone(),
                since: at(200),
            },
        ]
    );
    let work_only: Vec<FocusRow> = reader
        .focus(Some(&context("work")))
        .unwrap()
        .into_iter()
        .map(|(row, _)| row)
        .collect();
    assert_eq!(
        work_only,
        [FocusRow {
            context: context("work"),
            task_id: first.id.clone(),
            since: at(200),
        }]
    );

    // Switching focus within `work` demotes `first` back to `ready` and leaves `home` untouched.
    let switched = tasks
        .go(&second.id, at(400), &origin(), &mut random(13))
        .unwrap();
    assert_eq!(switched.task.id, second.id);
    assert_eq!(switched.task.status, Status::Active);
    assert_eq!(switched.since, at(400));
    let demoted = switched.switched_from.unwrap();
    assert_eq!(demoted.id, first.id);
    assert_eq!(demoted.status, Status::Ready);
    assert_eq!(demoted.updated, at(400));
    assert_eq!(
        tasks
            .events(&first.id)
            .unwrap()
            .events
            .last()
            .map(|record| &record.event),
        Some(&Event::StatusChanged(Change {
            from: Status::Active,
            to: Status::Ready,
        }))
    );

    // Reindexing preserves focus for tasks that remain valid and active.
    tasks.reindex().unwrap();
    let focused_after_reindex: Vec<FocusRow> = reader
        .focus(None)
        .unwrap()
        .into_iter()
        .map(|(row, _)| row)
        .collect();
    assert_eq!(
        focused_after_reindex,
        [
            FocusRow {
                context: context("home"),
                task_id: home_task.id,
                since: at(300),
            },
            FocusRow {
                context: context("work"),
                task_id: second.id.clone(),
                since: at(400),
            },
        ]
    );

    // Focus is machine-local in `index.db` and never written to the context store.
    let mut work_entries: Vec<String> = fs::read_dir(fixture.store_root("work"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    work_entries.sort();
    assert_eq!(work_entries, ["archive", "tasks"]);
    let task_md = fs::read_to_string(
        fixture
            .store_root("work")
            .join("tasks")
            .join(second.id.as_str())
            .join("task.md"),
    )
    .unwrap();
    assert!(!task_md.contains("focus"));
    assert!(!task_md.contains("since"));
}

#[test]
fn focus_clears_when_the_focused_task_stops_being_active_or_vanishes() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let first = tasks
        .create(
            new_task("First", "work", Vec::new()),
            at(100),
            &origin(),
            &mut random(1),
        )
        .unwrap();
    let second = tasks
        .create(
            new_task("Second", "work", Vec::new()),
            at(110),
            &origin(),
            &mut random(2),
        )
        .unwrap();
    let reader = fixture.reader();

    tasks
        .go(&first.id, at(200), &origin(), &mut random(3))
        .unwrap();
    assert_eq!(reader.focus(None).unwrap().len(), 1);

    // Completing the focused task clears the focus of its context.
    tasks
        .apply(
            &first.id,
            Command::SetStatus {
                status: Status::Done,
            },
            at(300),
            &origin(),
            &mut random(4),
        )
        .unwrap();
    assert_eq!(reader.focus(None).unwrap(), []);

    // Removing the focused task on disk and reconciling clears its focus row.
    tasks
        .go(&second.id, at(400), &origin(), &mut random(5))
        .unwrap();
    assert_eq!(reader.focus(None).unwrap().len(), 1);
    fs::remove_dir_all(
        fixture
            .store_root("work")
            .join("tasks")
            .join(second.id.as_str()),
    )
    .unwrap();
    tasks
        .reconcile(
            &context("work"),
            &second.id,
            at(500),
            &origin(),
            &mut random(6),
        )
        .unwrap();
    assert_eq!(reader.focus(None).unwrap(), []);
}

#[test]
fn go_refuses_a_closed_or_missing_task_and_keeps_the_previous_focus() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let active = tasks
        .create(
            new_task("Current focus", "work", Vec::new()),
            at(100),
            &origin(),
            &mut random(1),
        )
        .unwrap();
    let closed = tasks
        .create(
            new_task("Already done", "work", Vec::new()),
            at(110),
            &origin(),
            &mut random(2),
        )
        .unwrap();
    tasks
        .apply(
            &closed.id,
            Command::SetStatus {
                status: Status::Done,
            },
            at(150),
            &origin(),
            &mut random(3),
        )
        .unwrap();
    tasks
        .go(&active.id, at(200), &origin(), &mut random(4))
        .unwrap();

    // Refusing a closed task must not demote the current focus.
    let refused = tasks.go(&closed.id, at(300), &origin(), &mut random(5));
    assert!(
        matches!(
            refused,
            Err(TaskError::Domain(DomainError::IllegalTransition {
                from: StatusName::Done,
                to: StatusName::Active,
            }))
        ),
        "{refused:?}"
    );
    let reader = fixture.reader();
    let focused = reader.focus(None).unwrap();
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].0.task_id, active.id);
    assert!(matches!(
        &focused[0].1.state,
        IndexedState::Valid { task, .. } if task.status == Status::Active
    ));

    // Archived task is refused.
    tasks.archive(&closed.id, 2026).unwrap();
    let archived = tasks.go(&closed.id, at(350), &origin(), &mut random(6));
    assert!(
        matches!(archived, Err(TaskError::Archived { year: 2026, .. })),
        "{archived:?}"
    );

    // Missing task is refused.
    let missing: TaskId = "k3q7zm2a".parse().unwrap();
    let not_found = tasks.go(&missing, at(400), &origin(), &mut random(7));
    assert!(
        matches!(&not_found, Err(TaskError::NotFound(id)) if *id == missing),
        "{not_found:?}"
    );
}

#[test]
fn change_refuses_a_read_only_task_up_front() {
    let fixture = Fixture::new();
    let mut tasks = fixture.tasks();
    let task = tasks
        .create(
            new_task("Writable initially", "work", Vec::new()),
            at(100),
            &origin(),
            &mut random(1),
        )
        .unwrap();
    let path = fixture
        .store_root("work")
        .join("tasks")
        .join(task.id.as_str())
        .join("task.md");
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("kind = \"bug\"", "kind = \"future_kind\"");
    fs::write(&path, &edited).unwrap();

    let refused = tasks.apply(&task.id, Command::Pin, at(200), &origin(), &mut random(2));
    assert!(
        matches!(
            refused,
            Err(TaskError::Domain(DomainError::ReadOnly(ReadOnlyReason::UnknownKind(ref name)))) if name == "future_kind"
        ),
        "{refused:?}"
    );
}
