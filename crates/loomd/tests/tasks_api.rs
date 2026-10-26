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

#[test]
fn a_parent_is_a_task_of_the_same_context_and_never_a_descendant() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start();
    let socket = sandbox.socket();
    let new = |context: &str, parent: Option<&str>| {
        let mut body = json!({
            "title": "Task",
            "kind": "chore",
            "context": context,
            "status": "inbox",
        });
        if let Some(parent) = parent {
            body["relations"] = json!({ "parent": parent });
        }
        body
    };
    let root = create(&socket, &new("work", None)).summary.id;
    let child = create(&socket, &new("work", Some(root.as_str())))
        .summary
        .id;

    // Create: another context, an unknown task.
    let refused = |body: &Value| {
        let error = problem(&call(&socket, "POST", "/v1/tasks", Some(body)), 400);
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        error.message
    };
    let message = refused(&new("home", Some(root.as_str())));
    assert!(message.contains("it is in context work"), "{message}");
    let message = refused(&new("work", Some("zzzzzzzz")));
    assert!(message.contains("no task has this ID"), "{message}");

    // Edit: the task itself, its descendant.
    let edit = |id: &str, parent: &str| {
        call(
            &socket,
            "PATCH",
            &format!("/v1/tasks/{id}"),
            Some(&json!({ "relations": { "parent": parent } })),
        )
    };
    for parent in [root.as_str(), child.as_str()] {
        let error = problem(&edit(root.as_str(), parent), 400);
        assert!(
            error.message.contains("its own ancestor"),
            "{}",
            error.message
        );
    }
    // A valid move is accepted.
    let other = create(&socket, &new("work", None)).summary.id;
    let moved: TaskDetail = ok(&edit(child.as_str(), other.as_str()));
    assert_eq!(moved.summary.task.unwrap().relations.parent, Some(other));
}

#[test]
fn a_task_moves_to_another_context_and_keeps_its_id_and_history() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start();
    let socket = sandbox.socket();
    let new = |title: &str| json!({ "title": title, "kind": "chore", "context": "work", "status": "ready" });
    let task = create(&socket, &new("Move me")).summary.id;
    let move_to = |id: &str, context: &str| {
        call(
            &socket,
            "POST",
            &format!("/v1/tasks/{id}/move"),
            Some(&json!({ "context": context })),
        )
    };

    let moved: TaskDetail = ok(&move_to(task.as_str(), "home"));
    assert_eq!(moved.summary.id, task);
    assert_eq!(moved.summary.context.as_str(), "home");
    assert_eq!(moved.summary.task.unwrap().context.as_str(), "home");
    // Listed in the target context only; one task in all.
    let in_work: TaskList = ok(&call(&socket, "GET", "/v1/tasks?context=work", None));
    let in_home: TaskList = ok(&call(&socket, "GET", "/v1/tasks?context=home", None));
    assert_eq!(in_work.tasks.len(), 0);
    assert_eq!(in_home.tasks.len(), 1);
    // The files are in the target store only.
    let dir = |context: &str| {
        sandbox
            .loom_home()
            .join("contexts")
            .join(context)
            .join("tasks")
            .join(task.as_str())
    };
    assert!(!dir("work").exists());
    assert!(dir("home").join("task.md").is_file());
    // The history follows and ends with the move.
    let events: TaskEvents = ok(&call(
        &socket,
        "GET",
        &format!("/v1/tasks/{task}/events"),
        None,
    ));
    assert_eq!(event_types(&events), ["created", "edited"]);
    // The last change is the move: it is moved back, not undone.
    let refused = call(&socket, "POST", &format!("/v1/tasks/{task}/undo"), None);
    let error = problem(&refused, 409);
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(
        error
            .message
            .contains("move the task back to context `work`")
    );
    let back: TaskDetail = ok(&move_to(task.as_str(), "work"));
    assert_eq!(back.summary.context.as_str(), "work");
}

