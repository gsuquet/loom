//! The git repository of each context store: created at the first commit, one commit per
//! change, unsigned, a failed commit made later. Hermetic: a temporary `HOME` whose only git
//! configuration is a `.gitconfig` with an identity, no system configuration.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Instant;

use loom_api::{BackupOutcome, BackupReport, ContextName, ReindexResult, TaskId, Timestamp};
use loom_core::{Actor, Event, Kind, MachineId, NewTask, Origin, Relations, Status};
use loom_store::files::FileStore;
use loom_store::tasks::{TaskIndex, Tasks};
use loomd::backup::commit_message;
use loomd::repo_update::GitRunner;
use loomd::store_git::{Committed, StoreCommits};
use serde_json::json;
use tempfile::TempDir;

mod common;

use common::{Daemon, POLL, TIMEOUT, git, git_run, ok, request};

const CONFIG: &str = "[repos]\nroots = []\n\n[contexts.work]\n";

struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    /// A loom home and a user home under `/tmp` (the macOS `$TMPDIR` is too long for a socket
    /// path), the user's git identity in `~/.gitconfig`.
    fn new() -> Self {
        Self::with_config(CONFIG)
    }

    /// With a bare `remote.git` as the backup remote of `work`.
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
            format!("{CONFIG}store.backup.remote = \"{}\"\n", remote.display()),
        )
        .unwrap();
        sandbox
    }

    fn with_config(config: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("loomd-backup-")
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

    fn loom_home(&self) -> PathBuf {
        self.dir.path().join("l")
    }

    fn user_home(&self) -> PathBuf {
        self.dir.path().join("u")
    }

    fn remote(&self) -> PathBuf {
        self.dir.path().join("remote.git")
    }

    /// `POST /v1/tasks/sync` with `body`.
    fn sync(&self, body: &serde_json::Value) -> common::HttpResponse {
        request(&self.socket(), "POST", "/v1/tasks/sync", Some(body))
    }

    fn gitconfig(&self) -> PathBuf {
        self.user_home().join(".gitconfig")
    }

    fn store(&self) -> PathBuf {
        self.loom_home().join("contexts/work")
    }

    fn socket(&self) -> PathBuf {
        self.loom_home().join("run/loomd.sock")
    }

    /// The environment loomd and its git commands get.
    fn env(&self) -> Vec<(OsString, Option<OsString>)> {
        vec![
            ("HOME".into(), Some(self.user_home().into())),
            (
                "XDG_CONFIG_HOME".into(),
                Some(self.user_home().join(".config").into()),
            ),
            ("GIT_CONFIG_GLOBAL".into(), Some(self.gitconfig().into())),
            ("GIT_CONFIG_NOSYSTEM".into(), Some("1".into())),
        ]
    }

    fn start(&self) -> Daemon {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
        command
            .env("LOOM_HOME", self.loom_home())
            // Set again by `env()`, explicit for the isolation guard.
            .env("HOME", self.user_home())
            .arg("--color=never")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (name, value) in self.env() {
            if let Some(value) = value {
                command.env(name, value);
            }
        }
        Daemon::start(&mut command, &self.socket())
    }

    /// Reconciles every task and commits what is left: the task thread has run every command
    /// sent before it.
    fn reindex(&self) -> ReindexResult {
        ok(&request(&self.socket(), "POST", "/v1/tasks/reindex", None))
    }

    /// `git <args>` in the store.
    fn git(&self, args: &[&str]) -> String {
        git(&self.user_home(), &self.store(), args)
    }

    /// Subjects of the store's commits, oldest first; empty without a commit.
    fn subjects(&self) -> Vec<String> {
        let output = git_run(
            &self.user_home(),
            &self.store(),
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

    /// Waits until the store has `count` commits.
    fn wait_for_commits(&self, count: usize) -> Vec<String> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let subjects = self.subjects();
            if subjects.len() >= count {
                return subjects;
            }
            assert!(Instant::now() < deadline, "commits: {subjects:?}");
            thread::sleep(POLL);
        }
    }

    fn task_md(&self, id: &TaskId) -> PathBuf {
        self.store().join("tasks").join(id.as_str()).join("task.md")
    }

    /// Replaces `from` with `to` in the `task.md` of `id`, as an editor would.
    fn hand_edit(&self, id: &TaskId, from: &str, to: &str) {
        let path = self.task_md(id);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{text}");
        fs::write(&path, text.replace(from, to)).unwrap();
    }

    fn events(&self, id: &TaskId) -> usize {
        fs::read_to_string(
            self.store()
                .join("tasks")
                .join(id.as_str())
                .join("events.jsonl"),
        )
        .unwrap()
        .lines()
        .count()
    }
}

