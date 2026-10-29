use axum::http::StatusCode;
use loom_api::{
    Action, ErrorCode, IllegalTransition, Importance, Kind, NewTask, RankReason, Relations, Status,
    StatusName, Task, TaskActionName, TaskActionRequest, TaskEdit, TaskId, TaskReadOnly, Timestamp,
};
use loom_core::{Command, DomainError, ReadOnlyReason};
use loom_store::port::{ContentHash, TaskLocation};
use loom_store::tasks::{IndexedState, IndexedTask, TaskError};
use serde_json::json;

use super::*;

fn id(text: &str) -> TaskId {
    text.parse().unwrap()
}

fn date(text: &str) -> Date {
    text.parse().unwrap()
}

/// A valid ready p2 chore of `context`, created at `created`.
fn task(task_id: &str, context: &str, created: u64) -> Task {
    Task {
        id: id(task_id),
        title: format!("Task {task_id}"),
        kind: Kind::Chore,
        context: context.parse().unwrap(),
        status: Status::Ready,
        importance: Importance::P2,
        due: None,
        pinned: false,
        links: Vec::new(),
        relations: Relations::default(),
        created: Timestamp::from_unix_secs(created).unwrap(),
        updated: Timestamp::from_unix_secs(created).unwrap(),
    }
}

fn row(task: Task) -> IndexedTask {
    IndexedTask {
        id: task.id.clone(),
        context: task.context.clone(),
        hash: ContentHash::of(b""),
        location: TaskLocation::Active,
        state: IndexedState::Valid {
            task: Box::new(task),
            body: "## Notes\n".to_owned(),
        },
    }
}

fn invalid(task_id: &str, context: &str) -> IndexedTask {
    IndexedTask {
        id: id(task_id),
        context: context.parse().unwrap(),
        hash: ContentHash::of(b"x"),
        location: TaskLocation::Active,
        state: IndexedState::Invalid {
            error: "line 2: invalid TOML".to_owned(),
            text: "+++\nbroken\n+++\n".to_owned(),
        },
    }
}

fn filter(context: Option<&str>, status: Option<&str>, ranked: Option<bool>) -> TaskListQuery {
    TaskListQuery {
        context: context.map(str::to_owned),
        status: status.map(str::to_owned),
        ranked,
        archived: None,
        parent: None,
    }
}

