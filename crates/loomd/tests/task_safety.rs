//! The task store safety suite: crashes during a write, invalid or concurrent hand edits,
//! full reindex from files, forward-compatible files from a newer version, global or local
//! `commit.gpgsign=true`, and rejected backup pushes never lose task data (ADR-0013, #83).

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

mod common;

use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::{Daemon, HttpResponse, POLL, TIMEOUT, git, git_run, ok, request};
use loom_api::{
    ApiError, BackupOutcome, BackupReport, ErrorCode, Event, ReindexResult, StatusName, TaskDetail,
    TaskEvents, TaskList, TaskReadOnly,
};
use loom_store::Reader;
use loom_store::files::{CrashStage, TEST_CRASH_STAGE_ENV};
use loom_store::tasks::IndexedTask;
use serde_json::{Value, json};
use tempfile::TempDir;

const CONFIG: &str = r#"
[repos]
roots = []

[contexts.work]

[contexts.home]
store.path = "~/stores/home"
"#;

const TODAY: &str = "2026-10-14";

/// A loom home and a user home under `/tmp`: the macOS default `$TMPDIR` is too long for a
/// socket path.
struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self::with_config(CONFIG)
    }

    fn with_config(config: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("loomd-task-safety-")
            .tempdir_in("/tmp")
            .unwrap();
        let sandbox = Self { dir };
        fs::create_dir_all(sandbox.loom_home()).unwrap();
        fs::create_dir_all(sandbox.user_home()).unwrap();
        fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
        fs::write(
            sandbox.gitconfig(),
            "[user]\n\tname = Store Owner\n\temail = owner@example.invalid\n",
        )
        .unwrap();
        sandbox
    }

    fn with_remote() -> Self {
        let sandbox = Self::new();
        let remote = sandbox.remote();
        git(
            &sandbox.user_home(),
            sandbox.dir.path(),
            &["init", "-q", "--bare", remote.to_str().unwrap()],
        );
        fs::write(
            sandbox.loom_home().join("config.toml"),
            format!(
                "[repos]\nroots = []\n\n[contexts.work]\nstore.backup.remote = \"{}\"\n",
                remote.display()
            ),
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

    fn remote(&self) -> PathBuf {
        self.dir.path().join("remote.git")
    }

    fn socket(&self) -> PathBuf {
        self.loom_home().join("run/loomd.sock")
    }

    fn index(&self) -> PathBuf {
        self.loom_home().join("index.db")
    }

    fn store(&self, context: &str) -> PathBuf {
        match context {
            "home" => self.user_home().join("stores/home"),
            _ => self.loom_home().join("contexts").join(context),
        }
    }

    fn task_dir(&self, context: &str, id: &str) -> PathBuf {
        self.store(context).join("tasks").join(id)
    }

    fn write_task(&self, context: &str, id: &str, text: &str) {
        let dir = self.task_dir(context, id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("task.md"), text).unwrap();
    }

    fn env(&self) -> Vec<(OsString, OsString)> {
        vec![
            ("HOME".into(), self.user_home().into()),
            (
                "XDG_CONFIG_HOME".into(),
                self.user_home().join(".config").into(),
            ),
            ("GIT_CONFIG_GLOBAL".into(), self.gitconfig().into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("LOOM_TEST_TODAY".into(), TODAY.into()),
        ]
    }

    fn daemon_command(&self, extra_env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
        command
            .env("LOOM_HOME", self.loom_home())
            .env("HOME", self.user_home())
            .arg("--color=never")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (name, value) in self.env() {
            command.env(name, value);
        }
        for (name, value) in extra_env {
            command.env(name, value);
        }
        command
    }

    fn start(&self) -> Daemon {
        self.start_with(&[])
    }

    fn start_with(&self, extra_env: &[(&str, &str)]) -> Daemon {
        let mut command = self.daemon_command(extra_env);
        Daemon::start(&mut command, &self.socket())
    }

    /// Starts `loomd` as a raw [`Child`] when a test expects it to be killed by `SIGKILL`.
    fn spawn_crashable(&self, stage: &str) -> Child {
        let mut command = self.daemon_command(&[(TEST_CRASH_STAGE_ENV, stage)]);
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + TIMEOUT;
        while UnixStream::connect(self.socket()).is_err() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("loomd exited early with {status}");
            }
            assert!(Instant::now() < deadline, "socket never appeared");
            thread::sleep(POLL);
        }
        child
    }

    fn reindex(&self) -> ReindexResult {
        ok(&request(&self.socket(), "POST", "/v1/tasks/reindex", None))
    }

    fn indexed(&self) -> Vec<IndexedTask> {
        Reader::open(&self.index()).unwrap().tasks(None).unwrap()
    }

    fn git_in(&self, context: &str, args: &[&str]) -> String {
        git(&self.user_home(), &self.store(context), args)
    }

    fn subjects_in(&self, context: &str) -> Vec<String> {
        let output = git_run(
            &self.user_home(),
            &self.store(context),
            &["log", "--reverse", "--format=%s"],
        );
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn wait_for_commits(&self, context: &str, count: usize) -> Vec<String> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let subjects = self.subjects_in(context);
            if subjects.len() >= count {
                return subjects;
            }
            assert!(
                Instant::now() < deadline,
                "commits in {context}: {subjects:?}"
            );
            thread::sleep(POLL);
        }
    }

    /// Asserts that the store of `context` has no leftover `.tmp` entries and a clean git tree
    /// with at least `min_commits` commits.
    fn assert_store_clean(&self, context: &str, min_commits: usize) {
        let root = self.store(context);
        assert_eq!(
            find_tmp_entries(&root),
            Vec::<PathBuf>::new(),
            "leftover .tmp entries in {}",
            root.display()
        );
        let subjects = self.wait_for_commits(context, min_commits);
        assert!(
            subjects.len() >= min_commits,
            "expected at least {min_commits} commits in {context}, found {subjects:?}"
        );
        assert_eq!(
            self.git_in(context, &["status", "--porcelain"]),
            "",
            "dirty working tree in {context}"
        );
    }
}