fn context(name: &str) -> ContextName {
    name.parse().unwrap()
}

fn origin() -> Origin {
    Origin {
        actor: Actor::User,
        machine_id: MachineId("test".to_owned()),
    }
}

/// A source of IDs that differs per call.
fn random() -> impl loom_core::RandomSource {
    struct Counter(u64);
    impl loom_core::RandomSource for Counter {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }
    }
    Counter(7)
}

fn new_task(title: &str) -> NewTask {
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
    }
}

/// Creates a task through the store, as the tasks API does, before loomd starts.
fn create_task(sandbox: &Sandbox, title: &str) -> TaskId {
    let store = FileStore::new(context("work"), sandbox.store());
    let index = TaskIndex::open(&sandbox.loom_home().join("index.db")).unwrap();
    let mut tasks = Tasks::new(vec![store], index);
    tasks
        .create(
            new_task(title),
            Timestamp::from_unix_secs(1_760_000_000).unwrap(),
            &origin(),
            &mut random(),
        )
        .unwrap()
        .id
}

#[test]
fn the_store_becomes_a_repository_and_each_hand_edit_one_commit() {
    let sandbox = Sandbox::new();
    let id = create_task(&sandbox, "Before");
    let daemon = sandbox.start();

    // At start, the store gets its repository and a first commit of every file.
    let subjects = sandbox.wait_for_commits(1);
    assert_eq!(subjects, ["sync(work): uncommitted changes"]);
    let files = sandbox.git(&["ls-files"]);
    assert_eq!(
        files.lines().collect::<Vec<_>>(),
        [
            ".gitattributes".to_owned(),
            ".gitignore".to_owned(),
            format!("tasks/{id}/events.jsonl"),
            format!("tasks/{id}/task.md"),
        ]
    );
    assert_eq!(
        fs::read_to_string(sandbox.store().join(".gitattributes")).unwrap(),
        "*.jsonl merge=union\n"
    );
    assert_eq!(sandbox.git(&["branch", "--show-current"]), "main\n");
    // The user's identity applies.
    assert_eq!(
        sandbox.git(&["log", "-1", "--format=%an <%ae>"]),
        "Store Owner <owner@example.invalid>\n"
    );
    // Temporary files of atomic writes are never committed.
    sandbox.git(&["check-ignore", "-q", &format!("tasks/{id}/.task.md.tmp")]);
    sandbox.git(&["check-ignore", "-q", "tasks/.aaaaaaaa.tmp/task.md"]);

    sandbox.hand_edit(&id, "title = \"Before\"", "title = \"After\"");
    let subjects = sandbox.wait_for_commits(2);
    assert_eq!(subjects[1], format!("edited({id}): title"));
    let changed = sandbox.git(&["show", "--format=", "--name-only", "HEAD"]);
    assert_eq!(
        changed.lines().collect::<Vec<_>>(),
        [
            format!("tasks/{id}/events.jsonl"),
            format!("tasks/{id}/task.md")
        ]
    );
    assert_eq!(sandbox.events(&id), 2);

    // A reconciliation that finds nothing new commits nothing.
    sandbox.reindex();
    assert_eq!(sandbox.subjects().len(), 2);
    assert_eq!(sandbox.git(&["status", "--porcelain"]), "");
    daemon.stop();
}