#[test]
fn list_filters_are_checked_and_empty_values_ignored() {
    assert_eq!(
        task_filter(filter(Some(""), Some(""), None)).unwrap(),
        TaskFilter::default()
    );
    assert_eq!(
        task_filter(filter(Some("work"), Some("waiting"), Some(true))).unwrap(),
        TaskFilter {
            context: Some("work".parse().unwrap()),
            status: Some(StatusName::Waiting),
            ranked: true,
            archived: ArchivedFilter::Active,
            parent: Vec::new(),
        }
    );
    let archived = TaskListQuery {
        archived: Some(ArchivedFilter::Archived),
        ..filter(None, None, None)
    };
    assert_eq!(
        task_filter(archived).unwrap().archived,
        ArchivedFilter::Archived
    );
    for bad in [
        filter(Some("Bad/ctx"), None, None),
        filter(None, Some("blocked"), None),
    ] {
        assert_eq!(
            task_filter(bad).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }
    let parse = |value| serde_json::from_value::<TaskListQuery>(value);
    assert!(parse(json!({ "sort": "id" })).is_err());
}

#[test]
fn the_list_is_sorted_by_id_with_the_shortest_unique_prefixes() {
    let rows = vec![
        row(task("k3q7zm2a", "work", 100)),
        invalid("k3r00000", "home"),
        row(task("m0000000", "home", 100)),
    ];
    let list = task_list(&rows, &TaskFilter::default(), date("2026-10-14"));
    let summaries: Vec<(&str, &str)> = list
        .tasks
        .iter()
        .map(|summary| (summary.id.as_str(), summary.prefix.as_str()))
        .collect();
    assert_eq!(
        summaries,
        [("k3q7zm2a", "k3q"), ("k3r00000", "k3r"), ("m0000000", "m")]
    );
    let broken = &list.tasks[1];
    assert_eq!(broken.task, None);
    assert_eq!(
        broken.read_only,
        Some(TaskReadOnly::InvalidFile("line 2: invalid TOML".to_owned()))
    );
    assert!(list.tasks.iter().all(|summary| summary.rank.is_none()));
}

#[test]
fn the_list_filters_by_context_and_status_keeping_global_prefixes() {
    let mut done = task("k3r00000", "work", 100);
    done.status = Status::Done;
    let rows = vec![
        row(task("k3q7zm2a", "work", 100)),
        row(done),
        row(task("k3s00000", "home", 100)),
        invalid("k3t00000", "work"),
    ];
    let ids = |filter: &TaskFilter| -> Vec<String> {
        task_list(&rows, filter, date("2026-10-14"))
            .tasks
            .into_iter()
            .map(|summary| format!("{}:{}", summary.id, summary.prefix))
            .collect()
    };
    let work = TaskFilter {
        context: Some("work".parse().unwrap()),
        ..TaskFilter::default()
    };
    assert_eq!(ids(&work), ["k3q7zm2a:k3q", "k3r00000:k3r", "k3t00000:k3t"]);
    let done = TaskFilter {
        status: Some(StatusName::Done),
        ..work
    };
    assert_eq!(ids(&done), ["k3r00000:k3r"]);
}

#[test]
fn the_parent_filter_keeps_the_children_only() {
    let mut child = task("k3r00000", "work", 100);
    child.relations.parent = Some(id("k3q7zm2a"));
    let mut other = task("k3s00000", "work", 100);
    other.relations.parent = Some(id("m0000000"));
    let rows = vec![
        row(task("k3q7zm2a", "work", 100)),
        row(child),
        row(other),
        invalid("k3t00000", "work"),
    ];
    let query = TaskListQuery {
        parent: Some("k3q7zm2a".to_owned()),
        ..TaskListQuery::default()
    };
    let filter = task_filter(query).unwrap();
    assert_eq!(filter.parent, [id("k3q7zm2a")]);
    let ids: Vec<String> = task_list(&rows, &filter, date("2026-10-14"))
        .tasks
        .into_iter()
        .map(|summary| summary.id.to_string())
        .collect();
    assert_eq!(ids, ["k3r00000"]);
    // Several parents, separated by commas: the children of each. An unknown parent keeps nothing.
    let both = task_filter(TaskListQuery {
        parent: Some("k3q7zm2a,m0000000".to_owned()),
        ..TaskListQuery::default()
    })
    .unwrap();
    let ids: Vec<String> = task_list(&rows, &both, date("2026-10-14"))
        .tasks
        .into_iter()
        .map(|summary| summary.id.to_string())
        .collect();
    assert_eq!(ids, ["k3r00000", "k3s00000"]);
    let unknown = task_filter(TaskListQuery {
        parent: Some("zzzzzzzz".to_owned()),
        ..TaskListQuery::default()
    })
    .unwrap();
    assert_eq!(
        task_list(&rows, &unknown, date("2026-10-14")).tasks.len(),
        0
    );
    // An empty value is ignored; a value that is not a task ID is refused.
    let empty = TaskListQuery {
        parent: Some(String::new()),
        ..TaskListQuery::default()
    };
    assert_eq!(task_filter(empty).unwrap().parent, Vec::<TaskId>::new());
    let bad = TaskListQuery {
        parent: Some("not an id".to_owned()),
        ..TaskListQuery::default()
    };
    assert_eq!(
        task_filter(bad).unwrap_err().code,
        ErrorCode::InvalidRequest
    );
}

#[test]
fn archived_any_lists_both_locations() {
    let archived = IndexedTask {
        location: TaskLocation::Archived(2026),
        ..row(task("k3r00000", "work", 100))
    };
    let rows = vec![row(task("k3q7zm2a", "work", 100)), archived];
    let list = |archived| {
        let filter = TaskFilter {
            archived,
            ..TaskFilter::default()
        };
        task_list(&rows, &filter, date("2026-10-14")).tasks.len()
    };
    assert_eq!(list(ArchivedFilter::Active), 1);
    assert_eq!(list(ArchivedFilter::Archived), 1);
    assert_eq!(list(ArchivedFilter::Any), 2);
    let parse = |value: &str| {
        serde_json::from_value::<TaskListQuery>(json!({ "archived": value })).map(|q| q.archived)
    };
    assert_eq!(parse("any").unwrap(), Some(ArchivedFilter::Any));
    assert_eq!(parse("false").unwrap(), Some(ArchivedFilter::Active));
    assert!(parse("maybe").is_err());
}

#[test]
fn archived_tasks_are_listed_apart_and_never_ranked() {
    let mut closed = task("k3r00000", "work", 100);
    closed.status = Status::Done;
    // Reopened by hand in the archive: still archived, never in the priority list.
    let reopened = task("k3s00000", "work", 100);
    let archived = |task| IndexedTask {
        location: TaskLocation::Archived(2026),
        ..row(task)
    };
    let rows = vec![
        row(task("k3q7zm2a", "work", 100)),
        archived(closed),
        archived(reopened),
    ];
    let ids = |filter: &TaskFilter| -> Vec<String> {
        task_list(&rows, filter, date("2026-10-14"))
            .tasks
            .into_iter()
            .map(|summary| format!("{}:{}", summary.id, summary.prefix))
            .collect()
    };
    assert_eq!(ids(&TaskFilter::default()), ["k3q7zm2a:k3q"]);
    let ranked = TaskFilter {
        ranked: true,
        ..TaskFilter::default()
    };
    assert_eq!(ids(&ranked), ["k3q7zm2a:k3q"]);
    let only_archived = TaskFilter {
        archived: ArchivedFilter::Archived,
        ..TaskFilter::default()
    };
    // Prefixes stay unique across every task.
    assert_eq!(ids(&only_archived), ["k3r00000:k3r", "k3s00000:k3s"]);
    let ranked_archived = TaskFilter {
        ranked: true,
        ..only_archived.clone()
    };
    assert_eq!(ids(&ranked_archived), [] as [&str; 0]);
    let done = TaskFilter {
        status: Some(StatusName::Done),
        ..only_archived
    };
    assert_eq!(ids(&done), ["k3r00000:k3r"]);
}

#[test]
fn the_ranked_list_is_the_order_of_rank() {
    let today = date("2026-10-14");
    let mut pinned = task("p0000000", "work", 300);
    pinned.pinned = true;
    let mut urgent = task("v0000000", "work", 200);
    urgent.due = Some(today);
    let old = task("a0000000", "work", 100);
    let mut inbox = task("b0000000", "work", 50);
    inbox.status = Status::Inbox;
    let other = task("c0000000", "home", 10);
    let rows = vec![
        row(old.clone()),
        row(inbox),
        row(other.clone()),
        row(pinned.clone()),
        row(urgent.clone()),
        invalid("d0000000", "work"),
    ];
    let ranked = TaskFilter {
        ranked: true,
        ..TaskFilter::default()
    };
    let list = task_list(&rows, &ranked, today);
    let expected = loom_core::rank(&[old, other, pinned, urgent], today);
    let got: Vec<(TaskId, usize, RankReason)> = list
        .tasks
        .iter()
        .map(|summary| {
            let rank = summary.rank.unwrap();
            (summary.id.clone(), rank.position, rank.reason)
        })
        .collect();
    let want: Vec<(TaskId, usize, RankReason)> = expected
        .into_iter()
        .map(|ranked| (ranked.task_id, ranked.position, ranked.reason))
        .collect();
    assert_eq!(got, want);
    assert_eq!(got[0].0, id("p0000000"));

    let work = TaskFilter {
        context: Some("work".parse().unwrap()),
        ..ranked
    };
    let ids: Vec<String> = task_list(&rows, &work, today)
        .tasks
        .into_iter()
        .map(|summary| summary.id.to_string())
        .collect();
    assert_eq!(ids, ["p0000000", "v0000000", "a0000000"]);
}

#[test]
fn a_prefix_resolves_to_one_task_or_a_typed_problem() {
    let rows = vec![
        row(task("k3q7zm2a", "work", 100)),
        invalid("k3r00000", "home"),
    ];
    assert_eq!(resolve("K3Q", &rows).unwrap().id, id("k3q7zm2a"));
    assert_eq!(resolve("k3q7zm2a", &rows).unwrap().id, id("k3q7zm2a"));

    let (status, error) = resolve("k3", &rows).unwrap_err();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error.code, ErrorCode::AmbiguousPrefix);
    assert_eq!(
        error.details.unwrap(),
        json!({
            "prefix": "k3",
            "candidates": [
                { "id": "k3q7zm2a", "context": "work", "title": "Task k3q7zm2a" },
                { "id": "k3r00000", "context": "home", "title": "" },
            ],
        })
    );
    let (status, error) = resolve("zz", &rows).unwrap_err();
    assert_eq!(
        (status, error.code),
        (StatusCode::NOT_FOUND, ErrorCode::NotFound)
    );
    let (status, error) = resolve("k3q7zm2a0", &rows).unwrap_err();
    assert_eq!(
        (status, error.code),
        (StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest)
    );
}

