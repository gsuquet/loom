//! The archive end to end: a real `loomd` on a temporary `HOME` and loom home, queried over its
//! unix socket. Archived tasks are read, searched and reindexed, never changed.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{
    ApiError, ErrorCode, ReindexResult, SearchResult, SearchResults, TaskDetail, TaskList,
    TaskReadOnly,
};
use tempfile::TempDir;

mod common;

use common::{Daemon, HttpResponse, POLL, git, git_run, ok, request};

/// Longest wait for loomd to pick up a file change.
const TIMEOUT: Duration = Duration::from_secs(20);

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
        fs::write(
            sandbox.gitconfig(),
            "[user]\n\tname = Store Owner\n\temail = owner@example.invalid\n",
        )
        .unwrap();
        sandbox
    }

    fn loom_home(&self) -> PathBuf {
        self.dir.path().join("l")
    }

    fn user_home(&self) -> PathBuf {
        self.dir.path().join("u")
    }

    fn gitconfig(&self) -> PathBuf {
        self.user_home().join(".gitconfig")
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
            .env("GIT_CONFIG_GLOBAL", self.gitconfig())
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

    fn git(&self, args: &[&str]) -> String {
        git(&self.user_home(), &self.store(), args)
    }

    /// Writes the done task `ID` in `archive/2026/` and returns its directory.
    fn write_archived(&self) -> PathBuf {
        let dir = self.store().join("archive/2026").join(ID);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("task.md"), done_md()).unwrap();
        dir
    }
}

/// IDs of the tasks of `GET path`.
fn listed(sandbox: &Sandbox, path: &str) -> Vec<String> {
    let list: TaskList = ok(&sandbox.call("GET", path));
    list.tasks
        .into_iter()
        .map(|summary| summary.id.to_string())
        .collect()
}

/// Polls `check` until it holds, at most [`TIMEOUT`].
fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(POLL);
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

#[test]
fn a_hand_edit_in_the_archive_is_recorded_and_committed_at_once() {
    let sandbox = Sandbox::new();
    let dir = sandbox.write_archived();
    let daemon = sandbox.start();
    let detail: TaskDetail = ok(&sandbox.call("GET", "/v1/tasks/k3q"));
    assert_eq!(detail.summary.read_only, Some(TaskReadOnly::Archived(2026)));

    // Edited by hand while loomd runs: the watcher covers the archive.
    let text = done_md().replace("Rotate the certificates", "Rotate the old certificates");
    fs::write(dir.join("task.md"), text).unwrap();
    eventually("the edit to be indexed", || {
        let detail: TaskDetail = ok(&sandbox.call("GET", "/v1/tasks/k3q"));
        detail.summary.task.unwrap().title == "Rotate the old certificates"
    });
    // The store with only an archived task is committed at start, then the edit under its
    // archive path, nothing left over.
    eventually("the edit to be committed", || {
        let log = git_run(
            &sandbox.user_home(),
            &sandbox.store(),
            &["log", "--format=%s", "-1"],
        );
        String::from_utf8_lossy(&log.stdout).starts_with(&format!("edited({ID})"))
    });
    let subjects = sandbox.git(&["log", "--format=%s"]);
    assert_eq!(subjects.lines().count(), 2, "{subjects}");
    assert_eq!(sandbox.git(&["status", "--porcelain"]), "");
    let tracked = sandbox.git(&["ls-files"]);
    assert!(
        tracked.contains(&format!("archive/2026/{ID}/task.md")),
        "{tracked}"
    );
    daemon.stop();
}

#[test]
fn an_archived_task_reopened_by_hand_stays_archived_and_out_of_the_lists() {
    let sandbox = Sandbox::new();
    let dir = sandbox.write_archived();
    // Reopened by hand in the archive.
    fs::write(
        dir.join("task.md"),
        done_md().replace("\"done\"", "\"ready\""),
    )
    .unwrap();
    let active = sandbox.store().join("tasks/m0000000");
    fs::create_dir_all(&active).unwrap();
    fs::write(
        active.join("task.md"),
        done_md()
            .replace(ID, "m0000000")
            .replace("\"done\"", "\"ready\""),
    )
    .unwrap();
    let daemon = sandbox.start();

    assert_eq!(listed(&sandbox, "/v1/tasks"), ["m0000000"]);
    assert_eq!(listed(&sandbox, "/v1/tasks?ranked=true"), ["m0000000"]);
    assert_eq!(listed(&sandbox, "/v1/tasks?archived=true"), [ID]);
    assert_eq!(
        listed(&sandbox, "/v1/tasks?archived=true&status=done"),
        [] as [&str; 0]
    );
    let detail: TaskDetail = ok(&sandbox.call("GET", &format!("/v1/tasks/{ID}")));
    assert_eq!(detail.summary.read_only, Some(TaskReadOnly::Archived(2026)));
    let error = problem(
        &sandbox.call("POST", &format!("/v1/tasks/{ID}/actions/done")),
        409,
    );
    assert_eq!(error.code, ErrorCode::ReadOnly);
    daemon.stop();

    // Moved back by hand: active and writable again.
    fs::rename(&dir, sandbox.store().join("tasks").join(ID)).unwrap();
    let daemon = sandbox.start();
    let detail: TaskDetail = ok(&sandbox.call("GET", &format!("/v1/tasks/{ID}")));
    assert_eq!(detail.summary.read_only, None);
    let _: TaskDetail = ok(&sandbox.call("POST", &format!("/v1/tasks/{ID}/actions/done")));
    daemon.stop();
}
