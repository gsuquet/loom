//! The task store of `loomd` end to end: task files under a temporary `HOME` and loom home,
//! the daemon queried over its unix socket, the index read through `loom_store::Reader`.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{ContextName, ReindexResult, TaskId, Timestamp};
use loom_core::{Actor, Kind, MachineId, NewTask, Origin, Relations, Status};
use loom_store::Reader;
use loom_store::files::FileStore;
use loom_store::tasks::{IndexedState, IndexedTask, TaskIndex, Tasks};
use serde_json::{Value, json};
use tempfile::TempDir;

mod common;

use common::{Daemon, HttpResponse, POLL, TIMEOUT, ok, request};

const CONFIG: &str = r#"
[repos]
roots = []

[contexts.work]

[contexts.home]
store.path = "~/stores/home"
"#;

/// A `task.md` of task `id` in `context`, titled `title`.
fn task_md(id: &str, context: &str, title: &str) -> String {
    format!(
        "+++\nid = \"{id}\"\ntitle = \"{title}\"\nkind = \"chore\"\ncontext = \"{context}\"\nstatus = \"ready\"\nimportance = \"p3\"\ncreated = 2026-10-12T09:14:00Z\n+++\n\n## Notes\n"
    )
}

/// A loom home and a user home under `/tmp`: the macOS default `$TMPDIR` is too long for a
/// socket path.
struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("loomd-tasks-")
            .tempdir_in("/tmp")
            .unwrap();
        let sandbox = Self { dir };
        fs::create_dir_all(sandbox.loom_home()).unwrap();
        fs::create_dir_all(sandbox.user_home()).unwrap();
        fs::write(sandbox.loom_home().join("config.toml"), CONFIG).unwrap();
        sandbox
    }

    fn loom_home(&self) -> PathBuf {
        self.dir.path().join("l")
    }

    fn user_home(&self) -> PathBuf {
        self.dir.path().join("u")
    }

    fn socket(&self) -> PathBuf {
        self.loom_home().join("run/loomd.sock")
    }

    fn index(&self) -> PathBuf {
        self.loom_home().join("index.db")
    }

    /// The directory of task `id` in the store of `context`.
    fn task_dir(&self, context: &str, id: &str) -> PathBuf {
        let root = match context {
            "home" => self.user_home().join("stores/home"),
            _ => self.loom_home().join("contexts").join(context),
        };
        root.join("tasks").join(id)
    }

    fn write_task(&self, context: &str, id: &str, text: &str) {
        let dir = self.task_dir(context, id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("task.md"), text).unwrap();
    }

    fn start(&self) -> Daemon {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
        command
            .env("LOOM_HOME", self.loom_home())
            .env("HOME", self.user_home())
            .env("XDG_CONFIG_HOME", self.user_home().join(".config"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("--color=never")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Daemon::start(&mut command, &self.socket())
    }

    fn reindex(&self) -> ReindexResult {
        ok(&self.post("/v1/tasks/reindex"))
    }

    fn post(&self, path: &str) -> HttpResponse {
        request(&self.socket(), "POST", path, None)
    }

    fn indexed(&self) -> Vec<IndexedTask> {
        Reader::open(&self.index()).unwrap().tasks(None).unwrap()
    }
}

fn context(name: &str) -> ContextName {
    name.parse().unwrap()
}

#[test]
fn loomd_indexes_the_task_files_of_every_context_store() {
    let sandbox = Sandbox::new();
    sandbox.write_task("work", "aaaaaaaa", &task_md("aaaaaaaa", "work", "Ship it"));
    sandbox.write_task(
        "home",
        "bbbbbbbb",
        &task_md("bbbbbbbb", "home", "Water the plants"),
    );
    sandbox.write_task("work", "cccccccc", "+++\nbroken\n+++\n");
    let daemon = sandbox.start();

    let result = sandbox.reindex();
    assert_eq!(result.tasks, 3);
    assert_eq!(result.invalid.len(), 1);
    assert_eq!(result.invalid[0].context, context("work"));
    assert_eq!(result.invalid[0].id.as_str(), "cccccccc");
    assert!(
        result.invalid[0].error.starts_with("line 2: invalid TOML"),
        "{}",
        result.invalid[0].error
    );

    let indexed = sandbox.indexed();
    let titles: Vec<(&str, &str, &str)> = indexed
        .iter()
        .map(|task| (task.id.as_str(), task.context.as_str(), task.title()))
        .collect();
    assert_eq!(
        titles,
        [
            ("aaaaaaaa", "work", "Ship it"),
            ("bbbbbbbb", "home", "Water the plants"),
            ("cccccccc", "work", ""),
        ]
    );
    assert!(indexed[2].read_only());
    // The invalid file is left as it is.
    assert_eq!(
        fs::read_to_string(sandbox.task_dir("work", "cccccccc").join("task.md")).unwrap(),
        "+++\nbroken\n+++\n"
    );

    // A task added by hand is picked up by the next reindex, a removed one dropped.
    fs::remove_dir_all(sandbox.task_dir("home", "bbbbbbbb")).unwrap();
    sandbox.write_task(
        "home",
        "dddddddd",
        &task_md("dddddddd", "home", "Call the bank"),
    );
    assert_eq!(sandbox.reindex().tasks, 3);
    let ids: Vec<String> = sandbox
        .indexed()
        .into_iter()
        .map(|task| task.id.to_string())
        .collect();
    assert_eq!(ids, ["aaaaaaaa", "cccccccc", "dddddddd"]);
    daemon.stop();
}

#[test]
fn a_deleted_index_is_rebuilt_with_the_same_rows() {
    let sandbox = Sandbox::new();
    sandbox.write_task("work", "aaaaaaaa", &task_md("aaaaaaaa", "work", "Ship it"));
    sandbox.write_task(
        "home",
        "bbbbbbbb",
        &task_md("bbbbbbbb", "home", "Water the plants"),
    );
    let daemon = sandbox.start();
    sandbox.reindex();
    let before = sandbox.indexed();
    assert_eq!(before.len(), 2);
    assert!(matches!(before[0].state, IndexedState::Valid { .. }));
    daemon.stop();

    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", sandbox.index().display()));
    }
    let daemon = sandbox.start();
    // Queued after the reindex on start: answered once the index is rebuilt.
    sandbox.reindex();
    assert_eq!(sandbox.indexed(), before);
    daemon.stop();
}

