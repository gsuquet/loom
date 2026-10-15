//! Recurring tasks end to end: a real `loomd` with a schedule in `config.toml`, on a temporary
//! `HOME` and loom home, restarted on chosen dates (`LOOM_TEST_TODAY`).

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Instant;

use loom_api::{Actor, Link, StatusName, TaskEvents, TaskList, TaskSummary};
use tempfile::TempDir;

mod common;

use common::{Daemon, POLL, TIMEOUT, ok, request};

const CONFIG: &str = r#"
[repos]
roots = []

[contexts.work]

[[contexts.work.schedules]]
id = "weekly-review"
title = "Weekly review"
kind = "chore"
rrule = "FREQ=WEEKLY;BYDAY=FR"
policy = "replace"
due = "same-day"
"#;

struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        // Under `/tmp`: the macOS default `$TMPDIR` is too long for a socket path.
        let dir = tempfile::Builder::new()
            .prefix("loomd-schedules-")
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

    /// Starts loomd on `today`.
    fn start(&self, today: &str) -> Daemon {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
        command
            .env("LOOM_HOME", self.loom_home())
            .env("HOME", self.user_home())
            .env("XDG_CONFIG_HOME", self.user_home().join(".config"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LOOM_TEST_TODAY", today)
            .arg("--color=never")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Daemon::start(&mut command, &self.socket())
    }
}

fn tasks(socket: &Path) -> Vec<TaskSummary> {
    let list: TaskList = ok(&request(socket, "GET", "/v1/tasks?context=work", None));
    list.tasks
}

/// The tasks once there are `count` of them: the first run of the schedules is not awaited by
/// the reads of the index.
fn wait_for(socket: &Path, count: usize) -> Vec<TaskSummary> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let tasks = tasks(socket);
        if tasks.len() >= count {
            return tasks;
        }
        assert!(
            Instant::now() < deadline,
            "{} tasks, {count} expected",
            tasks.len()
        );
        thread::sleep(POLL);
    }
}

/// The events of task `id`. Read on the task thread, so after the run of the schedules queued
/// at start.
fn events(socket: &Path, id: &str) -> TaskEvents {
    ok(&request(
        socket,
        "GET",
        &format!("/v1/tasks/{id}/events"),
        None,
    ))
}

fn origin_key(summary: &TaskSummary) -> String {
    let task = summary.task.as_ref().unwrap();
    match task.links.as_slice() {
        [Link::Origin(origin)] => format!("{}:{}", origin.provider, origin.key),
        links => panic!("one origin link expected: {links:?}"),
    }
}

#[test]
fn a_schedule_creates_one_task_per_occurrence_across_restarts() {
    let sandbox = Sandbox::new();
    let socket = sandbox.socket();

    // Friday: the occurrence of the day.
    let daemon = sandbox.start("2026-10-16");
    let [first] = wait_for(&socket, 1).try_into().unwrap();
    let task = first.task.clone().unwrap();
    assert_eq!(task.title, "Weekly review");
    assert_eq!(task.status.name(), Some(StatusName::Ready));
    assert_eq!(task.due, Some("2026-10-16".parse().unwrap()));
    assert_eq!(origin_key(&first), "schedule:weekly-review/2026-10-16");
    let created = events(&socket, first.id.as_str());
    assert_eq!(created.events[0].actor, Actor::Daemon);
    daemon.stop();

    // A restart the same day, then the day after: nothing new.
    for today in ["2026-10-16", "2026-10-17"] {
        let daemon = sandbox.start(today);
        events(&socket, first.id.as_str());
        assert_eq!(tasks(&socket).len(), 1, "{today}");
        daemon.stop();
    }

    // Two Fridays later, the first still open: one task for the latest Friday, the first
    // cancelled (policy `replace`).
    let daemon = sandbox.start("2026-10-30");
    events(&socket, first.id.as_str());
    let mut both = tasks(&socket);
    both.sort_by_key(origin_key);
    let keys: Vec<String> = both.iter().map(origin_key).collect();
    assert_eq!(
        keys,
        [
            "schedule:weekly-review/2026-10-16",
            "schedule:weekly-review/2026-10-30"
        ]
    );
    let status = |summary: &TaskSummary| summary.task.as_ref().unwrap().status.name();
    assert_eq!(status(&both[0]), Some(StatusName::Cancelled));
    assert_eq!(status(&both[1]), Some(StatusName::Ready));
    daemon.stop();
}