/// Recursively collects any file or directory under `dir` whose name ends with `.tmp`
/// (excluding `.git`).
fn find_tmp_entries(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" {
            continue;
        }
        let path = entry.path();
        if name.ends_with(".tmp") {
            found.push(path.clone());
        }
        if path.is_dir() {
            found.extend(find_tmp_entries(&path));
        }
    }
    found.sort();
    found
}

fn create(socket: &Path, body: &Value) -> TaskDetail {
    ok(&request(socket, "POST", "/v1/tasks", Some(body)))
}

fn problem(response: &HttpResponse, status: u16) -> ApiError {
    assert_eq!(response.status, status, "{}", response.body);
    let value: Value = serde_json::from_str(&response.body).unwrap();
    let schema = serde_json::to_value(schemars::schema_for!(ApiError)).unwrap();
    assert!(jsonschema::is_valid(&schema, &value), "{value}");
    serde_json::from_value(value).unwrap()
}

/// Sends a raw HTTP request that is expected to break its connection when `loomd` crashes mid-write.
fn send_expecting_crash(socket: &Path, method: &str, path: &str, body: &Value, child: &mut Child) {
    let body = body.to_string();
    let mut stream = UnixStream::connect(socket).unwrap();
    let _ = write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: loomd\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                !status.success(),
                "expected loomd to be killed during write, exited with {status}"
            );
            return;
        }
        assert!(Instant::now() < deadline, "loomd did not crash in time");
        thread::sleep(POLL);
    }
}

/// Scenario 1: Kill `loomd` (`SIGKILL`) during a task write at each stage (`after_task_stage`,
/// `after_task_rename`, `after_events_append`), then restart `loomd` on the same home and verify
/// that task files, `index.db`, and git history heal without losing data or leaving `.tmp` files.
#[test]
fn loomd_killed_during_a_write_heals_the_store_on_restart() {
    for stage in [
        CrashStage::AfterTaskStage,
        CrashStage::AfterTaskRename,
        CrashStage::AfterEventsAppend,
    ] {
        let sandbox = Sandbox::new();
        let daemon = sandbox.start();
        let created = create(
            &sandbox.socket(),
            &json!({
                "title": "Before crash",
                "kind": "chore",
                "context": "work",
                "status": "ready",
            }),
        );
        let id = created.summary.id;
        sandbox.assert_store_clean("work", 1);
        daemon.stop();

        // Start loomd with the crash stage armed, then trigger a task write.
        let mut crashing = sandbox.spawn_crashable(stage.as_str());
        // Wait for startup reconciliation on the unchanged store to finish before writing.
        let listed: TaskList = ok(&request(&sandbox.socket(), "GET", "/v1/tasks", None));
        assert_eq!(listed.tasks.len(), 1);

        send_expecting_crash(
            &sandbox.socket(),
            "PATCH",
            &format!("/v1/tasks/{id}"),
            &json!({ "title": "After crash" }),
            &mut crashing,
        );

        let task_dir = sandbox.task_dir("work", id.as_str());
        let on_disk = fs::read_to_string(task_dir.join("task.md")).unwrap();
        match stage {
            CrashStage::AfterTaskStage => {
                // The rename did not happen: `task.md` still holds the previous title, and
                // `.task.md.tmp` is ignored by git.
                assert!(on_disk.contains("title = \"Before crash\""), "{on_disk}");
                assert!(task_dir.join(".task.md.tmp").exists());
                assert_eq!(sandbox.git_in("work", &["status", "--porcelain"]), "");
            }
            CrashStage::AfterTaskRename => {
                // `task.md` has the new title, no `.tmp` file remains, and `events.jsonl` still
                // has only `created` until startup reconciliation heals it.
                assert!(on_disk.contains("title = \"After crash\""), "{on_disk}");
                assert!(!task_dir.join(".task.md.tmp").exists());
                let lines = fs::read_to_string(task_dir.join("events.jsonl")).unwrap();
                assert_eq!(lines.lines().count(), 1, "{lines}");
            }
            CrashStage::AfterEventsAppend => {
                // Both `task.md` and `events.jsonl` have the change on disk, not yet indexed or
                // committed.
                assert!(on_disk.contains("title = \"After crash\""), "{on_disk}");
                assert!(!task_dir.join(".task.md.tmp").exists());
                let lines = fs::read_to_string(task_dir.join("events.jsonl")).unwrap();
                assert_eq!(lines.lines().count(), 2, "{lines}");
            }
            _ => unreachable!(),
        }

        // Restart loomd normally on the same home.
        let daemon = sandbox.start();
        sandbox.reindex();

        if stage == CrashStage::AfterTaskStage {
            // Retry the interrupted edit: it replaces `.task.md.tmp` and commits it.
            let updated: TaskDetail = ok(&request(
                &sandbox.socket(),
                "PATCH",
                &format!("/v1/tasks/{id}"),
                Some(&json!({ "title": "After crash" })),
            ));
            assert_eq!(updated.summary.task.unwrap().title, "After crash");
        }

        let detail: TaskDetail = ok(&request(
            &sandbox.socket(),
            "GET",
            &format!("/v1/tasks/{id}"),
            None,
        ));
        assert_eq!(
            detail.summary.task.unwrap().title,
            "After crash",
            "stage: {stage:?}"
        );
        let events: TaskEvents = ok(&request(
            &sandbox.socket(),
            "GET",
            &format!("/v1/tasks/{id}/events"),
            None,
        ));
        assert_eq!(events.events.len(), 2, "stage {stage:?}: {events:?}");
        assert_eq!(events.skipped, []);
        let replayed = loom_core::replay(&events.events).unwrap();
        assert_eq!(replayed.title, "After crash");

        let final_md = fs::read_to_string(task_dir.join("task.md")).unwrap();
        assert!(final_md.contains("title = \"After crash\""), "{final_md}");
        sandbox.assert_store_clean("work", 2);
        daemon.stop();
    }
}