/// A task of `work` titled `title`, created through the store as loomd would, with its
/// `created` event.
fn create_task(sandbox: &Sandbox, title: &str) -> TaskId {
    let store = FileStore::new(context("work"), sandbox.loom_home().join("contexts/work"));
    let mut tasks = Tasks::new(vec![store], TaskIndex::open(&sandbox.index()).unwrap());
    let mut bits = 0x5eed_u64;
    let mut random = move || {
        bits = bits
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        bits
    };
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
            Timestamp::from_unix_secs(1_760_000_000).unwrap(),
            &Origin {
                actor: Actor::User,
                machine_id: MachineId("test".to_owned()),
            },
            &mut random,
        )
        .unwrap()
        .id
}

/// Polls the index until `done` holds for the row of `id`, at most `deadline`: slow CI machines
/// get their time, a fast one does not wait. Returns how long it took.
fn wait_for(
    sandbox: &Sandbox,
    id: &TaskId,
    deadline: Duration,
    done: impl Fn(&IndexedTask) -> bool,
) -> Duration {
    let started = Instant::now();
    loop {
        let row = Reader::open(&sandbox.index())
            .ok()
            .and_then(|reader| reader.task(id).ok().flatten());
        if row.as_ref().is_some_and(&done) {
            return started.elapsed();
        }
        assert!(
            started.elapsed() < deadline,
            "not indexed after {deadline:?}: {row:?}"
        );
        thread::sleep(POLL);
    }
}

