//! The archive end to end: a real `loomd` on a temporary `HOME` and loom home, queried over its
//! unix socket. Archived tasks are read, searched and reindexed, never changed.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use loom_api::{
    ApiError, ErrorCode, ReindexResult, SearchResult, SearchResults, TaskDetail, TaskReadOnly,
};
use tempfile::TempDir;

mod common;

use common::{Daemon, HttpResponse, ok, request};

const CONFIG: &str = r"
[repos]
roots = []

[contexts.work]
";

const ID: &str = "k3q7zm2a";

/// A done `task.md` of task `ID` in `work`.
fn done_md() -> String {
    format!(
        "+++\nid = \"{ID}\"\ntitle = \"Rotate the certificates\"\nkind = \"chore\"\ncontext = \"work\"\nstatus = \"done\"\nimportance = \"p3\"\ncreated = 2026-10-12T09:14:00Z\n+++\n\n## Notes\n"
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
            .prefix("loomd-archive-")
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

    fn store(&self) -> PathBuf {
        self.loom_home().join("contexts/work")
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

    fn call(&self, method: &str, path: &str) -> HttpResponse {
        request(&self.socket(), method, path, None)
    }
}

/// The error of `response`, which must have `status`.
fn problem(response: &HttpResponse, status: u16) -> ApiError {
    assert_eq!(response.status, status, "{}", response.body);
    serde_json::from_str(&response.body).unwrap()
}

/// IDs of the tasks a search for `query` finds.
fn found(sandbox: &Sandbox, query: &str) -> Vec<String> {
    let results: SearchResults = ok(&sandbox.call("GET", &format!("/v1/search?q={query}")));
    results
        .results
        .into_iter()
        .filter_map(|result| match result {
            SearchResult::Task(task) => Some(task.id.to_string()),
            SearchResult::Repo(_) => None,
        })
        .collect()
}

#[test]
fn a_task_moved_to_the_archive_while_loomd_was_stopped_is_archived_and_read_only() {
    let sandbox = Sandbox::new();
    let active = sandbox.store().join("tasks").join(ID);
    fs::create_dir_all(&active).unwrap();
    fs::write(active.join("task.md"), done_md()).unwrap();
    let daemon = sandbox.start();
    let detail: TaskDetail = ok(&sandbox.call("GET", &format!("/v1/tasks/{ID}")));
    assert_eq!(detail.summary.read_only, None);
    daemon.stop();

    // As a crash between the move and the index write leaves it.
    let archived = sandbox.store().join("archive/2026").join(ID);
    fs::create_dir_all(archived.parent().unwrap()).unwrap();
    fs::rename(&active, &archived).unwrap();
    let before = fs::read(archived.join("task.md")).unwrap();
    let daemon = sandbox.start();

    let detail: TaskDetail = ok(&sandbox.call("GET", "/v1/tasks/k3q"));
    assert_eq!(detail.summary.read_only, Some(TaskReadOnly::Archived(2026)));
    assert_eq!(detail.actions, []);
    assert_eq!(
        detail.summary.task.unwrap().title,
        "Rotate the certificates"
    );
    assert_eq!(found(&sandbox, "certificates"), [ID]);

    // Reopening, or any other change, is refused with the reason.
    for (method, path) in [
        ("POST", format!("/v1/tasks/{ID}/actions/ready")),
        ("POST", format!("/v1/tasks/{ID}/actions/pin")),
        ("POST", format!("/v1/tasks/{ID}/undo")),
    ] {
        let error = problem(&sandbox.call(method, &path), 409);
        assert_eq!(error.code, ErrorCode::ReadOnly, "{path}");
        assert!(
            error.message.contains("is archived (archive/2026)"),
            "{}",
            error.message
        );
        let details: TaskReadOnly = serde_json::from_value(error.details.unwrap()).unwrap();
        assert_eq!(details, TaskReadOnly::Archived(2026));
    }
    let response = request(
        &sandbox.socket(),
        "PATCH",
        &format!("/v1/tasks/{ID}"),
        Some(&serde_json::json!({ "title": "Renamed" })),
    );
    assert_eq!(problem(&response, 409).code, ErrorCode::ReadOnly);
    assert_eq!(fs::read(archived.join("task.md")).unwrap(), before);
    assert!(!active.exists());

    // A reindex gives the same answer.
    let reindexed: ReindexResult = ok(&sandbox.call("POST", "/v1/tasks/reindex"));
    assert_eq!(reindexed.tasks, 1);
    let again: TaskDetail = ok(&sandbox.call("GET", "/v1/tasks/k3q"));
    assert_eq!(again.summary.read_only, Some(TaskReadOnly::Archived(2026)));
    daemon.stop();
}