#[test]
fn an_mr_key_resolves_to_its_review_task() {
    let review = |id: &str, context: &str, key: &str| {
        let mut task = task(id, context, 100);
        task.kind = Kind::Review;
        task.links = vec![loom_api::Link::Origin(loom_api::ExternalRef {
            provider: "github".to_owned(),
            key: key.to_owned(),
            url: Some(format!("https://github.com/{}", key.replace('#', "/pull/"))),
        })];
        row(task)
    };
    let rows = vec![
        row(task("k3q7zm2a", "work", 100)),
        review("r1000000", "work", "acme/shop#412"),
        review("r2000000", "work", "acme/shop#7"),
    ];
    // The project is matched without case, as the forges do.
    assert_eq!(resolve("Acme/Shop#412", &rows).unwrap().id, id("r1000000"));
    let (status, error) = resolve("acme/shop#9", &rows).unwrap_err();
    assert_eq!(
        (status, error.code),
        (StatusCode::NOT_FOUND, ErrorCode::NotFound)
    );
    // An archived task of the same MR: the active one is the alias.
    let mut archived = rows.clone();
    let mut old = review("r0000000", "work", "acme/shop#412");
    old.location = TaskLocation::Archived(2025);
    archived.push(old);
    assert_eq!(
        resolve("acme/shop#412", &archived).unwrap().id,
        id("r1000000")
    );
    let mut twice = rows;
    twice.push(review("r3000000", "home", "acme/shop#412"));
    let (status, error) = resolve("acme/shop#412", &twice).unwrap_err();
    assert_eq!(
        (status, error.code),
        (StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest)
    );
}

