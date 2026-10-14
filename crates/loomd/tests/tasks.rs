//! The task store of `loomd` end to end: task files under a temporary `HOME` and loom home,
//! the daemon queried over its unix socket, the index read through `loom_store::Reader`.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use loom_api::{ContextName, ReindexResult};
use loom_store::Reader;
use loom_store::tasks::{IndexedState, IndexedTask};
use tempfile::TempDir;

mod common;

use common::{Daemon, HttpResponse, ok, request};

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
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(result.tasks, 1);
    assert_eq!(result.failed.len(), 1, "{result:?}");
    assert!(
        result.failed[0].path.ends_with("tasks/bbbbbbbb"),
        "{result:?}"
    );
    let ids: Vec<String> = sandbox
        .indexed()
        .into_iter()
        .map(|task| task.id.to_string())
        .collect();
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