/// The lines of the event log of `id`, parsed.
fn events(sandbox: &Sandbox, id: &TaskId) -> Vec<Value> {
    fs::read_to_string(sandbox.task_dir("work", id.as_str()).join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Replaces `from` with `to` in the `task.md` of `id`, as an editor would; the new text.
fn hand_edit(sandbox: &Sandbox, id: &TaskId, from: &str, to: &str) -> String {
    let path = sandbox.task_dir("work", id.as_str()).join("task.md");
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains(from), "{text}");
    let edited = text.replace(from, to);
    fs::write(&path, &edited).unwrap();
    edited
}

#[test]
fn a_hand_edit_of_the_title_is_indexed_and_recorded_as_an_event() {
    let sandbox = Sandbox::new();
    let id = create_task(&sandbox, "Before");
    let daemon = sandbox.start();
    // The reconciliation on start finds the file loomd indexed: nothing recorded.
    sandbox.reindex();
    assert_eq!(events(&sandbox, &id).len(), 1);

    hand_edit(&sandbox, &id, "title = \"Before\"", "title = \"After\"");
    let took = wait_for(&sandbox, &id, TIMEOUT, |row| row.title() == "After");
    // The target is 2 s; the deadline is longer so that a loaded CI runner does not fail.
    eprintln!("hand edit indexed after {took:?}");
    let log = events(&sandbox, &id);
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(log[1]["type"], "edited");
    assert_eq!(log[1]["actor"], json!({ "type": "file" }));
    assert_eq!(
        log[1]["changes"],
        json!([{ "field": "title", "from": "Before", "to": "After" }])
    );
    daemon.stop();
}

#[test]
fn an_invalid_hand_edit_is_flagged_read_only_and_never_rewritten() {
    let sandbox = Sandbox::new();
    let id = create_task(&sandbox, "Fragile");
    let daemon = sandbox.start();
    sandbox.reindex();

    let broken = hand_edit(&sandbox, &id, "title = \"Fragile\"", "title = ");
    // The whole edit, not the empty file an editor may leave for an instant while it writes.
    wait_for(
        &sandbox,
        &id,
        TIMEOUT,
        |row| matches!(&row.state, IndexedState::Invalid { text, .. } if *text == broken),
    );
    let row = Reader::open(&sandbox.index())
        .unwrap()
        .task(&id)
        .unwrap()
        .unwrap();
    assert!(row.read_only());
    assert!(
        matches!(row.state, IndexedState::Invalid { ref error, .. } if error.starts_with("line 3: invalid TOML")),
        "{row:?}"
    );
    assert_eq!(
        fs::read_to_string(sandbox.task_dir("work", id.as_str()).join("task.md")).unwrap(),
        broken
    );
    assert_eq!(events(&sandbox, &id).len(), 1);
    daemon.stop();
}

#[test]
fn a_hand_edit_made_while_loomd_was_stopped_is_recorded_at_start() {
    let sandbox = Sandbox::new();
    let id = create_task(&sandbox, "Offline");
    hand_edit(&sandbox, &id, "status = \"ready\"", "status = \"done\"");
    let daemon = sandbox.start();
    // Queued after the reconciliation on start.
    sandbox.reindex();
    let log = events(&sandbox, &id);
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(log[1]["actor"], json!({ "type": "file" }));
    assert_eq!(log[1]["changes"][0]["field"], "status");
    daemon.stop();
}

#[test]
fn an_unreadable_task_is_reported_and_the_others_indexed() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let sandbox = Sandbox::new();
    if fs::metadata(sandbox.loom_home()).unwrap().uid() == 0 {
        eprintln!("skipped: root reads a file of mode 000");
        return;
    }
    sandbox.write_task("work", "aaaaaaaa", &task_md("aaaaaaaa", "work", "Ship it"));
    sandbox.write_task("work", "bbbbbbbb", &task_md("bbbbbbbb", "work", "Locked"));
    let locked = sandbox.task_dir("work", "bbbbbbbb").join("task.md");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let daemon = sandbox.start();

    let result = sandbox.reindex();
    // Read the index before restoring the mode: the watcher reindexes the task as soon as it
    // sees the change (inotify reports it at once on Linux).
    let ids: Vec<String> = sandbox
        .indexed()
        .into_iter()
        .map(|task| task.id.to_string())
        .collect();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(result.tasks, 1);
    assert_eq!(result.failed.len(), 1, "{result:?}");
    assert!(
        result.failed[0].path.ends_with("tasks/bbbbbbbb"),
        "{result:?}"
    );
    assert_eq!(ids, ["aaaaaaaa"]);
    daemon.stop();
}

#[test]
fn the_reindex_endpoint_takes_post_only() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let response = request(&sandbox.socket(), "GET", "/v1/tasks/reindex", None);
    assert_eq!(response.status, 405, "{}", response.body);
    daemon.stop();
}