#[test]
fn a_detail_has_the_body_and_the_actions() {
    let rows = [
        row(task("k3q7zm2a", "work", 100)),
        invalid("k3r00000", "home"),
    ];
    let ids: Vec<TaskId> = rows.iter().map(|row| row.id.clone()).collect();
    let detail = task_detail(&rows[0], &ids);
    assert_eq!(detail.summary.prefix, "k3q");
    assert_eq!(detail.body, "## Notes\n");
    assert!(
        detail
            .actions
            .contains(&Action::SetStatus(StatusName::Done))
    );
    assert!(detail.actions.contains(&Action::Pin));
    assert_eq!(detail.summary.read_only, None);

    let broken = task_detail(&rows[1], &ids);
    assert_eq!(broken.body, "+++\nbroken\n+++\n");
    assert_eq!(broken.actions, []);

    let mut newer = task("k3q7zm2a", "work", 100);
    newer.status = Status::Unknown("blocked".to_owned());
    let detail = task_detail(&row(newer), &ids);
    assert_eq!(
        detail.summary.read_only,
        Some(TaskReadOnly::UnknownStatus("blocked".to_owned()))
    );
    assert_eq!(detail.actions, []);
}

#[test]
fn an_archived_task_is_read_only_with_no_action() {
    let mut done = task("k3q7zm2a", "work", 100);
    done.status = Status::Done;
    let archived = IndexedTask {
        location: TaskLocation::Archived(2026),
        ..row(done)
    };
    let ids = [archived.id.clone()];
    let detail = task_detail(&archived, &ids);
    assert_eq!(detail.summary.read_only, Some(TaskReadOnly::Archived(2026)));
    assert_eq!(detail.actions, []);
    assert_eq!(detail.body, "## Notes\n");
    // Archived first, whatever else makes it read-only.
    let broken = IndexedTask {
        location: TaskLocation::Archived(2025),
        ..invalid("k3r00000", "home")
    };
    assert_eq!(
        task_detail(&broken, &ids).summary.read_only,
        Some(TaskReadOnly::Archived(2025))
    );
}