/// Kill `loomd` (`SIGKILL`) during task creation (`after_create_stage`), then verify that on
/// restart no partial task exists, the temporary staging folder is ignored, and a new creation
/// succeeds.
#[test]
fn loomd_killed_during_creation_leaves_no_broken_task() {
    let sandbox = Sandbox::new();
    let mut crashing = sandbox.spawn_crashable(CrashStage::AfterCreateStage.as_str());

    send_expecting_crash(
        &sandbox.socket(),
        "POST",
        "/v1/tasks",
        &json!({
            "title": "Aborted creation",
            "kind": "chore",
            "context": "work",
            "status": "ready",
        }),
        &mut crashing,
    );

    assert_eq!(sandbox.indexed().len(), 0);
    if sandbox.store("work").join(".git").exists() {
        assert_eq!(sandbox.git_in("work", &["status", "--porcelain"]), "");
    }

    let daemon = sandbox.start();
    let reindexed = sandbox.reindex();
    assert_eq!(reindexed.tasks, 0);
    assert_eq!(reindexed.invalid, []);

    let created = create(
        &sandbox.socket(),
        &json!({
            "title": "Successful creation",
            "kind": "chore",
            "context": "work",
            "status": "ready",
        }),
    );
    assert_eq!(created.summary.task.unwrap().title, "Successful creation");
    sandbox.assert_store_clean("work", 1);
    daemon.stop();
}

/// Kill `loomd` (`SIGKILL`) during archive moves (`before_archive_rename`, `after_archive_rename`),
/// then restart `loomd` on the same home and verify the task store heals and reconciles.
#[test]
fn loomd_killed_during_archiving_heals_on_restart() {
    for stage in [
        CrashStage::BeforeArchiveRename,
        CrashStage::AfterArchiveRename,
    ] {
        let sandbox = Sandbox::new();
        let daemon = sandbox.start();
        let created = create(
            &sandbox.socket(),
            &json!({
                "title": "Task to archive",
                "kind": "chore",
                "context": "work",
                "status": "ready",
            }),
        );
        let id = created.summary.id;
        let _: TaskDetail = ok(&request(
            &sandbox.socket(),
            "POST",
            &format!("/v1/tasks/{id}/actions/done"),
            None,
        ));
        let events: TaskEvents = ok(&request(
            &sandbox.socket(),
            "GET",
            &format!("/v1/tasks/{id}/events"),
            None,
        ));
        let closed = events.events.last().unwrap().at;
        let year: u16 = closed.to_string()[..4].parse().unwrap();
        daemon.stop();

        // 8 days later, archiving is due. Spawn daemon with crash armed.
        let day: jiff::civil::Date = closed.to_string()[..10].parse().unwrap();
        let day_8 = day
            .checked_add(jiff::Span::new().days(8))
            .unwrap()
            .to_string();

        let mut command = sandbox.daemon_command(&[
            ("LOOM_TEST_TODAY", &day_8),
            (TEST_CRASH_STAGE_ENV, stage.as_str()),
        ]);
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(
                    !status.success(),
                    "expected loomd to be killed on archive crash"
                );
                break;
            }
            assert!(Instant::now() < deadline, "loomd did not crash on archive");
            thread::sleep(POLL);
        }

        // Restart loomd on day 8 without crash hook: archiver / reindex runs.
        let daemon = sandbox.start_with(&[("LOOM_TEST_TODAY", &day_8)]);
        let detail: TaskDetail = ok(&request(
            &sandbox.socket(),
            "GET",
            &format!("/v1/tasks/{id}"),
            None,
        ));
        assert_eq!(detail.summary.read_only, Some(TaskReadOnly::Archived(year)));
        let archived_path = sandbox
            .store("work")
            .join("archive")
            .join(format!("{year:04}"))
            .join(id.as_str())
            .join("task.md");
        assert!(archived_path.is_file());
        sandbox.assert_store_clean("work", 3);
        daemon.stop();
    }
}