#[test]
fn commits_are_unsigned_even_when_the_configuration_signs() {
    let sandbox = Sandbox::new();
    let id = create_task(&sandbox, "Before");
    let daemon = sandbox.start();
    sandbox.wait_for_commits(1);

    // Signing on, in the store's configuration and in the user's, with a program that fails:
    // a signed commit cannot be made.
    let gpg = sandbox.user_home().join("fake-gpg");
    fs::write(&gpg, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&gpg, fs::Permissions::from_mode(0o755)).unwrap();
    let gpg = gpg.to_str().unwrap();
    sandbox.git(&["config", "commit.gpgsign", "true"]);
    sandbox.git(&["config", "tag.gpgsign", "true"]);
    sandbox.git(&["config", "gpg.program", gpg]);
    sandbox.git(&["config", "user.signingkey", "ABCDEF"]);
    fs::write(
        sandbox.gitconfig(),
        format!("[user]\n\tname = Store Owner\n\temail = owner@example.invalid\n[commit]\n\tgpgsign = true\n[gpg]\n\tprogram = {gpg}\n"),
    )
    .unwrap();
    let signed = Command::new("git")
        .current_dir(sandbox.store())
        .envs(
            sandbox
                .env()
                .into_iter()
                .filter_map(|(k, v)| v.map(|v| (k, v))),
        )
        .args(["commit", "--allow-empty", "-q", "-m", "signed"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        !signed.status.success(),
        "signing should fail in this setup"
    );

    sandbox.hand_edit(&id, "title = \"Before\"", "title = \"After\"");
    let subjects = sandbox.wait_for_commits(2);
    assert_eq!(subjects[1], format!("edited({id}): title"));
    for commit in ["HEAD", "HEAD~1"] {
        let raw = sandbox.git(&["cat-file", "commit", commit]);
        assert!(!raw.contains("gpgsig"), "{raw}");
    }
    daemon.stop();
}

#[test]
fn a_failed_commit_keeps_the_write_and_is_made_later() {
    let sandbox = Sandbox::new();
    let id = create_task(&sandbox, "Before");
    let daemon = sandbox.start();
    sandbox.wait_for_commits(1);

    // Another git process holds the index: every commit fails.
    let lock = sandbox.store().join(".git/index.lock");
    fs::write(&lock, "").unwrap();
    sandbox.hand_edit(&id, "title = \"Before\"", "title = \"After\"");
    // Once the reindex answers, the edit was recorded and its commit tried, and the catch-up
    // commit of the reindex too.
    let deadline = Instant::now() + TIMEOUT;
    while sandbox.events(&id) < 2 {
        assert!(Instant::now() < deadline, "edit never recorded");
        thread::sleep(POLL);
    }
    sandbox.reindex();
    assert_eq!(sandbox.subjects().len(), 1);
    assert!(
        fs::read_to_string(sandbox.task_md(&id))
            .unwrap()
            .contains("title = \"After\"")
    );
    assert_eq!(sandbox.events(&id), 2);

    fs::remove_file(&lock).unwrap();
    sandbox.reindex();
    let subjects = sandbox.subjects();
    assert_eq!(subjects.len(), 2, "{subjects:?}");
    assert_eq!(subjects[1], "sync(work): uncommitted changes");
    assert_eq!(sandbox.git(&["status", "--porcelain"]), "");
    daemon.stop();
}

/// The path every API mutation takes: the write through `Tasks`, then one commit.
#[test]
fn each_mutation_through_the_store_is_one_commit() {
    let sandbox = Sandbox::new();
    let store = FileStore::new(context("work"), sandbox.store());
    store.init().unwrap();
    let index = TaskIndex::open(&sandbox.loom_home().join("index.db")).unwrap();
    let mut tasks = Tasks::new(vec![store], index);
    let mut runner = GitRunner::from_env();
    runner.env = sandbox.env();
    let mut commits =
        StoreCommits::new(runner, BTreeMap::from([(context("work"), sandbox.store())]));
    let now = Timestamp::from_unix_secs(1_760_000_000).unwrap();
    let mut random = random();

    let task = tasks
        .create(new_task("Ship it"), now, &origin(), &mut random)
        .unwrap();
    let created = Event::Created {
        task: Box::new(task.clone()),
    };
    let message = commit_message(&task.id, &[&created]).unwrap();
    assert_eq!(
        commits.commit_task(&context("work"), &task.id, &message),
        Ok(Committed::Created)
    );

    let events = tasks
        .apply(
            &task.id,
            loom_core::Command::Pin,
            now,
            &origin(),
            &mut random,
        )
        .map(|_| {
            tasks
                .stores()
                .values()
                .next()
                .map(|store| {
                    loom_store::port::StorePort::read_events(store, &task.id)
                        .unwrap()
                        .events
                })
                .unwrap()
        })
        .unwrap();
    let last = &events.last().unwrap().event;
    let message = commit_message(&task.id, &[last]).unwrap();
    assert_eq!(
        commits.commit_task(&context("work"), &task.id, &message),
        Ok(Committed::Created)
    );
    // Nothing changed since: no empty commit.
    assert_eq!(
        commits.commit_task(&context("work"), &task.id, &message),
        Ok(Committed::Nothing)
    );
    assert_eq!(
        sandbox.subjects(),
        [
            format!("created({}): Ship it", task.id),
            format!("edited({}): pinned", task.id)
        ]
    );
    assert!(commits.pending().is_empty());
    assert!(is_clean(&sandbox.user_home(), &sandbox.store()));
}

fn is_clean(home: &Path, store: &Path) -> bool {
    git(home, store, &["status", "--porcelain"]).is_empty()
}

#[test]
fn each_api_mutation_is_one_commit() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let call = |method: &str, path: &str, body: Option<serde_json::Value>| {
        let response = request(&sandbox.socket(), method, path, body.as_ref());
        assert_eq!(response.status, 200, "{method} {path}: {}", response.body);
        serde_json::from_str::<serde_json::Value>(&response.body).unwrap()
    };
    let created = call(
        "POST",
        "/v1/tasks",
        Some(
            serde_json::json!({ "title": "Ship it", "kind": "chore", "context": "work", "status": "ready" }),
        ),
    );
    let id = created["id"].as_str().unwrap().to_owned();
    let mut expected = vec![format!("created({id}): Ship it")];
    assert_eq!(sandbox.wait_for_commits(1), expected);

    let steps: [(&str, String, Option<serde_json::Value>, String); 6] = [
        (
            "PATCH",
            format!("/v1/tasks/{id}"),
            Some(serde_json::json!({ "title": "Ship it now" })),
            format!("edited({id}): title"),
        ),
        (
            "POST",
            format!("/v1/tasks/{id}/actions/pin"),
            None,
            format!("edited({id}): pinned"),
        ),
        (
            "POST",
            format!("/v1/tasks/{id}/actions/unpin"),
            None,
            format!("edited({id}): pinned"),
        ),
        (
            "POST",
            format!("/v1/tasks/{id}/actions/log_note"),
            Some(serde_json::json!({ "text": "Asked Alice" })),
            format!("note_logged({id}): Asked Alice"),
        ),
        (
            "POST",
            format!("/v1/tasks/{id}/actions/active"),
            None,
            format!("status_changed({id}): ready -> active"),
        ),
        (
            "POST",
            format!("/v1/tasks/{id}/undo"),
            None,
            format!("undone({id}): event "),
        ),
    ];
    for (method, path, body, subject) in steps {
        call(method, &path, body);
        expected.push(subject);
        let subjects = sandbox.wait_for_commits(expected.len());
        assert_eq!(
            subjects.len(),
            expected.len(),
            "{method} {path}: {subjects:?}"
        );
        let last = subjects.last().unwrap();
        assert!(
            last.starts_with(expected.last().unwrap()),
            "{method} {path}: {subjects:?}"
        );
    }
    // A request that changes nothing commits nothing.
    call("POST", &format!("/v1/tasks/{id}/actions/unpin"), None);
    sandbox.reindex();
    assert_eq!(sandbox.subjects().len(), expected.len());
    assert_eq!(sandbox.git(&["status", "--porcelain"]), "");
    daemon.stop();
}