#[test]
fn a_new_task_rejects_unknown_fields() {
    let body = json!({
        "title": "Write the talk",
        "kind": "content",
        "format": "talk",
        "context": "work",
        "status": "inbox",
        "due": "2026-10-20",
    });
    let new: NewTask = new_task(body.clone()).unwrap();
    assert_eq!(new.title, "Write the talk");
    assert_eq!(new.due, Some(date("2026-10-20")));
    let mut typo = body;
    typo["importanc"] = json!("p0");
    let error = new_task(typo).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert!(error.message.contains("importanc"), "{}", error.message);
    for bad in [json!([]), json!({ "title": "x" })] {
        assert_eq!(new_task(bad).unwrap_err().code, ErrorCode::InvalidRequest);
    }
}

#[test]
fn the_fields_of_a_new_task_are_those_of_its_schema() {
    let schema = serde_json::to_value(schemars::schema_for!(NewTask)).unwrap();
    let mut properties: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    properties.sort_unstable();
    let mut fields = NEW_TASK_FIELDS.to_vec();
    fields.sort_unstable();
    assert_eq!(fields, properties);

    let schema = serde_json::to_value(schemars::schema_for!(TaskEdit)).unwrap();
    let mut properties: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    properties.sort_unstable();
    let mut fields = TASK_EDIT_FIELDS.to_vec();
    fields.sort_unstable();
    assert_eq!(fields, properties);
}

#[test]
fn an_edit_rejects_unknown_fields_and_reads_null_as_removal() {
    let edit = task_edit(json!({ "title": "Renamed", "due": null })).unwrap();
    assert_eq!(
        edit,
        TaskEdit {
            title: Some("Renamed".to_owned()),
            due: Some(None),
            ..TaskEdit::default()
        }
    );
    for bad in [
        json!({ "status": "done" }),
        json!({ "pinned": true }),
        json!("x"),
    ] {
        assert_eq!(task_edit(bad).unwrap_err().code, ErrorCode::InvalidRequest);
    }
}

#[test]
fn actions_become_commands() {
    let empty = TaskActionRequest::default();
    assert_eq!(
        action_command(TaskActionName::Ready, empty.clone()).unwrap(),
        Command::SetStatus {
            status: Status::Ready,
        }
    );
    let accept_fields = TaskActionRequest {
        kind: Some(Kind::Story),
        importance: Some(Importance::P0),
        due: Some(Some(date("2026-10-25"))),
        ..TaskActionRequest::default()
    };
    assert_eq!(
        action_command(TaskActionName::Accept, empty.clone()).unwrap(),
        Command::Accept {
            kind: None,
            importance: None,
            due: None,
        }
    );
    assert_eq!(
        action_command(TaskActionName::Accept, accept_fields).unwrap(),
        Command::Accept {
            kind: Some(Kind::Story),
            importance: Some(Importance::P0),
            due: Some(Some(date("2026-10-25"))),
        }
    );
    assert_eq!(
        action_command(TaskActionName::Done, empty.clone()).unwrap(),
        Command::SetStatus {
            status: Status::Done
        }
    );
    assert_eq!(
        action_command(TaskActionName::Pin, empty.clone()).unwrap(),
        Command::Pin
    );
    assert_eq!(
        action_command(TaskActionName::Unpin, empty).unwrap(),
        Command::Unpin
    );
    let wait = TaskActionRequest {
        on: Some("Alice".to_owned()),
        until: Some(date("2026-10-20")),
        ..TaskActionRequest::default()
    };
    assert_eq!(
        action_command(TaskActionName::Waiting, wait).unwrap(),
        Command::SetStatus {
            status: Status::Waiting {
                on: Some("Alice".to_owned()),
                until: Some(date("2026-10-20")),
            }
        }
    );
    let note = TaskActionRequest {
        text: Some("Called back".to_owned()),
        ..TaskActionRequest::default()
    };
    assert_eq!(
        action_command(TaskActionName::LogNote, note).unwrap(),
        Command::LogNote {
            text: "Called back".to_owned()
        }
    );
}