/// Scenario 2: An invalid hand edit marks the task read-only, refuses every API mutation without
/// overwriting the user's file, commits the hand edit to git, and restores writability once fixed.
#[test]
fn an_invalid_hand_edit_is_read_only_never_overwritten_and_heals_when_fixed() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let created = create(
        &sandbox.socket(),
        &json!({
            "title": "Fragile task",
            "kind": "chore",
            "context": "work",
            "status": "ready",
        }),
    );
    let id = created.summary.id;
    sandbox.assert_store_clean("work", 1);

    let task_md_path = sandbox.task_dir("work", id.as_str()).join("task.md");
    let original = fs::read_to_string(&task_md_path).unwrap();
    let broken = original.replace("title = \"Fragile task\"", "title = [unclosed\n# user note");
    fs::write(&task_md_path, &broken).unwrap();

    // Wait for the watcher / reindex to index the invalid file and commit it.
    let reindexed = sandbox.reindex();
    assert_eq!(reindexed.tasks, 1);
    assert_eq!(reindexed.invalid.len(), 1);
    assert_eq!(reindexed.invalid[0].id, id);

    let detail: TaskDetail = ok(&request(
        &sandbox.socket(),
        "GET",
        &format!("/v1/tasks/{id}"),
        None,
    ));
    assert!(detail.summary.task.is_none());
    assert!(matches!(
        detail.summary.read_only,
        Some(TaskReadOnly::InvalidFile(_))
    ));
    assert_eq!(detail.body, broken);

    // Every API mutation is refused with 409 ReadOnly and never touches the broken file.
    for (method, path, body) in [
        (
            "PATCH",
            format!("/v1/tasks/{id}"),
            Some(json!({ "title": "Overwrite attempt" })),
        ),
        ("POST", format!("/v1/tasks/{id}/actions/done"), None),
        (
            "POST",
            format!("/v1/tasks/{id}/actions/log_note"),
            Some(json!({ "text": "Should not write" })),
        ),
        ("POST", format!("/v1/tasks/{id}/undo"), None),
    ] {
        let err = problem(
            &request(&sandbox.socket(), method, &path, body.as_ref()),
            409,
        );
        assert_eq!(err.code, ErrorCode::ReadOnly);
        let ro: TaskReadOnly = serde_json::from_value(err.details.unwrap()).unwrap();
        assert!(matches!(ro, TaskReadOnly::InvalidFile(_)));
    }
    assert_eq!(fs::read_to_string(&task_md_path).unwrap(), broken);
    sandbox.assert_store_clean("work", 2);

    // Fix the TOML by hand: the task becomes valid and writable again.
    let fixed = original.replace("title = \"Fragile task\"", "title = \"Healed by hand\"");
    fs::write(&task_md_path, &fixed).unwrap();
    let reindexed = sandbox.reindex();
    assert_eq!(reindexed.invalid, []);

    let done: TaskDetail = ok(&request(
        &sandbox.socket(),
        "POST",
        &format!("/v1/tasks/{id}/actions/done"),
        None,
    ));
    let task = done.summary.task.unwrap();
    assert_eq!(task.title, "Healed by hand");
    assert_eq!(task.status.name(), Some(StatusName::Done));

    let subjects = sandbox.wait_for_commits("work", 4);
    assert_eq!(
        subjects.first(),
        Some(&format!("created({id}): Fragile task"))
    );
    assert_eq!(
        subjects.last(),
        Some(&format!("status_changed({id}): ready -> done"))
    );
    let intermediate = &subjects[1..subjects.len() - 1];
    assert!(
        intermediate.contains(&format!("edited({id}): title")),
        "missing title edit in {subjects:?}"
    );
    for s in intermediate {
        assert!(
            s == &format!("edited({id}): title")
                || s == &format!("edited({id}): changed by hand")
                || s == "sync(work): uncommitted changes",
            "unexpected commit subject in {subjects:?}: {s}"
        );
    }
    sandbox.assert_store_clean("work", 4);
    daemon.stop();
}

fn spawn_hand_edits(id: loom_api::TaskId, path: PathBuf, rounds: usize) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for round in 0..rounds {
            let content = format!(
                "+++\nid = \"{id}\"\ntitle = \"Hand title {round}\"\nkind = \"chore\"\ncontext = \"work\"\nstatus = \"ready\"\nimportance = \"p1\"\ncreated = 2026-10-12T09:14:00Z\n+++\n\n## Notes\n\nHand note {round}\n"
            );
            let tmp = path.with_file_name(format!(".hand-{round}.md"));
            fs::write(&tmp, &content).unwrap();
            fs::rename(&tmp, &path).unwrap();
            thread::sleep(Duration::from_millis(5));
        }
    })
}