#[test]
fn hooks_never_block_a_commit() {
    let sandbox = Sandbox::new();
    let id = create_task(&sandbox, "Before");
    let daemon = sandbox.start();
    sandbox.wait_for_commits(1);

    // A failing pre-commit hook in the store, and a global hooks directory with another one.
    let failing = "#!/bin/sh\necho refused >&2\nexit 1\n";
    let hook = sandbox.store().join(".git/hooks/pre-commit");
    fs::write(&hook, failing).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let global = sandbox.user_home().join("hooks");
    fs::create_dir_all(&global).unwrap();
    for name in ["pre-commit", "commit-msg"] {
        let path = global.join(name);
        fs::write(&path, failing).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        sandbox.gitconfig(),
        format!(
            "[user]\n\tname = Store Owner\n\temail = owner@example.invalid\n[core]\n\thooksPath = {}\n",
            global.display()
        ),
    )
    .unwrap();

    sandbox.hand_edit(&id, "title = \"Before\"", "title = \"After\"");
    let subjects = sandbox.wait_for_commits(2);
    assert_eq!(subjects[1], format!("edited({id}): title"));
    assert_eq!(sandbox.git(&["status", "--porcelain"]), "");
    daemon.stop();
}

#[test]
fn loom_sync_pushes_and_reports_a_divergence() {
    let sandbox = Sandbox::with_remote();
    let id = create_task(&sandbox, "Before");
    let daemon = sandbox.start();

    let report: BackupReport = ok(&sandbox.sync(&json!({})));
    assert_eq!(report.contexts.len(), 1);
    let work = &report.contexts[0];
    assert_eq!(work.context, context("work"));
    assert_eq!(
        work.remote.as_deref(),
        Some(sandbox.remote().to_str().unwrap())
    );
    // The sync at start may have pushed already.
    let outcome = &work.last_sync.as_ref().unwrap().outcome;
    assert!(
        matches!(
            outcome,
            BackupOutcome::Pushed { commits: 1 } | BackupOutcome::UpToDate
        ),
        "{outcome:?}"
    );
    let head = sandbox.git(&["rev-parse", "HEAD"]);
    assert_eq!(
        git(
            &sandbox.user_home(),
            &sandbox.remote(),
            &["rev-parse", "main"]
        ),
        head
    );

    // Another machine pushed, this one edited: diverged, nothing merged.
    let other = sandbox.dir.path().join("other");
    git(
        &sandbox.user_home(),
        sandbox.dir.path(),
        &[
            "clone",
            "-q",
            sandbox.remote().to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    fs::write(other.join("note.txt"), "elsewhere\n").unwrap();
    git(&sandbox.user_home(), &other, &["add", "note.txt"]);
    git(
        &sandbox.user_home(),
        &other,
        &["commit", "-q", "-m", "elsewhere"],
    );
    git(
        &sandbox.user_home(),
        &other,
        &["push", "-q", "origin", "main"],
    );
    sandbox.hand_edit(&id, "title = \"Before\"", "title = \"After\"");
    sandbox.wait_for_commits(2);
    let local = sandbox.git(&["rev-parse", "HEAD"]);

    let report: BackupReport = ok(&sandbox.sync(&json!({ "context": "work" })));
    assert_eq!(
        report.contexts[0].last_sync.as_ref().unwrap().outcome,
        BackupOutcome::Diverged {
            local: 1,
            remote: 1
        }
    );
    assert_eq!(sandbox.git(&["rev-parse", "HEAD"]), local);
    assert!(!sandbox.store().join("note.txt").exists());

    // The state stays readable without a sync.
    let state: BackupReport = ok(&request(&sandbox.socket(), "GET", "/v1/tasks/backup", None));
    assert_ne!(state.machine, "");
    assert!(state.contexts[0].failed());

    let unknown = sandbox.sync(&json!({ "context": "nope" }));
    assert_eq!(unknown.status, 400, "{}", unknown.body);
    assert!(
        unknown.body.contains("no context `nope`"),
        "{}",
        unknown.body
    );
    daemon.stop();
}
