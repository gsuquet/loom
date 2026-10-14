//! The tasks API end to end: a real `loomd` on a temporary `HOME` and loom home, queried over
//! its unix socket. Nominal paths, each typed problem, and concurrent changes of one task.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use loom_api::{
    Action, Actor, AmbiguousPrefix, ApiError, ErrorCode, Event, IllegalTransition, StatusName,
    TaskDetail, TaskEvents, TaskList, TaskReadOnly,
};
use serde_json::{Value, json};
use tempfile::TempDir;

mod common;

use common::{Daemon, HttpResponse, ok, request};

const CONFIG: &str = r"
[repos]
roots = []

[contexts.work]

[contexts.home]
";

/// The date of the priority list in these tests.
const TODAY: &str = "2026-10-14";

/// A `task.md` of task `id` in `context`, with `status` (a TOML value) and `importance`.
fn task_md(id: &str, context: &str, status: &str) -> String {
    format!(
        "+++\nid = \"{id}\"\ntitle = \"Task {id}\"\nkind = \"chore\"\ncontext = \"{context}\"\nstatus = \"{status}\"\nimportance = \"p3\"\ncreated = 2026-10-12T09:14:00Z\n+++\n\n## Notes\n"
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
            .prefix("loomd-tasks-api-")
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

    /// Writes `task.md` of task `id` in the store of `context`, before the daemon starts.
    fn write_task(&self, context: &str, id: &str, text: &str) {
        let dir = self
            .loom_home()
            .join("contexts")
            .join(context)
            .join("tasks")
            .join(id);
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
            .env("LOOM_TEST_TODAY", TODAY)
            .arg("--color=never")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Daemon::start(&mut command, &self.socket())
    }
}

fn call(socket: &Path, method: &str, path: &str, body: Option<&Value>) -> HttpResponse {
    request(socket, method, path, body)
}

/// The error of `response`, which must have `status`, checked against the `ApiError` schema.
fn problem(response: &HttpResponse, status: u16) -> ApiError {
    assert_eq!(response.status, status, "{}", response.body);
    let value: Value = serde_json::from_str(&response.body).unwrap();
    let schema = serde_json::to_value(schemars::schema_for!(ApiError)).unwrap();
    assert!(jsonschema::is_valid(&schema, &value), "{value}");
    serde_json::from_value(value).unwrap()
}

/// The type of each event of `events`, in order.
fn event_types(events: &TaskEvents) -> Vec<&'static str> {
    events
        .events
        .iter()
        .map(|record| match &record.event {
            Event::Created { .. } => "created",
            Event::Edited { .. } => "edited",
            Event::StatusChanged(_) => "status_changed",
            Event::NoteLogged { .. } => "note_logged",
            Event::Undone { .. } => "undone",
        })
        .collect()
}

fn create(socket: &Path, body: &Value) -> TaskDetail {
    ok(&call(socket, "POST", "/v1/tasks", Some(body)))
}

