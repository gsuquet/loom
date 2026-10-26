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

use loom_api::{FocusList, RankReason, Status, TaskDetail, TaskList};
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
        self.run_env(cwd, context, &[], args)
    }

    /// [`Self::run`] with the variables `env` set as well.
    fn run_env(
        &self,
        cwd: &Path,
        context: Option<&str>,
        env: &[(&str, &str)],
        args: &[&str],
    ) -> Output {
        let mut command = Command::new(loom_bin());
        command
            .env("LOOM_HOME", self.loom_home())
            .envs(isolation::isolated_env(&self.home()))
            .env("PWD", cwd)
            .env("LOOM_TEST_TODAY", TODAY)
            .env_remove("LOOM_CONTEXT")
            .env_remove("VISUAL")
            .env_remove("EDITOR")
            .env_remove("NO_COLOR")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
            .current_dir(cwd)
            .arg("--color=never")
            .args(args)
            .stdin(Stdio::null());
        if let Some(context) = context {
            command.env("LOOM_CONTEXT", context);
        }
        command.envs(env.iter().copied());
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

    // The full ID: a prefix unique when the task was created may be shared by a later one.
    let shown = sandbox.ok(&["show", bug.summary.id.as_str()]);
    assert!(
        shown.starts_with(&format!("id:         {} (", bug.summary.id)),
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
    // The full ID: a prefix unique when the parent was created may be shared by a later task.
    let prefix = parent.summary.id.to_string();
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

/// `loom <args> --json` in the user home: the task it answers.
fn gesture(sandbox: &Sandbox, args: &[&str]) -> TaskDetail {
    let mut full = args.to_vec();
    full.push("--json");
    serde_json::from_str(&sandbox.ok(&full)).unwrap()
}

#[test]
fn a_task_is_changed_closed_snoozed_and_undone() {
    let sandbox = Sandbox::new();
    let task = sandbox.add(&["-c", "work", "--ready", "--kind", "bug", "Login timeout"]);
    let id = task.summary.id.to_string();
    let other = sandbox.add(&["-c", "work", "Hardening"]).summary.id;

    let set = gesture(
        &sandbox,
        &[
            "set",
            &id,
            "title=Login timeout on checkout",
            "importance=p0",
            "due=+3d",
            "pinned=yes",
            &format!("parent={other}"),
        ],
    );
    let changed = set.summary.task.unwrap();
    assert_eq!(changed.title, "Login timeout on checkout");
    assert_eq!(changed.importance, loom_api::Importance::P0);
    assert_eq!(changed.due, Some("2026-10-17".parse().unwrap()));
    assert!(changed.pinned);
    assert_eq!(changed.relations.parent, Some(other));
    let cleared = gesture(&sandbox, &["set", &id, "parent=none", "due=none"]);
    let cleared = cleared.summary.task.unwrap();
    assert_eq!(cleared.relations.parent, None);
    assert_eq!(cleared.due, None);

    // `status=` is the same gesture as `loom done`; `waiting` belongs to `loom snooze`.
    let active = gesture(&sandbox, &["set", &id, "status=active"]);
    assert_eq!(active.summary.task.unwrap().status, Status::Active);
    let refused = sandbox.run(&sandbox.home(), None, &["set", &id, "status=waiting"]);
    assert_ne!(refused.status.code(), Some(0));
    assert!(
        stderr(&refused).contains("loom snooze"),
        "{}",
        stderr(&refused)
    );
    gesture(&sandbox, &["set", &id, "status=ready"]);

    // One line per gesture.
    let line = sandbox.ok(&["done", &id]);
    assert!(
        line.ends_with(" work done Login timeout on checkout\n"),
        "{line}"
    );
    let undone = gesture(&sandbox, &["undo", &id]);
    assert_eq!(undone.summary.task.unwrap().status, Status::Ready);

    let snoozed = gesture(&sandbox, &["snooze", &id, "+1w", "--on", "Alice"]);
    assert_eq!(
        snoozed.summary.task.unwrap().status,
        Status::Waiting {
            on: Some("Alice".to_owned()),
            until: Some("2026-10-21".parse().unwrap()),
        }
    );
    assert!(!sandbox.ok(&["-c", "work", "ls"]).contains("Login timeout"));

    let cancelled = gesture(&sandbox, &["cancel", &id]);
    assert_eq!(cancelled.summary.task.unwrap().status, Status::Cancelled);
    // `cancelled` moves to `ready` only: the refusal of loomd, exit 1.
    let output = sandbox.run(&sandbox.home(), None, &["done", &id]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("illegal_transition"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_inbox_task_is_accepted_with_fields_and_undone() {
    let sandbox = Sandbox::new();
    let task = sandbox.add(&["-c", "work", "Triage me"]);
    let id = task.summary.id.to_string();
    assert_eq!(task.summary.task.unwrap().status, Status::Inbox);

    // Accept with new kind, importance, due
    let accepted = gesture(
        &sandbox,
        &[
            "accept",
            &id,
            "--kind",
            "story",
            "--importance",
            "p1",
            "--due",
            "2026-10-30",
        ],
    );
    let accepted_task = accepted.summary.task.unwrap();
    assert_eq!(accepted_task.status, Status::Ready);
    assert_eq!(accepted_task.kind.name(), "story");
    assert_eq!(accepted_task.importance, loom_api::Importance::P1);
    assert_eq!(accepted_task.due, Some("2026-10-30".parse().unwrap()));

    // Undo reverts back to Inbox and restores original fields
    let undone = gesture(&sandbox, &["undo", &id]);
    let undone_task = undone.summary.task.unwrap();
    assert_eq!(undone_task.status, Status::Inbox);
    assert_eq!(undone_task.kind.name(), "chore");
    assert_eq!(undone_task.importance, loom_api::Importance::P3);
    assert_eq!(undone_task.due, None);

    // Accept with no fields moves status to ready
    let accepted_plain = gesture(&sandbox, &["accept", &id]);
    assert_eq!(accepted_plain.summary.task.unwrap().status, Status::Ready);

    // Accepting a ready task (even with no fields) is refused as illegal transition
    let output = sandbox.run(&sandbox.home(), None, &["accept", &id]);
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("illegal_transition") || err.contains("cannot move"),
        "unexpected error: {err}"
    );

    // Marking a task done then calling accept without fields is also refused
    let done_task = sandbox.add(&["-c", "work", "--ready", "Already done"]);
    let done_id = done_task.summary.id.to_string();
    gesture(&sandbox, &["done", &done_id]);
    let output_done = sandbox.run(&sandbox.home(), None, &["accept", &done_id]);
    assert_eq!(output_done.status.code(), Some(1));
    let err_done = stderr(&output_done);
    assert!(
        err_done.contains("illegal_transition") || err_done.contains("cannot move"),
        "unexpected error: {err_done}"
    );

    // Undo reverts back to Inbox
    let undone_plain = gesture(&sandbox, &["undo", &id]);
    assert_eq!(undone_plain.summary.task.unwrap().status, Status::Inbox);

    // Accept with kind only defaults importance to kind's default
    let accepted_bug = gesture(
        &sandbox,
        &["accept", &id, "--kind", "bug", "--due", "2026-10-30"],
    );
    let bug_task = accepted_bug.summary.task.unwrap();
    assert_eq!(bug_task.status, Status::Ready);
    assert_eq!(bug_task.kind.name(), "bug");
    assert_eq!(bug_task.importance, loom_api::Importance::P1);
    assert_eq!(bug_task.due, Some("2026-10-30".parse().unwrap()));

    // Undo reverts back to Inbox
    gesture(&sandbox, &["undo", &id]);

    // Manually set importance to p0 with set, then accept with same kind (--kind bug):
    // importance remains p0 rather than being overwritten with bug's default (p1)
    gesture(&sandbox, &["set", &id, "kind=bug", "importance=p0"]);
    let accepted_same_kind = gesture(&sandbox, &["accept", &id, "--kind", "bug"]);
    let same_kind_task = accepted_same_kind.summary.task.unwrap();
    assert_eq!(same_kind_task.status, Status::Ready);
    assert_eq!(same_kind_task.kind.name(), "bug");
    assert_eq!(same_kind_task.importance, loom_api::Importance::P0);

    // Undo reverts back to Inbox
    gesture(&sandbox, &["undo", &id]);

    // Accept with due none clears due date
    let accepted_cleared = gesture(&sandbox, &["accept", &id, "--due", "none"]);
    assert_eq!(accepted_cleared.summary.task.unwrap().due, None);
}

#[test]
fn set_arguments_are_checked_before_loomd() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(&["-c", "work", "X"]).summary.id.to_string();
    for args in [
        &["set", &id][..],
        &["set", &id, "colour=red"],
        &["set", &id, "title"],
    ] {
        let output = sandbox.run(&sandbox.home(), None, args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    for args in [
        &["set", &id, "format=talk"][..],
        &["set", &id, "due=soon"],
        &["set", &id, "due=today", "due=none"],
        &["snooze", &id, "later"],
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

/// An editor script in the sandbox: `body` runs with the file as `$1`.
fn editor(sandbox: &Sandbox, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    let path = sandbox.dir.path().join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

#[test]
fn edit_opens_the_task_file_and_reports_an_invalid_one() {
    let sandbox = Sandbox::new();
    let task = sandbox.add(&["-c", "work", "Original"]);
    let id = task.summary.id.to_string();
    let file = sandbox
        .loom_home()
        .join("contexts/work/tasks")
        .join(&id)
        .join("task.md");

    let unchanged = sandbox.run_env(&sandbox.home(), None, &[("EDITOR", "true")], &["edit", &id]);
    assert_eq!(unchanged.status.code(), Some(0), "{}", stderr(&unchanged));
    assert!(String::from_utf8_lossy(&unchanged.stdout).ends_with(" unchanged\n"));

    // `VISUAL` first; the editor gets the file as its argument.
    let retitle = editor(
        &sandbox,
        "retitle",
        r#"t=$(mktemp) && sed 's/^title = .*/title = "Edited by hand"/' "$1" > "$t" && cat "$t" > "$1" && rm "$t""#,
    );
    let edited = sandbox.run_env(
        &sandbox.home(),
        None,
        &[("VISUAL", &retitle), ("EDITOR", "false")],
        &["edit", &id],
    );
    assert_eq!(edited.status.code(), Some(0), "{}", stderr(&edited));
    let line = String::from_utf8_lossy(&edited.stdout).into_owned();
    assert!(line.ends_with(" work inbox Edited by hand\n"), "{line}");

    let breaker = editor(&sandbox, "breaker", r#"printf 'not a task file\n' > "$1""#);
    let broken = sandbox.run_env(
        &sandbox.home(),
        None,
        &[("EDITOR", &breaker)],
        &["edit", &id],
    );
    assert_eq!(broken.status.code(), Some(1), "{}", stderr(&broken));
    assert!(stderr(&broken).contains("task.md"), "{}", stderr(&broken));
    assert!(
        stderr(&broken).contains("kept as saved"),
        "{}",
        stderr(&broken)
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "not a task file\n");

    // A failing editor changes nothing and says so.
    let failing = sandbox.run_env(
        &sandbox.home(),
        None,
        &[("EDITOR", "false")],
        &["edit", &id],
    );
    assert_eq!(failing.status.code(), Some(1), "{}", stderr(&failing));
    assert!(
        stderr(&failing).contains("exited with"),
        "{}",
        stderr(&failing)
    );
}

#[test]
fn edit_warns_when_the_task_changed_while_the_editor_was_open() {
    let sandbox = Sandbox::new();
    let task = sandbox.add(&["-c", "work", "--ready", "Original"]);
    let id = task.summary.id.to_string();
    // The editor reads its buffer, another terminal closes the task, then the editor saves
    // the buffer it read, with a new title.
    let concurrent = editor(
        &sandbox,
        "concurrent",
        &format!(
            r#"buf=$(mktemp) && cp "$1" "$buf" && "$LOOM_BIN" --color=never done {id} > /dev/null && sed 's/^title = .*/title = "Saved over"/' "$buf" > "$1" && rm "$buf""#
        ),
    );
    let loom = loom_bin().display().to_string();
    let output = sandbox.run_env(
        &sandbox.home(),
        None,
        &[("EDITOR", &concurrent), ("LOOM_BIN", &loom)],
        &["edit", &id],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let warning = stderr(&output);
    assert!(
        warning.contains("changed while it was open in the editor (by user)"),
        "{warning}"
    );
    assert!(warning.contains("loom undo"), "{warning}");
    // The save won: the status is back to ready, recorded as a hand edit.
    let line = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(line.ends_with(" work ready Saved over\n"), "{line}");
}

#[test]
fn archived_tasks_are_listed_apart_shown_and_read_only() {
    let sandbox = Sandbox::new();
    let open = sandbox.add(&["-c", "work", "--ready", "Renew the domain"]);
    // A done task already in the archive of 2026.
    let archived = sandbox
        .loom_home()
        .join("contexts/work/archive/2026/k3q7zm2a");
    fs::create_dir_all(&archived).unwrap();
    fs::write(
        archived.join("task.md"),
        "+++\nid = \"k3q7zm2a\"\ntitle = \"Rotate the certificates\"\nkind = \"chore\"\ncontext = \"work\"\nstatus = \"done\"\nimportance = \"p3\"\ncreated = 2026-10-12T09:14:00Z\n+++\n\n## Notes\n",
    )
    .unwrap();
    let ids = |args: &[&str]| -> Vec<String> {
        let list: TaskList = serde_json::from_str(&sandbox.ok(args)).unwrap();
        list.tasks
            .into_iter()
            .map(|summary| summary.id.to_string())
            .collect()
    };
    // Picked up by the watcher: wait for it, the whole file read (a file read during its write
    // is indexed as invalid, without a status, for a moment).
    let deadline = Instant::now() + TIMEOUT;
    while ids(&[
        "ls",
        "-c",
        "work",
        "--archived",
        "--status",
        "done",
        "--json",
    ])
    .is_empty()
    {
        assert!(
            Instant::now() < deadline,
            "the archived task is not indexed"
        );
        thread::sleep(POLL);
    }
    assert_eq!(
        ids(&["ls", "-c", "work", "--json"]),
        [open.summary.id.to_string()]
    );
    assert_eq!(
        ids(&[
            "ls",
            "-c",
            "work",
            "--archived",
            "--status",
            "done",
            "--json"
        ]),
        ["k3q7zm2a"]
    );
    let table = sandbox.ok(&["ls", "-c", "work", "--archived"]);
    assert!(table.contains("Rotate the certificates"), "{table}");
    assert!(!table.contains("Renew the domain"), "{table}");

    let shown = sandbox.ok(&["show", "k3q7"]);
    assert!(
        shown.contains("read-only:  archived in archive/2026"),
        "{shown}"
    );

    // Reopening, or any change, is refused with the reason.
    for args in [
        &["set", "k3q7", "title=Renamed"][..],
        &["undo", "k3q7"],
        &["cancel", "k3q7"],
    ] {
        let output = sandbox.run(&sandbox.home(), None, args);
        assert_ne!(output.status.code(), Some(0), "{args:?}");
        let error = stderr(&output);
        assert!(
            error.contains("is archived (archive/2026)"),
            "{args:?}: {error}"
        );
    }
    let edit = sandbox.run_env(
        &sandbox.home(),
        None,
        &[("EDITOR", "true")],
        &["edit", "k3q7"],
    );
    assert_ne!(edit.status.code(), Some(0));
    let error = stderr(&edit);
    assert!(
        error.contains("is archived (archive/2026)") && error.contains("move its directory back"),
        "{error}"
    );

    let usage = sandbox.run(&sandbox.home(), None, &["ls", "--archived", "--why"]);
    assert_eq!(usage.status.code(), Some(2), "{}", stderr(&usage));
}

#[test]
fn edit_of_the_notes_only_or_of_the_spacing_only_is_shown_at_once() {
    let sandbox = Sandbox::new();
    let id = sandbox
        .add(&["-c", "work", "Original"])
        .summary
        .id
        .to_string();

    // Notes only: no event, the index holds the new notes.
    let notes = editor(&sandbox, "notes", r#"printf 'Asked Alice.\n' >> "$1""#);
    let output = sandbox.run_env(&sandbox.home(), None, &[("EDITOR", &notes)], &["edit", &id]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).ends_with(" work inbox Original\n"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(sandbox.ok(&["show", &id]).contains("Asked Alice."));

    // Spacing only: the same task, shown at once, not after the 5 s wait.
    let spacing = editor(
        &sandbox,
        "spacing",
        r#"t=$(mktemp) && sed 's/^title = /title  =  /' "$1" > "$t" && cat "$t" > "$1" && rm "$t""#,
    );
    let started = Instant::now();
    let output = sandbox.run_env(
        &sandbox.home(),
        None,
        &[("EDITOR", &spacing)],
        &["edit", &id],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let line = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(line.ends_with(" work inbox Original\n"), "{line}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn loom_go_on_a_task_without_a_repo_says_so() {
    let sandbox = Sandbox::new();
    let detail = sandbox.add(&["-c", "work", "No repo task"]);
    let id = detail.summary.id.to_string();
    let prefix = detail.summary.prefix;

    let output = sandbox.run(&sandbox.home(), None, &["go", &id]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(&format!(
            "no repository linked to {prefix}: staying in the current directory"
        )),
        "expected notice in stdout: {stdout}"
    );
    assert!(
        stdout.contains("active No repo task"),
        "expected activated status in stdout: {stdout}"
    );
}

#[test]
fn loom_log_with_no_focus_fails_with_a_hint() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&sandbox.home(), None, &["log", "some note"]);
    assert_ne!(output.status.code(), Some(0));
    let err = stderr(&output);
    assert!(
        err.contains("no focus task in context personal: run `loom go <id>` to focus a task, or pass `--task <id>`"),
        "expected hint in stderr: {err}"
    );
}

#[test]
fn two_contexts_keep_two_separate_focuses_checked_through_the_cli() {
    let sandbox = Sandbox::new();
    let work_task = sandbox.add(&["-c", "work", "Work focus task"]);
    let home_task = sandbox.add(&["-c", "home", "Home focus task"]);
    let work_id = work_task.summary.id.to_string();
    let home_id = home_task.summary.id.to_string();

    // 1. Focus task in `work` context
    let work_go = sandbox.ok(&["-c", "work", "go", &work_id]);
    assert!(work_go.contains("active Work focus task"));

    // 2. Focus task in `home` context
    let home_go = sandbox.ok(&["-c", "home", "go", &home_id]);
    assert!(home_go.contains("active Home focus task"));

    // 3. `loom focus --json` returns both focuses
    let focus_json = sandbox.ok(&["focus", "--json"]);
    let list: FocusList = serde_json::from_str(&focus_json).unwrap();
    assert_eq!(list.entries.len(), 2);
    let work_entry = list
        .entries
        .iter()
        .find(|e| e.context.as_str() == "work")
        .unwrap();
    assert_eq!(work_entry.task.id.as_str(), work_id);
    let home_entry = list
        .entries
        .iter()
        .find(|e| e.context.as_str() == "home")
        .unwrap();
    assert_eq!(home_entry.task.id.as_str(), home_id);

    // 4. `loom -c work focus` only outputs work focus, `loom -c home focus` only outputs home focus
    let work_focus = sandbox.ok(&["-c", "work", "focus"]);
    assert!(work_focus.contains("work"));
    assert!(work_focus.contains("Work focus task"));
    assert!(!work_focus.contains("Home focus task"));

    let home_focus = sandbox.ok(&["-c", "home", "focus"]);
    assert!(home_focus.contains("home"));
    assert!(home_focus.contains("Home focus task"));
    assert!(!home_focus.contains("Work focus task"));
}

#[test]
fn set_status_follows_the_transitions_of_the_other_gestures() {
    let sandbox = Sandbox::new();
    let id = sandbox
        .add(&["-c", "work", "Move me"])
        .summary
        .id
        .to_string();
    let status = |args: &[&str]| gesture(&sandbox, args).summary.task.unwrap().status;
    let refused = |value: &str| {
        let output = sandbox.run(&sandbox.home(), None, &["set", &id, value]);
        assert_eq!(output.status.code(), Some(1), "{value}");
        stderr(&output)
    };

    // An inbox task moves to ready, back to inbox, and to active.
    assert_eq!(status(&["set", &id, "status=ready"]), Status::Ready);
    assert_eq!(status(&["set", &id, "status=inbox"]), Status::Inbox);
    assert_eq!(status(&["set", &id, "status=active"]), Status::Active);
    // Same status: loomd refuses, like `loom done` on a done task.
    let message = refused("status=active");
    assert_ne!(message.trim(), "");
    assert_eq!(status(&["show", &id]), Status::Active);

    assert_eq!(status(&["set", &id, "status=done"]), Status::Done);
    // A closed task only reopens to ready: the refusal text reaches standard error.
    let message = refused("status=active");
    assert!(message.contains("done"), "{message}");
    assert_eq!(status(&["set", &id, "status=ready"]), Status::Ready);
    assert_eq!(status(&["set", &id, "status=cancelled"]), Status::Cancelled);

    // Each step is its own event: the title stays changed when the status is refused.
    let message = refused("status=inbox");
    assert_ne!(message.trim(), "");
    let output = sandbox.run(
        &sandbox.home(),
        None,
        &["set", &id, "title=Renamed", "status=inbox"],
    );
    assert_eq!(output.status.code(), Some(1));
    let shown = gesture(&sandbox, &["show", &id]).summary.task.unwrap();
    assert_eq!(shown.title, "Renamed");
    assert_eq!(shown.status, Status::Cancelled);
}

#[test]
fn set_context_moves_the_task_after_the_other_changes() {
    let sandbox = Sandbox::new();
    let parent = sandbox
        .add(&["-c", "work", "Parent"])
        .summary
        .id
        .to_string();
    let id = sandbox.add(&["-c", "work", "Child"]).summary.id.to_string();
    gesture(&sandbox, &["set", &id, &format!("parent={parent}")]);

    // A task with a parent does not move; the message says what to do.
    // It is refused before the other changes of the command are applied.
    let refused = sandbox.run(
        &sandbox.home(),
        None,
        &["set", &id, "title=Renamed", "context=home"],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr(&refused).contains("has a parent"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(
        gesture(&sandbox, &["show", &id])
            .summary
            .task
            .unwrap()
            .title,
        "Child"
    );
    assert_eq!(
        gesture(&sandbox, &["show", &id]).summary.context.as_str(),
        "work"
    );

    // Detached in the same command: the parent is cleared first, then the task moves.
    let moved = gesture(
        &sandbox,
        &["set", &id, "context=home", "parent=none", "importance=p0"],
    );
    assert_eq!(moved.summary.id.to_string(), id);
    assert_eq!(moved.summary.context.as_str(), "home");
    let task = moved.summary.task.unwrap();
    assert_eq!(task.importance, loom_api::Importance::P0);
    assert_eq!(task.relations.parent, None);

    // The same context is a no-op, like the other values already set.
    let again = gesture(&sandbox, &["set", &id, "context=home"]);
    assert_eq!(again.summary.context.as_str(), "home");
    // An unknown context, and a name that is not one.
    for bad in ["context=nowhere", "context=Not A Name"] {
        let output = sandbox.run(&sandbox.home(), None, &["set", &id, bad]);
        assert_eq!(output.status.code(), Some(1), "{bad}");
    }
}