/// Scenario 3: Concurrent hand edits of `task.md` and API mutations (`log_note`, `PATCH`) lose
/// neither the API events nor the hand edits, leave no `.tmp` files, and leave a clean git history.
#[test]
fn concurrent_hand_edits_and_api_mutations_lose_neither_and_leave_a_clean_store() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let created = create(
        &sandbox.socket(),
        &json!({
            "title": "Concurrent target",
            "kind": "chore",
            "context": "work",
            "status": "ready",
        }),
    );
    let id = created.summary.id;
    let socket = sandbox.socket();
    let task_md_path = sandbox.task_dir("work", id.as_str()).join("task.md");

    let api_threads: Vec<_> = (0..8)
        .map(|i| {
            let socket = socket.clone();
            let id = id.clone();
            thread::spawn(move || {
                let response = request(
                    &socket,
                    "POST",
                    &format!("/v1/tasks/{id}/actions/log_note"),
                    Some(&json!({ "text": format!("api-note-{i}") })),
                );
                assert_eq!(response.status, 200, "{}", response.body);
            })
        })
        .collect();

    let hand_thread = spawn_hand_edits(id.clone(), task_md_path.clone(), 6);

    for handle in api_threads {
        handle.join().unwrap();
    }
    hand_thread.join().unwrap();

    // Reconcile hand edits and API updates.
    let reindexed = sandbox.reindex();
    assert_eq!(reindexed.invalid, []);

    // 1. Verify that hand edits survived in the markdown file on disk:
    let raw_md = fs::read_to_string(&task_md_path).unwrap();
    assert!(
        raw_md.contains("Hand note"),
        "hand note missing from task.md: {raw_md}"
    );
    assert!(
        raw_md.contains("Hand title"),
        "hand title missing from task.md: {raw_md}"
    );

    // 2. Verify all API notes were logged and recorded in events.jsonl:
    let events: TaskEvents = ok(&request(
        &socket,
        "GET",
        &format!("/v1/tasks/{id}/events"),
        None,
    ));
    assert_eq!(events.skipped, []);
    let mut logged_notes: Vec<String> = events
        .events
        .iter()
        .filter_map(|record| match &record.event {
            Event::NoteLogged { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    logged_notes.sort();
    let mut expected_notes: Vec<String> = (0..8).map(|i| format!("api-note-{i}")).collect();
    expected_notes.sort();
    assert_eq!(logged_notes, expected_notes);

    // 3. Verify reconciliation recorded at least one Edited event for the hand edits:
    assert!(
        events
            .events
            .iter()
            .any(|record| matches!(&record.event, Event::Edited { .. })),
        "expected at least one Edited event for hand edits in events.jsonl: {:?}",
        events.events
    );

    // 4. Verify task detail matches the replayed events and preserves the hand note body:
    let detail: TaskDetail = ok(&request(&socket, "GET", &format!("/v1/tasks/{id}"), None));
    let task = detail.summary.task.unwrap();
    assert!(
        task.title.starts_with("Hand title"),
        "expected Hand title, got {:?}",
        task.title
    );
    assert!(
        detail.body.contains("Hand note"),
        "expected body to contain Hand note, got {:?}",
        detail.body
    );

    let replayed = loom_core::replay(&events.events).unwrap();
    assert_eq!(replayed.title, task.title);

    sandbox.assert_store_clean("work", 2);
    daemon.stop();
}

/// Populates `sandbox` with tasks across `work` and `home`, edits, notes, status transitions,
/// and an undo; returns the task ID that was closed so the caller can archive it.
fn seed_reindex_history(sandbox: &Sandbox) -> loom_api::TaskId {
    let socket = sandbox.socket();
    let work_open = create(
        &socket,
        &json!({
            "title": "Design storage",
            "kind": "story",
            "context": "work",
            "status": "ready",
            "importance": "p1",
            "due": "2026-10-20",
        }),
    )
    .summary
    .id;
    let work_archived = create(
        &socket,
        &json!({
            "title": "Close sprint",
            "kind": "chore",
            "context": "work",
            "status": "ready",
        }),
    )
    .summary
    .id;
    let home_task = create(
        &socket,
        &json!({
            "title": "Fix bike brake",
            "kind": "chore",
            "context": "home",
            "status": "inbox",
        }),
    )
    .summary
    .id;

    let _: TaskDetail = ok(&request(
        &socket,
        "PATCH",
        &format!("/v1/tasks/{work_open}"),
        Some(&json!({ "title": "Design task store", "importance": "p0" })),
    ));
    let _: TaskDetail = ok(&request(
        &socket,
        "POST",
        &format!("/v1/tasks/{work_open}/actions/pin"),
        None,
    ));
    let _: TaskDetail = ok(&request(
        &socket,
        "POST",
        &format!("/v1/tasks/{work_open}/actions/log_note"),
        Some(&json!({ "text": "Reviewed ADR-0013" })),
    ));
    let _: TaskDetail = ok(&request(
        &socket,
        "POST",
        &format!("/v1/tasks/{work_open}/undo"),
        None,
    ));
    let _: TaskDetail = ok(&request(
        &socket,
        "POST",
        &format!("/v1/tasks/{home_task}/actions/ready"),
        None,
    ));
    let _: TaskDetail = ok(&request(
        &socket,
        "POST",
        &format!("/v1/tasks/{work_archived}/actions/done"),
        None,
    ));
    work_archived
}

/// Scenario 4: Reindexing from files (`POST /v1/tasks/reindex` and `loom reindex --json` after
/// deleting `index.db`) produces the exact same indexed rows as the live index across creates,
/// edits, notes, status transitions, undo, and archive moves.
#[test]
fn reindex_from_files_and_cli_loom_reindex_equal_the_live_index() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let work_archived = seed_reindex_history(&sandbox);
    sandbox.assert_store_clean("work", 6);
    sandbox.assert_store_clean("home", 2);
    daemon.stop();

    // Advance `LOOM_TEST_TODAY` by 15 days so `work_archived` moves to `archive/<yyyy>/` at start.
    let daemon = sandbox.start_with(&[("LOOM_TEST_TODAY", "2026-11-05")]);
    sandbox.reindex();
    let live = sandbox.indexed();
    assert_eq!(live.len(), 3);
    assert!(
        live.iter()
            .any(|row| row.id == work_archived && row.location.archive_year().is_some()),
        "{live:?}"
    );

    // 1. Live `POST /v1/tasks/reindex` produces the exact same rows.
    let api_reindexed = sandbox.reindex();
    assert_eq!(api_reindexed.tasks, 3);
    assert_eq!(api_reindexed.invalid, []);
    assert_eq!(api_reindexed.failed, []);
    assert_eq!(sandbox.indexed(), live);

    // 2. End-to-end CLI `loom reindex --json` after deleting `index.db` rebuilds the exact rows.
    daemon.stop();
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", sandbox.index().display()));
    }
    assert!(!sandbox.index().exists());

    let daemon = sandbox.start_with(&[("LOOM_TEST_TODAY", "2026-11-05")]);
    let loom_bin = Path::new(env!("CARGO_BIN_EXE_loomd")).with_file_name("loom");
    if loom_bin.is_file() {
        let output = Command::new(&loom_bin)
            .env("LOOM_HOME", sandbox.loom_home())
            .env("HOME", sandbox.user_home())
            .env("XDG_CONFIG_HOME", sandbox.user_home().join(".config"))
            .env("GIT_CONFIG_GLOBAL", sandbox.gitconfig())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LOOM_TEST_TODAY", "2026-11-05")
            .current_dir(sandbox.user_home())
            .args(["--color=never", "reindex", "--json"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "loom reindex --json failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let cli_result: ReindexResult = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(cli_result.tasks, 3);
        assert_eq!(cli_result.invalid, []);
        assert_eq!(cli_result.failed, []);
    } else {
        sandbox.reindex();
    }

    assert_eq!(sandbox.indexed(), live);
    sandbox.assert_store_clean("work", 7);
    sandbox.assert_store_clean("home", 2);
    daemon.stop();
}

const CREATED_FUTURE_LINE: &str = "{\"id\":\"00000001\",\"v\":1,\"at\":\"2026-10-12T09:14:00Z\",\"actor\":{\"type\":\"user\"},\"machine_id\":\"future-box\",\"type\":\"created\",\"task\":{\"id\":\"k3q7zm2a\",\"title\":\"From the future\",\"kind\":\"bug\",\"context\":\"work\",\"status\":\"ready\",\"importance\":\"p2\",\"pinned\":false,\"created\":\"2026-10-12T09:14:00Z\",\"updated\":\"2026-10-12T09:14:00Z\",\"links\":[],\"relations\":{\"spawned_from\":[],\"blocks\":[]}}}";
const FUTURE_V2_LINE: &str = "{\"id\":\"00000002\",\"v\":2,\"at\":\"2026-10-12T09:15:00Z\",\"actor\":{\"type\":\"user\"},\"machine_id\":\"future-box\",\"type\":\"edited\",\"changes\":[]}";
const FUTURE_TYPE_LINE: &str = "{\"id\":\"00000003\",\"v\":1,\"at\":\"2026-10-12T09:16:00Z\",\"actor\":{\"type\":\"user\"},\"machine_id\":\"future-box\",\"type\":\"delegated_to_mars\",\"extra\":true}";

const MD_EXT: &str = "\
+++
# A comment kept across rewrites
id = \"k3q7zm2a\"
title = \"From the future\"
kind = \"bug\"
context = \"work\"
status = \"ready\"
importance = \"p2\"
created = 2026-10-12T09:14:00Z
future_scalar = \"keep-me\"

[future_table]
 setting = 42
+++

## Notes

Initial body.
";

const MD_UNKNOWN_KIND: &str = "\
+++
id = \"hhhhhhhh\"
title = \"Unknown kind task\"
kind = \"spaceship\"
context = \"work\"
status = \"ready\"
importance = \"p3\"
created = 2026-10-12T09:14:00Z
+++

## Notes
";

const MD_UNKNOWN_STATUS: &str = "\
+++
id = \"nnnnnnnn\"
title = \"Unknown status task\"
kind = \"chore\"
context = \"work\"
status = \"hibernating\"
importance = \"p3\"
created = 2026-10-12T09:14:00Z
+++

## Notes
";

/// Scenario 5: Files written by a newer version of `loom` (unknown TOML keys, comments, unknown
/// event version or type, unknown `kind` or `status`) preserve unknown data byte-for-byte on
/// rewrite and refuse edits on unknown `kind` or `status` with `409 Conflict`.
#[test]
fn files_from_a_newer_version_keep_unknown_data_and_guard_unknown_kind_or_status() {
    let sandbox = Sandbox::new();
    let id_ext = "k3q7zm2a";
    let dir_ext = sandbox.task_dir("work", id_ext);
    fs::create_dir_all(&dir_ext).unwrap();
    fs::write(dir_ext.join("task.md"), MD_EXT).unwrap();
    fs::write(
        dir_ext.join("events.jsonl"),
        format!("{CREATED_FUTURE_LINE}\n{FUTURE_V2_LINE}\n{FUTURE_TYPE_LINE}\n"),
    )
    .unwrap();
    sandbox.write_task("work", "hhhhhhhh", MD_UNKNOWN_KIND);
    sandbox.write_task("work", "nnnnnnnn", MD_UNKNOWN_STATUS);

    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.reindex();

    // Edit `k3q7zm2a` through the API: comments, unknown TOML keys, and future event lines survive.
    let edited: TaskDetail = ok(&request(
        &socket,
        "PATCH",
        &format!("/v1/tasks/{id_ext}"),
        Some(&json!({ "title": "Edited in v0.3", "importance": "p0" })),
    ));
    assert_eq!(edited.summary.task.unwrap().title, "Edited in v0.3");
    let _: TaskDetail = ok(&request(
        &socket,
        "POST",
        &format!("/v1/tasks/{id_ext}/actions/log_note"),
        Some(&json!({ "text": "Note from v0.3" })),
    ));

    let rewritten_md = fs::read_to_string(dir_ext.join("task.md")).unwrap();
    for fragment in [
        "# A comment kept across rewrites",
        "future_scalar = \"keep-me\"",
        "[future_table]\n setting = 42",
        "title = \"Edited in v0.3\"",
        "Initial body.",
    ] {
        assert!(rewritten_md.contains(fragment), "{rewritten_md}");
    }

    let rewritten_events = fs::read_to_string(dir_ext.join("events.jsonl")).unwrap();
    let lines: Vec<&str> = rewritten_events.lines().collect();
    assert_eq!(lines.len(), 5, "{rewritten_events}");
    assert_eq!(lines[0], CREATED_FUTURE_LINE);
    assert_eq!(lines[1], FUTURE_V2_LINE);
    assert_eq!(lines[2], FUTURE_TYPE_LINE);
    assert!(lines[4].contains("Note from v0.3"), "{}", lines[4]);

    let api_events: TaskEvents = ok(&request(
        &socket,
        "GET",
        &format!("/v1/tasks/{id_ext}/events"),
        None,
    ));
    assert_eq!(api_events.events.len(), 3);
    assert_eq!(api_events.skipped.len(), 2);

    // Unknown kind and unknown status are read-only and refuse edits with 409 Conflict.
    for (id, expected_ro, original_md) in [
        (
            "hhhhhhhh",
            TaskReadOnly::UnknownKind("spaceship".to_owned()),
            MD_UNKNOWN_KIND,
        ),
        (
            "nnnnnnnn",
            TaskReadOnly::UnknownStatus("hibernating".to_owned()),
            MD_UNKNOWN_STATUS,
        ),
    ] {
        let detail: TaskDetail = ok(&request(&socket, "GET", &format!("/v1/tasks/{id}"), None));
        assert_eq!(detail.summary.read_only, Some(expected_ro.clone()));
        assert_eq!(detail.actions, []);

        let err = problem(
            &request(
                &socket,
                "PATCH",
                &format!("/v1/tasks/{id}"),
                Some(&json!({ "title": "Clobber" })),
            ),
            409,
        );
        assert_eq!(err.code, ErrorCode::ReadOnly);
        let ro: TaskReadOnly = serde_json::from_value(err.details.unwrap()).unwrap();
        assert_eq!(ro, expected_ro);
        assert_eq!(
            fs::read_to_string(sandbox.task_dir("work", id).join("task.md")).unwrap(),
            original_md
        );
    }

    sandbox.assert_store_clean("work", 3);
    daemon.stop();
}

/// Scenario 6: Store commits succeed when `commit.gpgsign=true` and `tag.gpgsign=true` are set in
/// the user's `.gitconfig` and in the store's `.git/config` with a failing GPG binary.
#[test]
fn store_commits_succeed_when_gpg_signing_is_enabled_and_broken() {
    let sandbox = Sandbox::new();
    fs::write(
        sandbox.gitconfig(),
        "\
[user]
\tname = Store Owner
\temail = owner@example.invalid
[commit]
\tgpgsign = true
[tag]
\tgpgsign = true
[gpg]
\tprogram = /nonexistent-gpg-binary
",
    )
    .unwrap();

    let daemon = sandbox.start();
    let socket = sandbox.socket();

    let created = create(
        &socket,
        &json!({
            "title": "Unsigned store commit",
            "kind": "chore",
            "context": "work",
            "status": "ready",
        }),
    );
    let id = created.summary.id;
    sandbox.wait_for_commits("work", 1);

    // Also set `commit.gpgsign = true` inside the store repository's own `.git/config`.
    sandbox.git_in("work", &["config", "commit.gpgsign", "true"]);
    sandbox.git_in(
        "work",
        &["config", "gpg.program", "/nonexistent-gpg-binary"],
    );

    let _: TaskDetail = ok(&request(
        &socket,
        "PATCH",
        &format!("/v1/tasks/{id}"),
        Some(&json!({ "title": "Edited with gpgsign enabled" })),
    ));
    let _: TaskDetail = ok(&request(
        &socket,
        "POST",
        &format!("/v1/tasks/{id}/actions/done"),
        None,
    ));

    let subjects = sandbox.wait_for_commits("work", 3);
    assert_eq!(
        subjects,
        [
            format!("created({id}): Unsigned store commit"),
            format!("edited({id}): title"),
            format!("status_changed({id}): ready -> done"),
        ]
    );
    let backup: BackupReport = ok(&request(&socket, "GET", "/v1/tasks/backup", None));
    assert_eq!(backup.contexts[0].uncommitted, None);
    sandbox.assert_store_clean("work", 3);
    daemon.stop();
}

/// Scenario 7: Pushing to a remote that rejects the push (`pre-receive` hook exiting `1`) reports
/// the push failure without losing local task files, index rows, or local git commits.
#[test]
fn a_rejected_push_reports_the_failure_and_keeps_local_tasks_and_commits_intact() {
    let sandbox = Sandbox::with_remote();
    let daemon = sandbox.start();
    let socket = sandbox.socket();

    let first = create(
        &socket,
        &json!({
            "title": "Initial synced task",
            "kind": "chore",
            "context": "work",
            "status": "ready",
        }),
    )
    .summary
    .id;
    sandbox.wait_for_commits("work", 1);

    let initial_sync: BackupReport = ok(&request(
        &socket,
        "POST",
        "/v1/tasks/sync",
        Some(&json!({ "context": "work" })),
    ));
    assert!(!initial_sync.contexts[0].failed(), "{initial_sync:?}");

    // Install a rejecting `pre-receive` hook in the bare backup remote.
    let hook = sandbox.remote().join("hooks/pre-receive");
    fs::write(
        &hook,
        "#!/bin/sh\necho 'remote policy: push refused' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();

    // Create a second task and edit the first one locally.
    let second = create(
        &socket,
        &json!({
            "title": "Created while remote rejects",
            "kind": "bug",
            "context": "work",
            "status": "active",
            "importance": "p0",
        }),
    )
    .summary
    .id;
    let _: TaskDetail = ok(&request(
        &socket,
        "POST",
        &format!("/v1/tasks/{first}/actions/log_note"),
        Some(&json!({ "text": "Local note survives rejected push" })),
    ));
    let subjects = sandbox.wait_for_commits("work", 3);
    assert_eq!(
        subjects,
        [
            format!("created({first}): Initial synced task"),
            format!("created({second}): Created while remote rejects"),
            format!("note_logged({first}): Local note survives rejected push"),
        ]
    );

    // Sync against the rejecting remote: the push failure is reported as PushRejected.
    let failed_sync: BackupReport = ok(&request(
        &socket,
        "POST",
        "/v1/tasks/sync",
        Some(&json!({ "context": "work" })),
    ));
    let work = &failed_sync.contexts[0];
    assert!(work.failed(), "{work:?}");
    assert_eq!(work.uncommitted, None);
    let outcome = &work.last_sync.as_ref().unwrap().outcome;
    assert!(
        matches!(
            outcome,
            BackupOutcome::PushRejected { stderr_tail }
                if stderr_tail.contains("remote policy: push refused")
        ),
        "{outcome:?}"
    );

    // Local files, index, and git commits are completely intact.
    let first_events = fs::read_to_string(
        sandbox
            .task_dir("work", first.as_str())
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(
        first_events.contains("Local note survives rejected push"),
        "{first_events}"
    );
    let second_md =
        fs::read_to_string(sandbox.task_dir("work", second.as_str()).join("task.md")).unwrap();
    assert!(
        second_md.contains("Created while remote rejects"),
        "{second_md}"
    );
    assert_eq!(sandbox.indexed().len(), 2);
    assert_eq!(sandbox.subjects_in("work"), subjects);
    sandbox.assert_store_clean("work", 3);
    daemon.stop();
}

/// A move commits the task in the target store and its removal in the source store, and both
/// working trees are clean.
#[test]
fn a_move_is_committed_in_both_stores() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let created = create(
        &sandbox.socket(),
        &json!({ "title": "Travelling", "kind": "chore", "context": "work", "status": "ready" }),
    );
    let id = created.summary.id;
    sandbox.wait_for_commits("work", 1);
    let _: TaskDetail = ok(&request(
        &sandbox.socket(),
        "POST",
        &format!("/v1/tasks/{id}/move"),
        Some(&json!({ "context": "home" })),
    ));

    let work = sandbox.wait_for_commits("work", 2);
    assert_eq!(work[1], format!("moved({id}): moved to home"));
    let home = sandbox.wait_for_commits("home", 1);
    assert_eq!(home[0], format!("edited({id}): context"));
    sandbox.assert_store_clean("work", 2);
    sandbox.assert_store_clean("home", 1);
    assert!(!sandbox.task_dir("work", id.as_str()).exists());
    assert!(sandbox.task_dir("home", id.as_str()).is_dir());
    daemon.stop();
}

/// Kill `loomd` (`SIGKILL`) after the copy of a moved task is in place and before the source is
/// removed (`after_receive`): both stores hold the task. On restart it is flagged as a duplicate
/// ID, read-only, and removing one copy heals it.
#[test]
fn loomd_killed_during_a_move_leaves_a_flagged_duplicate_that_one_removal_heals() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let created = create(
        &sandbox.socket(),
        &json!({ "title": "Interrupted", "kind": "chore", "context": "work", "status": "ready" }),
    );
    let id = created.summary.id;
    daemon.stop();

    let mut crashing = sandbox.spawn_crashable(CrashStage::AfterReceive.as_str());
    send_expecting_crash(
        &sandbox.socket(),
        "POST",
        &format!("/v1/tasks/{id}/move"),
        &json!({ "context": "home" }),
        &mut crashing,
    );
    assert!(sandbox.task_dir("work", id.as_str()).is_dir());
    assert!(sandbox.task_dir("home", id.as_str()).is_dir());

    // Restarted: flagged, not chosen between.
    let daemon = sandbox.start();
    let reindexed = sandbox.reindex();
    assert_eq!(reindexed.invalid.len(), 1, "{reindexed:?}");
    assert!(
        reindexed.invalid[0]
            .error
            .contains("duplicate ID across contexts"),
        "{}",
        reindexed.invalid[0].error
    );
    let refused = request(
        &sandbox.socket(),
        "POST",
        &format!("/v1/tasks/{id}/actions/done"),
        None,
    );
    assert_eq!(refused.status, 409, "{}", refused.body);

    // One removal, then the task is whole again, in the context left.
    fs::remove_dir_all(sandbox.task_dir("work", id.as_str())).unwrap();
    let reindexed = sandbox.reindex();
    assert_eq!(reindexed.invalid, []);
    let rows = sandbox.indexed();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].context.as_str(), "home");
    daemon.stop();
}