#[test]
fn a_task_is_created_read_changed_and_undone() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();

    let created = create(
        &socket,
        &json!({
            "title": "Review the API",
            "kind": "review",
            "context": "work",
            "status": "inbox",
            "due": "2026-10-20",
        }),
    );
    let id = created.summary.id.clone();
    let task = created.summary.task.clone().unwrap();
    assert_eq!(task.title, "Review the API");
    assert_eq!(created.summary.prefix, &id.as_str()[..1]);
    assert!(
        created
            .actions
            .contains(&Action::SetStatus(StatusName::Ready))
    );

    // By prefix, in any case.
    let prefix = id.as_str()[..3].to_uppercase();
    let read: TaskDetail = ok(&call(&socket, "GET", &format!("/v1/tasks/{prefix}"), None));
    assert_eq!(read, created);

    let edited: TaskDetail = ok(&call(
        &socket,
        "PATCH",
        &format!("/v1/tasks/{id}"),
        Some(&json!({ "title": "Review the tasks API", "due": null, "importance": "p0" })),
    ));
    let task = edited.summary.task.unwrap();
    assert_eq!(task.title, "Review the tasks API");
    assert_eq!(task.due, None);

    let action = |action: &str, body: Option<&Value>| -> TaskDetail {
        ok(&call(
            &socket,
            "POST",
            &format!("/v1/tasks/{id}/actions/{action}"),
            body,
        ))
    };
    action("ready", None);
    let waiting = action(
        "waiting",
        Some(&json!({ "on": "Alice", "until": "2026-10-20" })),
    );
    assert_eq!(
        waiting.summary.task.unwrap().status.name(),
        Some(StatusName::Waiting)
    );
    action("log_note", Some(&json!({ "text": "Asked Alice" })));
    let pinned = action("pin", None);
    assert!(pinned.summary.task.unwrap().pinned);
    assert!(pinned.actions.contains(&Action::Unpin));

    let undone: TaskDetail = ok(&call(
        &socket,
        "POST",
        &format!("/v1/tasks/{id}/undo"),
        None,
    ));
    assert!(!undone.summary.task.unwrap().pinned);

    let events: TaskEvents = ok(&call(
        &socket,
        "GET",
        &format!("/v1/tasks/{id}/events"),
        None,
    ));
    assert_eq!(
        event_types(&events),
        [
            "created",
            "edited",
            "status_changed",
            "status_changed",
            "note_logged",
            "edited",
            "undone"
        ]
    );
    assert!(
        events
            .events
            .iter()
            .all(|record| record.actor == Actor::User)
    );

    let list: TaskList = ok(&call(&socket, "GET", "/v1/tasks?context=work", None));
    assert_eq!(list.tasks.len(), 1);
    let list: TaskList = ok(&call(&socket, "GET", "/v1/tasks?context=home", None));
    assert_eq!(list.tasks, []);
    daemon.stop();
}

#[test]
fn the_ranked_list_matches_rank() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    let specs = [
        json!({ "title": "Old chore", "kind": "chore", "context": "work", "status": "ready" }),
        json!({ "title": "Pinned", "kind": "story", "context": "home", "status": "active", "pinned": true }),
        json!({ "title": "Due today", "kind": "chore", "context": "work", "status": "ready", "due": TODAY }),
        json!({ "title": "Urgent bug", "kind": "bug", "context": "work", "status": "ready", "importance": "p0" }),
        json!({ "title": "Triage me", "kind": "bug", "context": "work", "status": "inbox" }),
        json!({ "title": "Later", "kind": "chore", "context": "work", "status": "waiting", "waiting_until": "2026-12-01" }),
        json!({ "title": "Back today", "kind": "chore", "context": "home", "status": "waiting", "waiting_until": TODAY }),
    ];
    for spec in &specs {
        create(&socket, spec);
    }
    let all: TaskList = ok(&call(&socket, "GET", "/v1/tasks", None));
    assert_eq!(all.tasks.len(), specs.len());
    let tasks: Vec<loom_api::Task> = all
        .tasks
        .iter()
        .map(|summary| summary.task.clone().unwrap())
        .collect();
    let expected = loom_core::rank(&tasks, TODAY.parse().unwrap());

    let ranked: TaskList = ok(&call(&socket, "GET", "/v1/tasks?ranked=true", None));
    let got: Vec<_> = ranked
        .tasks
        .iter()
        .map(|summary| {
            let rank = summary.rank.unwrap();
            (summary.id.clone(), rank.position, rank.reason)
        })
        .collect();
    let want: Vec<_> = expected
        .into_iter()
        .map(|ranked| (ranked.task_id, ranked.position, ranked.reason))
        .collect();
    assert_eq!(got, want);
    let mut titles: Vec<String> = ranked
        .tasks
        .iter()
        .map(|summary| summary.task.clone().unwrap().title)
        .collect();
    // The last two tie up to their age, which depends on the second each was created in.
    titles[3..].sort();
    assert_eq!(
        titles,
        [
            "Pinned",
            "Due today",
            "Urgent bug",
            "Back today",
            "Old chore"
        ]
    );

    let ranked_work: TaskList = ok(&call(
        &socket,
        "GET",
        "/v1/tasks?ranked=true&context=work",
        None,
    ));
    assert_eq!(ranked_work.tasks.len(), 3);
    let inbox: TaskList = ok(&call(&socket, "GET", "/v1/tasks?status=inbox", None));
    assert_eq!(inbox.tasks.len(), 1);
    daemon.stop();
}