#[test]
fn invalid_action_requests_are_rejected() {
    let empty = TaskActionRequest::default();
    let accept_fields = TaskActionRequest {
        kind: Some(Kind::Story),
        importance: Some(Importance::P0),
        due: Some(Some(date("2026-10-25"))),
        ..TaskActionRequest::default()
    };
    let wait = TaskActionRequest {
        on: Some("Alice".to_owned()),
        until: Some(date("2026-10-20")),
        ..TaskActionRequest::default()
    };
    let note = TaskActionRequest {
        text: Some("Called back".to_owned()),
        ..TaskActionRequest::default()
    };
    let wait_with_accept = TaskActionRequest {
        on: Some("Alice".to_owned()),
        ..accept_fields.clone()
    };
    assert_eq!(
        action_command(TaskActionName::Ready, accept_fields.clone())
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        action_command(TaskActionName::Accept, wait_with_accept)
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    for (action, request) in [
        (TaskActionName::LogNote, empty),
        (TaskActionName::Done, wait.clone()),
        (TaskActionName::Done, accept_fields.clone()),
        (TaskActionName::Pin, note.clone()),
        (TaskActionName::Waiting, note.clone()),
        (TaskActionName::Waiting, accept_fields.clone()),
        (TaskActionName::LogNote, wait.clone()),
        (TaskActionName::LogNote, accept_fields),
        (TaskActionName::Ready, wait),
        (TaskActionName::Ready, note),
    ] {
        assert_eq!(
            action_command(action, request).unwrap_err().code,
            ErrorCode::InvalidRequest,
            "{action:?}"
        );
    }
}

#[test]
fn task_errors_are_typed_problems() {
    let problem = |error: TaskError| {
        let (status, error) = task_problem(&error);
        (status, error.code, error.details)
    };
    assert_eq!(
        problem(TaskError::NotFound(id("k3q7zm2a"))),
        (StatusCode::NOT_FOUND, ErrorCode::NotFound, None)
    );
    assert_eq!(
        problem(TaskError::UnknownContext("client".parse().unwrap())),
        (StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest, None)
    );
    assert_eq!(
        problem(TaskError::Domain(DomainError::ReadOnly(
            ReadOnlyReason::UnknownKind("epic".to_owned())
        ))),
        (
            StatusCode::CONFLICT,
            ErrorCode::ReadOnly,
            Some(json!({ "reason": "unknown_kind", "value": "epic" }))
        )
    );
    assert_eq!(
        problem(TaskError::Archived {
            id: id("k3q7zm2a"),
            year: 2026,
        }),
        (
            StatusCode::CONFLICT,
            ErrorCode::ReadOnly,
            Some(json!({ "reason": "archived", "value": 2026 }))
        )
    );
    let (status, code, details) =
        problem(TaskError::Invalid(loom_store::port::InvalidTask::NotUtf8));
    assert_eq!((status, code), (StatusCode::CONFLICT, ErrorCode::ReadOnly));
    assert_eq!(details.unwrap()["reason"], json!("invalid_file"));
    assert_eq!(
        problem(TaskError::Domain(DomainError::IllegalTransition {
            from: StatusName::Done,
            to: StatusName::Active,
        })),
        (
            StatusCode::CONFLICT,
            ErrorCode::IllegalTransition,
            Some(
                serde_json::to_value(IllegalTransition {
                    from: StatusName::Done,
                    to: StatusName::Active,
                })
                .unwrap()
            )
        )
    );
    assert_eq!(
        problem(TaskError::Domain(DomainError::Conflict("status"))),
        (
            StatusCode::CONFLICT,
            ErrorCode::Conflict,
            Some(json!({ "field": "status" }))
        )
    );
    assert_eq!(
        problem(TaskError::Domain(DomainError::NothingToUndo)),
        (StatusCode::CONFLICT, ErrorCode::NothingToUndo, None)
    );
    for refused in [
        DomainError::EmptyTitle,
        DomainError::EmptyNote,
        DomainError::UnknownValue(ReadOnlyReason::UnknownStatus("x".to_owned())),
    ] {
        assert_eq!(
            problem(TaskError::Domain(refused)),
            (StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest, None)
        );
    }
    assert_eq!(
        problem(TaskError::Domain(DomainError::Id(
            loom_core::IdError::Exhausted
        )))
        .1,
        ErrorCode::Internal
    );
}

#[test]
fn today_is_the_local_date_unless_a_test_date_is_allowed() {
    let local = || date("2026-10-14");
    assert_eq!(today(None, true, local), date("2026-10-14"));
    assert_eq!(today(Some("2026-01-02"), true, local), date("2026-01-02"));
    assert_eq!(today(Some("2026-01-02"), false, local), date("2026-10-14"));
    assert_eq!(today(Some(""), true, local), date("2026-10-14"));
    assert_eq!(today(Some("tomorrow"), true, local), date("2026-10-14"));
}

#[test]
fn an_action_body_is_optional() {
    assert_eq!(action_request(b"").unwrap(), TaskActionRequest::default());
    assert_eq!(
        action_request(b" \n").unwrap(),
        TaskActionRequest::default()
    );
    assert_eq!(
        action_request(br#"{"text": "Called"}"#)
            .unwrap()
            .text
            .as_deref(),
        Some("Called")
    );
    for bad in [&br#"{"txt": "x"}"#[..], b"not json", b"[]"] {
        assert_eq!(
            action_request(bad).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }
}

#[test]
fn action_names_are_read_from_the_path() {
    assert_eq!(action_name_of("log_note"), Some(TaskActionName::LogNote));
    assert_eq!(action_name_of("done"), Some(TaskActionName::Done));
    assert_eq!(action_name_of("accept"), Some(TaskActionName::Accept));
    assert_eq!(action_name_of("archive"), None);
    assert_eq!(action_name_of("Done"), None);
}

#[test]
fn the_review_of_an_mr_is_the_active_task_then_the_last_archived_one() {
    use crate::review_sync::find_review;
    use loom_forge::model::MrKey;

    let review = |id: &str, updated: u64, location: TaskLocation| {
        let mut task = task(id, "work", 100);
        task.kind = Kind::Review;
        task.updated = Timestamp::from_unix_secs(updated).unwrap();
        task.links = vec![loom_api::Link::Origin(loom_api::ExternalRef {
            provider: "github".to_owned(),
            key: "acme/shop#412".to_owned(),
            url: Some("https://github.com/acme/shop/pull/412".to_owned()),
        })];
        let mut row = row(task);
        row.location = location;
        row
    };
    let key = MrKey::parse("acme/shop#412").unwrap();
    let context: loom_api::ContextName = "work".parse().unwrap();
    let found = |rows: &[IndexedTask]| {
        find_review(rows, &context, "github.com", &key).map(|(id, _, _)| id.to_string())
    };
    // Two archived tasks of one year: the last changed, whatever the order of their IDs.
    let mut rows = vec![
        review("r2000000", 100, TaskLocation::Archived(2026)),
        review("r1000000", 200, TaskLocation::Archived(2026)),
    ];
    assert_eq!(found(&rows).as_deref(), Some("r1000000"));
    rows.reverse();
    assert_eq!(found(&rows).as_deref(), Some("r1000000"));
    // The active task wins; another host is not this MR.
    rows.push(review("r0000000", 50, TaskLocation::Active));
    assert_eq!(found(&rows).as_deref(), Some("r0000000"));
    assert_eq!(
        find_review(&rows, &context, "github.example.com", &key),
        None
    );
}
