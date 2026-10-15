//! `loom add`, `ls` and `show` end to end: a temporary loom home and user home, the `loomd`
//! built next to `loom` started on demand.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{RankReason, Status, TaskDetail, TaskList};
use tempfile::TempDir;

mod isolation;

const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
/// The date every command of these tests runs on (debug builds read `LOOM_TEST_TODAY`).
const TODAY: &str = "2026-10-14";

/// `work` takes the repositories of `gitlab.example.com/company-a`; `home` takes `~/perso`;
/// everything else goes to `personal`.
const CONFIG: &str = r#"
default_context = "personal"

[repos]
roots = ["~/perso"]

[contexts.personal]

[contexts.home]
match.paths = ["~/perso"]

[contexts.work]
match.hosts = ["gitlab.example.com/company-a"]
"#;

fn loom_bin() -> PathBuf {
    PathBuf::from(assert_cmd::cargo::cargo_bin!("loom"))
}

/// A loom home and a user home under `/tmp` (the macOS `$TMPDIR` is too long for a socket).
struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let loomd = loom_bin().with_file_name("loomd");
        assert!(loomd.is_file(), "{} is missing", loomd.display());
        let dir = tempfile::Builder::new()
            .prefix("lt")
            .tempdir_in("/tmp")
            .unwrap();
        let sandbox = Self { dir };
        fs::create_dir_all(sandbox.loom_home()).unwrap();
        fs::create_dir_all(sandbox.home().join("perso")).unwrap();
        fs::write(sandbox.loom_home().join("config.toml"), CONFIG).unwrap();
        sandbox
    }

    fn loom_home(&self) -> PathBuf {
        self.dir.path().join("l")
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("u")
    }

    /// `loom --color=never <args>` run in `cwd`, as a shell would (`PWD` set), with
    /// `LOOM_CONTEXT` set to `context` (unset when `None`).
    fn run(&self, cwd: &Path, context: Option<&str>, args: &[&str]) -> Output {
        let mut command = Command::new(loom_bin());
        command
            .env("LOOM_HOME", self.loom_home())
            .envs(isolation::isolated_env(&self.home()))
            .env("PWD", cwd)
            .env("LOOM_TEST_TODAY", TODAY)
            .env_remove("LOOM_CONTEXT")
            .env_remove("NO_COLOR")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
            .current_dir(cwd)
            .arg("--color=never")
            .args(args)
            .stdin(Stdio::null());
        if let Some(context) = context {
            command.env("LOOM_CONTEXT", context);
        }
        command.output().unwrap()
    }

    /// Runs `args` in `cwd`, expecting success; stdout.
    fn ok_in(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.run(cwd, None, args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            stderr(&output)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.ok_in(&self.home(), args)
    }

    /// `loom add --json <args>` in `cwd`: the created task.
    fn add_in(&self, cwd: &Path, context: Option<&str>, args: &[&str]) -> TaskDetail {
        let mut full = vec!["add", "--json"];
        full.extend_from_slice(args);
        let output = self.run(cwd, context, &full);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            stderr(&output)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn add(&self, args: &[&str]) -> TaskDetail {
        self.add_in(&self.home(), None, args)
    }
}

/// Stops the loomd of the sandbox, if one runs, before the directory is removed.
impl Drop for Sandbox {
    fn drop(&mut self) {
        let lock = self.loom_home().join("run/loomd.lock");
        let Ok(file) = File::open(&lock) else {
            return;
        };
        if file.try_lock().is_ok() {
            return;
        }
        if let Some(pid) = fs::read_to_string(&lock)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
            let deadline = Instant::now() + TIMEOUT;
            while file.try_lock().is_err() && Instant::now() < deadline {
                thread::sleep(POLL);
            }
        }
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn git(home: &Path, dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", stderr(&output));
}

fn context_of(task: &TaskDetail) -> String {
    task.summary.context.to_string()
}

#[test]
fn the_context_follows_the_documented_order() {
    let sandbox = Sandbox::new();
    let perso = sandbox.home().join("perso");
    // A repository under `~/perso` whose remote belongs to `work`: its context is `work`.
    let repo = perso.join("api");
    fs::create_dir_all(&repo).unwrap();
    git(&sandbox.home(), &repo, &["init", "-q"]);
    git(
        &sandbox.home(),
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "git@gitlab.example.com:company-a/api.git",
        ],
    );
    let notes = perso.join("notes");
    fs::create_dir_all(&notes).unwrap();

    // 1. `-c` wins over everything.
    let task = sandbox.add_in(&repo, Some("home"), &["-c", "personal", "Flag"]);
    assert_eq!(context_of(&task), "personal");
    // 2. Then `LOOM_CONTEXT`.
    let task = sandbox.add_in(&repo, Some("home"), &["Environment"]);
    assert_eq!(context_of(&task), "home");
    // 3. Then the repository of the current directory (its remote, not its path).
    let task = sandbox.add_in(&repo.join(".git"), None, &["Repository"]);
    assert_eq!(context_of(&task), "work");
    // 4. Then the `match.paths` prefix of the directory.
    let task = sandbox.add_in(&notes, None, &["Directory"]);
    assert_eq!(context_of(&task), "home");
    // 5. Then `default_context`.
    let task = sandbox.add_in(&sandbox.home(), None, &["Default"]);
    assert_eq!(context_of(&task), "personal");

    // An invalid `LOOM_CONTEXT` is an error, not a fallback.
    let output = sandbox.run(&sandbox.home(), Some("Not A Name"), &["add", "X"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("LOOM_CONTEXT"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn without_any_rule_add_fails_and_says_how_to_choose() {
    let sandbox = Sandbox::new();
    fs::write(
        sandbox.loom_home().join("config.toml"),
        "[repos]\nroots = []\n\n[contexts.work]\n",
    )
    .unwrap();
    let output = sandbox.run(&sandbox.home(), None, &["add", "Lost"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("no context: pass -c"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn added_tasks_are_listed_ranked_and_shown() {
    let sandbox = Sandbox::new();
    let inbox = sandbox.add(&["-c", "work", "Read", "the", "RFC"]);
    let task = inbox.summary.task.as_ref().unwrap();
    assert_eq!(task.title, "Read the RFC");
    assert_eq!(task.status, Status::Inbox);
    assert_eq!(task.kind.name(), "chore");

    let bug = sandbox.add(&[
        "-c",
        "work",
        "--ready",
        "--kind",
        "bug",
        "--due",
        "tomorrow",
        "Login timeout",
    ]);
    let bug_task = bug.summary.task.as_ref().unwrap();
    assert_eq!(bug_task.status, Status::Ready);
    assert_eq!(bug_task.due, Some("2026-10-15".parse().unwrap()));
    let child = sandbox.add(&[
        "-c",
        "work",
        "--ready",
        "--importance",
        "p0",
        "--parent",
        &bug.summary.prefix,
        "Reproduce on iOS",
    ]);
    assert_eq!(
        child.summary.task.as_ref().unwrap().relations.parent,
        Some(bug.summary.id.clone())
    );

    // The priority list: inbox left out, p0 first, then the bug due tomorrow.
    let list: TaskList =
        serde_json::from_str(&sandbox.ok(&["-c", "work", "ls", "--json"])).unwrap();
    let ids: Vec<_> = list.tasks.iter().map(|task| task.id.clone()).collect();
    assert_eq!(ids, [child.summary.id, bug.summary.id.clone()]);
    assert_eq!(list.tasks[1].rank.unwrap().reason, RankReason::Importance);

    let why = sandbox.ok(&["-c", "work", "ls", "--why"]);
    let lines: Vec<&str> = why.lines().collect();
    assert_eq!(lines.len(), 3, "{why}");
    assert!(lines[0].ends_with("WHY"), "{why}");
    assert!(lines[1].ends_with("Reproduce on iOS  first"), "{why}");
    assert!(
        lines[2].ends_with("Login timeout     more important"),
        "{why}"
    );

    let inbox_list = sandbox.ok(&["-c", "work", "ls", "--status", "inbox"]);
    assert!(inbox_list.contains("Read the RFC"), "{inbox_list}");
    assert!(!inbox_list.contains("Login timeout"), "{inbox_list}");

    // Another context is not listed, unless `--all-contexts`.
    sandbox.add(&["-c", "home", "--ready", "Water the plants"]);
    assert!(
        !sandbox
            .ok(&["-c", "work", "ls"])
            .contains("Water the plants")
    );
    assert!(
        sandbox
            .ok(&["ls", "--all-contexts"])
            .contains("Water the plants")
    );

    let shown = sandbox.ok(&["show", &bug.summary.prefix]);
    assert!(
        shown.starts_with(&format!(
            "id:         {} ({})\n",
            bug.summary.id, bug.summary.prefix
        )),
        "{shown}"
    );
    assert!(shown.contains("\ndue:        2026-10-15\n"), "{shown}");
    let detail: TaskDetail =
        serde_json::from_str(&sandbox.ok(&["show", "--json", &bug.summary.id.to_string()]))
            .unwrap();
    assert_eq!(detail.summary.id, bug.summary.id);
}

#[test]
fn exit_codes_follow_the_cli_conventions() {
    let sandbox = Sandbox::new();
    // Not found: 1.
    let output = sandbox.run(&sandbox.home(), None, &["show", "zzzzzzzz"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    // Usage errors: 2.
    for args in [
        &["ls", "--status", "blocked"][..],
        &["ls", "--why", "--status", "inbox"],
        &["add"],
        &["add", "--kind", "epic", "X"],
    ] {
        let output = sandbox.run(&sandbox.home(), None, args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    // Rejected before loomd is asked: 1.
    for args in [
        &["add", "--format", "talk", "X"][..],
        &["add", "--due", "someday", "X"],
        &["-c", "work", "ls", "--all-contexts"],
    ] {
        let output = sandbox.run(&sandbox.home(), None, args);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn a_repository_not_indexed_yet_gets_its_context_from_its_remote() {
    let sandbox = Sandbox::new();
    // loomd starts and scans before the clone exists.
    sandbox.add(&["-c", "personal", "Warm up"]);
    let repo = sandbox.home().join("perso").join("fresh");
    fs::create_dir_all(&repo).unwrap();
    git(&sandbox.home(), &repo, &["init", "-q"]);
    git(
        &sandbox.home(),
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "git@gitlab.example.com:company-a/fresh.git",
        ],
    );
    // Under `~/perso` (`home` by path), but the remote says `work`.
    let task = sandbox.add_in(&repo, None, &["First task of the clone"]);
    assert_eq!(context_of(&task), "work");
}

#[test]
fn a_task_shares_the_context_of_its_parent() {
    let sandbox = Sandbox::new();
    let parent = sandbox.add(&["-c", "work", "Hardening"]);
    let prefix = parent.summary.prefix;
    // Inferred context (`default_context` here): the parent's wins.
    let child = sandbox.add(&["--parent", &prefix, "Rate limits"]);
    assert_eq!(context_of(&child), "work");
    // Chosen explicitly: another context is refused.
    for (context, args) in [
        (
            None,
            vec!["add", "-c", "personal", "--parent", &prefix, "X"],
        ),
        (Some("personal"), vec!["add", "--parent", &prefix, "X"]),
    ] {
        let output = sandbox.run(&sandbox.home(), context, &args);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("is in context work, not personal"),
            "{}",
            stderr(&output)
        );
    }
}

#[test]
fn malformed_ids_are_usage_errors() {
    let sandbox = Sandbox::new();
    for args in [
        &["show", "a b"][..],
        &["show", "a/b"],
        &["show", "abc?x"],
        &["show", "k3q7zm2a9"],
        &["add", "--parent", "x/y", "Child"],
    ] {
        let output = sandbox.run(&sandbox.home(), None, args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("is not a task ID or prefix"),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}