#[test]
fn ids_and_states_give_typed_problems() {
    let sandbox = Sandbox::new();
    sandbox.write_task("work", "k3q7zm2a", &task_md("k3q7zm2a", "work", "done"));
    sandbox.write_task("home", "k3r00000", &task_md("k3r00000", "home", "blocked"));
    sandbox.write_task("work", "m0000000", "+++\nbroken\n+++\n");
    let daemon = sandbox.start();
    let socket = sandbox.socket();

    // Ambiguous prefix: the candidates.
    let error = problem(&call(&socket, "GET", "/v1/tasks/k3", None), 400);
    assert_eq!(error.code, ErrorCode::AmbiguousPrefix);
    let details: AmbiguousPrefix = serde_json::from_value(error.details.unwrap()).unwrap();
    let candidates: Vec<(String, String, String)> = details
        .candidates
        .into_iter()
        .map(|candidate| {
            (
                candidate.id.to_string(),
                candidate.context.to_string(),
                candidate.title,
            )
        })
        .collect();
    assert_eq!(
        candidates,
        [
            (
                "k3q7zm2a".to_owned(),
                "work".to_owned(),
                "Task k3q7zm2a".to_owned()
            ),
            (
                "k3r00000".to_owned(),
                "home".to_owned(),
                "Task k3r00000".to_owned()
            ),
        ]
    );
    let error = problem(
        &call(&socket, "POST", "/v1/tasks/k3/actions/pin", None),
        400,
    );
    assert_eq!(error.code, ErrorCode::AmbiguousPrefix);

    // Unknown and malformed IDs.
    let error = problem(&call(&socket, "GET", "/v1/tasks/zz", None), 404);
    assert_eq!(error.code, ErrorCode::NotFound);
    let error = problem(&call(&socket, "POST", "/v1/tasks/zz/undo", None), 404);
    assert_eq!(error.code, ErrorCode::NotFound);
    let error = problem(&call(&socket, "GET", "/v1/tasks/k3q7zm2a9", None), 400);
    assert_eq!(error.code, ErrorCode::InvalidRequest);

    // Illegal transition: done can only be reopened.
    let error = problem(
        &call(&socket, "POST", "/v1/tasks/k3q/actions/active", None),
        409,
    );
    assert_eq!(error.code, ErrorCode::IllegalTransition);
    let details: IllegalTransition = serde_json::from_value(error.details.unwrap()).unwrap();
    assert_eq!(
        details,
        IllegalTransition {
            from: StatusName::Done,
            to: StatusName::Active,
        }
    );

    // Read-only: unknown status, invalid file.
    let error = problem(
        &call(
            &socket,
            "PATCH",
            "/v1/tasks/k3r",
            Some(&json!({ "title": "x" })),
        ),
        409,
    );
    assert_eq!(error.code, ErrorCode::ReadOnly);
    let details: TaskReadOnly = serde_json::from_value(error.details.unwrap()).unwrap();
    assert_eq!(details, TaskReadOnly::UnknownStatus("blocked".to_owned()));
    let read: TaskDetail = ok(&call(&socket, "GET", "/v1/tasks/k3r", None));
    assert_eq!(read.actions, []);
    let error = problem(&call(&socket, "POST", "/v1/tasks/m/actions/pin", None), 409);
    assert_eq!(error.code, ErrorCode::ReadOnly);
    assert_eq!(error.details.unwrap()["reason"], json!("invalid_file"));
    let error = problem(&call(&socket, "POST", "/v1/tasks/m/undo", None), 409);
    assert_eq!(error.code, ErrorCode::ReadOnly);

    // Nothing to undo: the task has no event.
    let error = problem(&call(&socket, "POST", "/v1/tasks/k3q/undo", None), 409);
    assert_eq!(error.code, ErrorCode::NothingToUndo);

    daemon.stop();
}