#[test]
fn a_move_that_cannot_happen_is_refused_and_changes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.write_task("work", "pppppppp", &task_md("pppppppp", "work", "ready"));
    sandbox.write_task(
        "work",
        "cccccccc",
        &task_md_with_parent("cccccccc", "work", "chore", "pppppppp"),
    );
    sandbox.write_task("work", "bbbbbbbb", "not a task file\n");
    let _daemon = sandbox.start();
    let socket = sandbox.socket();
    let move_to =
        |id: &str, body: &Value| call(&socket, "POST", &format!("/v1/tasks/{id}/move"), Some(body));
    let home = json!({ "context": "home" });

    // The same context, an unknown context, a parent, a child, a bad body.
    for (id, body, text) in [
        ("pppppppp", json!({ "context": "work" }), "already"),
        ("pppppppp", json!({ "context": "nowhere" }), "no task store"),
        ("pppppppp", home.clone(), "child task"),
        ("cccccccc", home.clone(), "has the parent"),
    ] {
        let error = problem(&move_to(id, &body), 400);
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert!(error.message.contains(text), "{}", error.message);
    }
    for body in [json!({}), json!({ "context": "home", "x": 1 }), json!([1])] {
        problem(&move_to("pppppppp", &body), 400);
    }
    // A task that does not exist, and one whose file is invalid.
    problem(&move_to("zzzzzzzz", &home), 404);
    let invalid = problem(&move_to("bbbbbbbb", &home), 409);
    assert_eq!(invalid.code, ErrorCode::ReadOnly);

    for id in ["pppppppp", "cccccccc", "bbbbbbbb"] {
        let dir = |context: &str| {
            sandbox
                .loom_home()
                .join("contexts")
                .join(context)
                .join("tasks")
                .join(id)
        };
        assert!(dir("work").is_dir(), "{id}");
        assert!(!dir("home").exists(), "{id}");
    }
}

/// A `task.md` of task `id` in `context` whose parent is `parent`, written by hand.
fn task_md_with_parent(id: &str, context: &str, kind: &str, parent: &str) -> String {
    format!(
        "+++\nid = \"{id}\"\ntitle = \"Task {id}\"\nkind = \"{kind}\"\ncontext = \"{context}\"\nstatus = \"ready\"\nimportance = \"p3\"\ncreated = 2026-10-12T09:14:00Z\n\n[relations]\nparent = \"{parent}\"\n+++\n\n## Notes\n"
    )
}

#[test]
fn only_a_new_parent_is_checked_and_an_invalid_file_is_a_known_task() {
    let sandbox = Sandbox::new();
    // `aaaa` points by hand to a task of another context; `bbbb` has an invalid file;
    // `cccc` has an unknown kind (read-only) and a parent in another context.
    sandbox.write_task("home", "hhhhhhhh", &task_md("hhhhhhhh", "home", "ready"));
    sandbox.write_task(
        "work",
        "aaaaaaaa",
        &task_md_with_parent("aaaaaaaa", "work", "chore", "hhhhhhhh"),
    );
    sandbox.write_task("work", "bbbbbbbb", "not a task file\n");
    sandbox.write_task(
        "work",
        "cccccccc",
        &task_md_with_parent("cccccccc", "work", "epic", "hhhhhhhh"),
    );
    let _daemon = sandbox.start();
    let socket = sandbox.socket();
    let patch =
        |id: &str, body: Value| call(&socket, "PATCH", &format!("/v1/tasks/{id}"), Some(&body));

    // The other relations change; the parent, already invalid, is resent as it is.
    let kept: TaskDetail = ok(&patch(
        "aaaaaaaa",
        json!({ "relations": { "parent": "hhhhhhhh", "blocks": ["bbbbbbbb"] } }),
    ));
    let relations = kept.summary.task.unwrap().relations;
    assert_eq!(relations.parent.unwrap().as_str(), "hhhhhhhh");
    assert_eq!(relations.blocks.len(), 1);

    // A task whose file is invalid is a known task of its context.
    let child = create(
        &socket,
        &json!({
            "title": "Child of an invalid file",
            "kind": "chore",
            "context": "work",
            "status": "inbox",
            "relations": { "parent": "bbbbbbbb" },
        }),
    );
    assert_eq!(
        child
            .summary
            .task
            .unwrap()
            .relations
            .parent
            .unwrap()
            .as_str(),
        "bbbbbbbb"
    );

    // A read-only task is refused as such, even with a new parent that is invalid.
    let error = problem(
        &patch("cccccccc", json!({ "relations": { "parent": "zzzzzzzz" } })),
        409,
    );
    assert_eq!(error.code, ErrorCode::ReadOnly);
}