#[test]
fn invalid_requests_are_refused() {
    let sandbox = Sandbox::new();
    sandbox.write_task("work", "k3q7zm2a", &task_md("k3q7zm2a", "work", "done"));
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    let bad = [
        call(
            &socket,
            "POST",
            "/v1/tasks",
            Some(
                &json!({ "title": "x", "kind": "chore", "context": "work", "status": "inbox", "importanc": "p0" }),
            ),
        ),
        call(
            &socket,
            "POST",
            "/v1/tasks",
            Some(&json!({ "title": "x", "kind": "chore", "context": "client", "status": "inbox" })),
        ),
        call(
            &socket,
            "POST",
            "/v1/tasks",
            Some(&json!({ "title": " ", "kind": "chore", "context": "work", "status": "inbox" })),
        ),
        call(
            &socket,
            "PATCH",
            "/v1/tasks/k3q",
            Some(&json!({ "status": "ready" })),
        ),
        call(
            &socket,
            "POST",
            "/v1/tasks/k3q/actions/log_note",
            Some(&json!({})),
        ),
        call(&socket, "GET", "/v1/tasks?status=blocked", None),
        call(&socket, "GET", "/v1/tasks?sort=id", None),
    ];
    for response in &bad {
        assert_eq!(problem(response, 400).code, ErrorCode::InvalidRequest);
    }
    let error = problem(
        &call(&socket, "POST", "/v1/tasks/k3q/actions/archive", None),
        404,
    );
    assert_eq!(error.code, ErrorCode::NotFound);
    daemon.stop();
}

#[test]
fn concurrent_changes_of_one_task_are_all_recorded() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    let created = create(
        &socket,
        &json!({ "title": "Busy", "kind": "chore", "context": "work", "status": "ready" }),
    );
    let id = created.summary.id.to_string();
    let writers: Vec<_> = (0..16)
        .map(|index| {
            let socket = socket.clone();
            let id = id.clone();
            thread::spawn(move || {
                let response = if index % 2 == 0 {
                    call(
                        &socket,
                        "POST",
                        &format!("/v1/tasks/{id}/actions/log_note"),
                        Some(&json!({ "text": format!("note {index}") })),
                    )
                } else {
                    call(
                        &socket,
                        "PATCH",
                        &format!("/v1/tasks/{id}"),
                        Some(&json!({ "title": format!("title {index}") })),
                    )
                };
                assert_eq!(response.status, 200, "{}", response.body);
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let events: TaskEvents = ok(&call(
        &socket,
        "GET",
        &format!("/v1/tasks/{id}/events"),
        None,
    ));
    let mut notes: Vec<String> = Vec::new();
    let mut titles = 0;
    for record in &events.events {
        match &record.event {
            Event::NoteLogged { text } => notes.push(text.clone()),
            Event::Edited { .. } => titles += 1,
            Event::Created { .. } | Event::StatusChanged(_) | Event::Undone { .. } => {}
        }
    }
    notes.sort();
    let mut expected: Vec<String> = (0..16).step_by(2).map(|i| format!("note {i}")).collect();
    expected.sort();
    assert_eq!(notes, expected);
    assert_eq!(titles, 8);
    assert_eq!(events.skipped, []);
    // The file holds the last title written, and the log replays into it.
    let read: TaskDetail = ok(&call(&socket, "GET", &format!("/v1/tasks/{id}"), None));
    let replayed = loom_core::replay(&events.events).unwrap();
    assert_eq!(read.summary.task.unwrap().title, replayed.title);
    daemon.stop();
}