#[test]
fn health_lists_an_invalid_file_until_it_is_fixed() {
    let sandbox = Sandbox::new();
    sandbox.write_task("work", "aaaaaaaa", &task_md("aaaaaaaa", "work", "ready"));
    sandbox.write_task("work", "bbbbbbbb", "not a task file\n");
    // An archived task that cannot be read is listed with its place in `archive/`.
    let archived = sandbox
        .loom_home()
        .join("contexts/work/archive/2025/cccccccc");
    fs::create_dir_all(&archived).unwrap();
    fs::write(archived.join("task.md"), "not a task file\n").unwrap();
    let _daemon = sandbox.start();
    let socket = sandbox.socket();

    let health: loom_api::TaskStoreHealth = ok(&call(&socket, "GET", "/v1/tasks/health", None));
    assert!(health.watching);
    assert_eq!(health.invalid.len(), 2, "{health:?}");
    assert_eq!(health.invalid[0].context.as_str(), "work");
    assert_eq!(health.invalid[0].id.as_str(), "bbbbbbbb");
    assert_eq!(
        health.invalid[0].path,
        sandbox.loom_home().join("contexts/work/tasks/bbbbbbbb")
    );
    assert_eq!(health.invalid[1].path, archived);
    assert!(health.unwatched.is_empty(), "{health:?}");
    assert!(
        health.invalid[0].error.starts_with("line 1"),
        "{}",
        health.invalid[0].error
    );
    assert!(health.failed.is_empty(), "{health:?}");

    // Fixed by hand: the watcher indexes it, and the problem is gone.
    sandbox.write_task("work", "bbbbbbbb", &task_md("bbbbbbbb", "work", "ready"));
    let mut remaining = health.invalid.len();
    for _ in 0..100 {
        let health: loom_api::TaskStoreHealth = ok(&call(&socket, "GET", "/v1/tasks/health", None));
        // Only the archived file is left.
        remaining = health.invalid.len() - 1;
        if remaining == 0 {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(remaining, 0, "the fixed file is still reported");
}

#[test]
fn the_list_keeps_the_children_of_several_parents_in_both_locations() {
    let sandbox = Sandbox::new();
    sandbox.write_task("work", "pppppppp", &task_md("pppppppp", "work", "ready"));
    sandbox.write_task("work", "qqqqqqqq", &task_md("qqqqqqqq", "work", "ready"));
    sandbox.write_task(
        "work",
        "cccccccc",
        &task_md_with_parent("cccccccc", "work", "chore", "pppppppp"),
    );
    sandbox.write_task(
        "work",
        "dddddddd",
        &task_md_with_parent("dddddddd", "work", "chore", "qqqqqqqq"),
    );
    let _daemon = sandbox.start();
    let socket = sandbox.socket();
    let ids = |path: &str| -> Vec<String> {
        let list: TaskList = ok(&call(&socket, "GET", path, None));
        list.tasks.iter().map(|t| t.id.to_string()).collect()
    };
    assert_eq!(ids("/v1/tasks?parent=pppppppp"), ["cccccccc"]);
    assert_eq!(
        ids("/v1/tasks?parent=pppppppp,qqqqqqqq&archived=any"),
        ["cccccccc", "dddddddd"]
    );
    assert_eq!(ids("/v1/tasks?parent=zzzzzzzz").len(), 0);
    let bad = call(&socket, "GET", "/v1/tasks?parent=not-an-id", None);
    assert_eq!(problem(&bad, 400).code, ErrorCode::InvalidRequest);
}
