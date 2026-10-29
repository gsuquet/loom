use super::*;
use crate::context::ContextName;

const DESIGN_EXAMPLE: &str = include_str!("../../tests/fixtures/task-md/design-example.md");
const HAND_EDITED: &str = include_str!("../../tests/fixtures/task-md/hand-edited.md");
const MALFORMED: &str = include_str!("../../tests/fixtures/task-md/malformed.md");
const EMPTY_BODY: &str = include_str!("../../tests/fixtures/task-md/empty-body.md");
const FUTURE: &str = include_str!("../../tests/fixtures/task-md/future.md");

const FIXTURES: [(&str, &str); 4] = [
    ("design-example", DESIGN_EXAMPLE),
    ("hand-edited", HAND_EDITED),
    ("empty-body", EMPTY_BODY),
    ("future", FUTURE),
];

fn crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

fn ts(text: &str) -> Timestamp {
    serde_json::from_value(Json::String(text.to_owned())).unwrap()
}

fn id(text: &str) -> TaskId {
    text.parse().unwrap()
}

/// The task of the design example in `docs/design/storage.md`.
fn example_task() -> Task {
    Task {
        id: id("k3q7zm2a"),
        title: "Login timeout on mobile checkout".to_owned(),
        kind: Kind::Bug,
        context: "company-a".parse::<ContextName>().unwrap(),
        status: Status::Active,
        importance: Importance::P1,
        due: Some("2026-10-15".parse().unwrap()),
        pinned: false,
        links: vec![
            Link::Origin(ExternalRef {
                provider: "jira".to_owned(),
                key: "CORE-1042".to_owned(),
                url: Some("https://jira.example/browse/CORE-1042".to_owned()),
            }),
            Link::Repo {
                repo: "company-a/checkout".to_owned(),
            },
        ],
        relations: Relations {
            parent: Some(id("h8w2nc4d")),
            spawned_from: vec![],
            blocks: vec![],
        },
        created: ts("2026-10-12T09:14:00Z"),
        updated: ts("2026-10-12T09:14:00Z"),
    }
}

/// Lines of `after` that differ from `before`, which must have as many lines.
fn changed_lines<'a>(before: &str, after: &'a str) -> Vec<&'a str> {
    assert_eq!(before.lines().count(), after.lines().count(), "{after}");
    before
        .lines()
        .zip(after.lines())
        .filter(|(old, new)| old != new)
        .map(|(_, new)| new)
        .collect()
}

#[test]
fn the_design_example_reads_into_its_task() {
    let document = parse_task_md(DESIGN_EXAMPLE).unwrap();
    let updated = ts("2026-10-13T08:00:00Z");
    assert_eq!(
        document.task(updated),
        Task {
            updated,
            ..example_task()
        }
    );
    assert_eq!(document.body(), "Free notes, kind template sections.\n");
    assert_eq!(document.unknown_keys(), [] as [String; 0]);
}

#[test]
fn a_new_document_renders_the_design_example() {
    let mut document = TaskDocument::new(&example_task());
    assert_eq!(document.body(), body_template(&Kind::Bug));
    document.set_body("Free notes, kind template sections.\n");
    assert_eq!(render_task_md(&document), DESIGN_EXAMPLE);
}

#[test]
fn a_new_document_reads_back_into_the_same_task() {
    let mut task = example_task();
    task.kind = Kind::Content {
        format: Some(ContentFormat::Talk),
    };
    task.status = Status::Waiting {
        on: Some("review from Alice".to_owned()),
        until: Some("2026-10-20".parse().unwrap()),
    };
    task.pinned = true;
    task.links.push(Link::Related(ExternalRef {
        provider: "gitlab".to_owned(),
        key: "!88".to_owned(),
        url: None,
    }));
    task.links.push(Link::Delivery(ExternalRef {
        provider: "gitlab".to_owned(),
        key: "company-a/checkout!91".to_owned(),
        url: Some("https://gitlab.com/company-a/checkout/-/merge_requests/91".to_owned()),
    }));
    task.relations.blocks = vec![id("m4x9pq2r")];
    let text = render_task_md(&TaskDocument::new(&task));
    assert!(text.contains("role = \"delivery\"\n"), "{text}");
    let document = parse_task_md(&text).unwrap();
    assert!(document.unknown_keys().is_empty(), "{text}");
    assert_eq!(document.task(task.updated), task, "{text}");
    assert_eq!(document.body(), body_template(&task.kind));
}

#[test]
fn an_unchanged_file_renders_byte_for_byte() {
    for (name, text) in FIXTURES {
        for text in [text.to_owned(), crlf(text)] {
            let document = parse_task_md(&text).unwrap();
            assert_eq!(render_task_md(&document), text, "{name}");
        }
    }
}

#[test]
fn setting_the_same_task_changes_nothing() {
    let mut document = parse_task_md(HAND_EDITED).unwrap();
    let task = document.task(Timestamp::EPOCH);
    document.set_task(&task).unwrap();
    assert_eq!(render_task_md(&document), HAND_EDITED);
}

#[test]
fn changing_the_status_rewrites_only_that_key() {
    for text in [HAND_EDITED.to_owned(), crlf(HAND_EDITED)] {
        let mut document = parse_task_md(&text).unwrap();
        let mut task = document.task(Timestamp::EPOCH);
        task.status = Status::Done;
        document.set_task(&task).unwrap();
        let after = render_task_md(&document);
        assert_eq!(
            changed_lines(&text, &after),
            ["status = \"done\"   # since the standup"]
        );
        if text.contains('\r') {
            assert_eq!(after.matches('\n').count(), after.matches("\r\n").count());
        }
        assert_eq!(parse_task_md(&after).unwrap().task(Timestamp::EPOCH), task);
    }
}

#[test]
fn waiting_adds_its_details_and_leaving_it_removes_them() {
    let mut document = parse_task_md(DESIGN_EXAMPLE).unwrap();
    let mut task = document.task(Timestamp::EPOCH);
    task.status = Status::Waiting {
        on: Some("Alice".to_owned()),
        until: Some("2026-10-20".parse().unwrap()),
    };
    document.set_task(&task).unwrap();
    let waiting = render_task_md(&document);
    assert!(waiting.contains("status = \"waiting\"\n"), "{waiting}");
    assert!(waiting.contains("waiting_on = \"Alice\"\n"), "{waiting}");
    assert!(
        waiting.contains("waiting_until = 2026-10-20\n"),
        "{waiting}"
    );
    assert_eq!(
        parse_task_md(&waiting).unwrap().task(Timestamp::EPOCH),
        task
    );

    task.status = Status::Active;
    document.set_task(&task).unwrap();
    assert_eq!(render_task_md(&document), DESIGN_EXAMPLE);
}

#[test]
fn unknown_keys_are_listed_and_survive_a_rewrite() {
    let mut document = parse_task_md(HAND_EDITED).unwrap();
    assert_eq!(
        document.unknown_keys(),
        [
            "estimate",
            "agent",
            "links[0].branch",
            "links[1]",
            "relations.duplicates"
        ]
    );
    let mut task = document.task(Timestamp::EPOCH);
    task.title = "Checkout times out on mobile".to_owned();
    task.importance = Importance::P0;
    task.due = Some("2026-10-20".parse().unwrap());
    task.pinned = true;
    task.relations.blocks.clear();
    task.relations.parent = Some(id("h8w2nc4d"));
    document.set_task(&task).unwrap();
    let after = render_task_md(&document);
    for kept in [
        "# Edited by hand: comments, key order and unknown keys must survive.\n",
        "estimate = \"2h\"     # unknown to this version\n",
        "title   =   \"Checkout times out on mobile\"\n",
        "duplicates = [\"z9z9z9z9\"]\n",
        "branch = \"fix/login-timeout\"\n",
        "role = \"ticket\" # a link role written by a newer version\n",
        "[agent]\nmodel = \"opus\"\n",
        "+++ not a delimiter, part of the notes\n",
    ] {
        assert!(after.contains(kept), "{kept:?} lost:\n{after}");
    }
    let reread = parse_task_md(&after).unwrap();
    assert_eq!(reread.task(Timestamp::EPOCH), task);
    assert_eq!(reread.unknown_keys(), document.unknown_keys());
}

#[test]
fn changing_links_keeps_the_tables_of_unchanged_and_unknown_links() {
    let mut document = parse_task_md(HAND_EDITED).unwrap();
    let mut task = document.task(Timestamp::EPOCH);
    task.links.insert(
        0,
        Link::Origin(ExternalRef {
            provider: "jira".to_owned(),
            key: "CORE-1042".to_owned(),
            url: None,
        }),
    );
    document.set_task(&task).unwrap();
    let after = render_task_md(&document);
    let reread = parse_task_md(&after).unwrap();
    assert_eq!(reread.task(Timestamp::EPOCH), task, "{after}");
    assert!(
        after.contains("branch = \"fix/login-timeout\"\n"),
        "{after}"
    );
    assert!(after.contains("system = \"freshservice\"\n"), "{after}");
    assert!(
        after.find("[agent]") > after.rfind("[[links]]"),
        "tables keep their order:\n{after}"
    );

    task.links.clear();
    document.set_task(&task).unwrap();
    let after = render_task_md(&document);
    assert!(!after.contains("role = \"repo\""), "{after}");
    assert!(after.contains("role = \"ticket\""), "{after}");
}

#[test]
fn unknown_kinds_and_statuses_are_read_and_the_file_is_read_only() {
    let mut document = parse_task_md(FUTURE).unwrap();
    let task = document.task(Timestamp::EPOCH);
    assert_eq!(
        task.kind,
        Kind::Unknown {
            name: "meeting".to_owned(),
            format: None
        }
    );
    assert_eq!(task.status, Status::Unknown("snoozed".to_owned()));
    assert_eq!(document.unknown_keys(), ["snooze_until"]);
    let mut changed = task;
    changed.title = "Something else".to_owned();
    assert_eq!(
        document.set_task(&changed),
        Err(ReadOnlyReason::UnknownKind("meeting".to_owned()))
    );
    assert_eq!(render_task_md(&document), FUTURE);
}

#[test]
fn malformed_toml_reports_its_line_and_keeps_the_body() {
    for text in [MALFORMED.to_owned(), crlf(MALFORMED)] {
        let error = parse_task_md(&text).unwrap_err();
        assert!(
            matches!(error, FormatError::InvalidToml { .. }),
            "{error:?}"
        );
        assert_eq!(error.line(), 4, "{error}");
        assert!(error.to_string().starts_with("line 4: invalid TOML: "));
        assert_eq!(
            error.body().map(|body| body.replace('\r', "")).as_deref(),
            Some("Notes I must not lose.\n")
        );
    }
}

#[test]
fn an_invalid_value_reports_the_line_of_its_key() {
    let text = DESIGN_EXAMPLE.replace("importance = \"p1\"", "importance = \"p9\"");
    let error = parse_task_md(&text).unwrap_err();
    let FormatError::InvalidField { line, key, .. } = &error else {
        panic!("{error:?}");
    };
    assert_eq!((*line, key.as_str()), (7, "importance"));
    assert_eq!(error.body(), Some("Free notes, kind template sections.\n"));

    let text = DESIGN_EXAMPLE.replace("due = 2026-10-15", "due = 2026-10-15T08:00:00Z");
    assert_eq!(parse_task_md(&text).unwrap_err().line(), 8);

    let text = DESIGN_EXAMPLE.replace("role = \"repo\"\n", "");
    let error = parse_task_md(&text).unwrap_err();
    assert!(
        error.to_string().contains("`links[1]`: missing `role`"),
        "{error}"
    );
}

#[test]
fn a_missing_key_points_at_the_closing_delimiter() {
    let text = DESIGN_EXAMPLE.replace("title = \"Login timeout on mobile checkout\"\n", "");
    let error = parse_task_md(&text).unwrap_err();
    assert_eq!(error.to_string(), "line 24: `title`: missing");
}

#[test]
fn a_file_without_frontmatter_is_all_body() {
    let text = "Just notes\n";
    let error = parse_task_md(text).unwrap_err();
    assert_eq!(error.line(), 1);
    assert_eq!(error.body(), Some(text));
}

#[test]
fn an_unclosed_frontmatter_reports_the_last_line() {
    let error = parse_task_md("+++\nid = \"k3q7zm2a\"\ntitle = \"x\"\n").unwrap_err();
    assert_eq!(error, FormatError::UnclosedFrontmatter { line: 3 });
    assert_eq!(error.body(), None);
    assert_eq!(parse_task_md("+++\n").unwrap_err().line(), 1);
}

#[test]
fn an_empty_body_stays_empty() {
    for text in [EMPTY_BODY.to_owned(), crlf(EMPTY_BODY)] {
        let mut document = parse_task_md(&text).unwrap();
        assert_eq!(document.body(), "");
        let mut task = document.task(Timestamp::EPOCH);
        task.status = Status::Ready;
        document.set_task(&task).unwrap();
        assert!(render_task_md(&document).ends_with("+++\n") || text.contains('\r'));
    }
    let text = EMPTY_BODY.strip_suffix('\n').unwrap();
    let mut document = parse_task_md(text).unwrap();
    assert_eq!(render_task_md(&document), text);
    document.set_body("Notes\n");
    assert!(render_task_md(&document).ends_with("+++\n\nNotes\n"));
}

#[test]
fn every_known_kind_has_a_template_of_sections() {
    let kinds = [
        Kind::Story,
        Kind::Bug,
        Kind::Chore,
        Kind::Research,
        Kind::Investigation,
        Kind::Review,
        Kind::Support,
        Kind::Content { format: None },
        Kind::Admin,
        Kind::Interview,
        Kind::Learning,
    ];
    for kind in kinds {
        let template = body_template(&kind);
        assert!(template.starts_with("## "), "{kind:?}");
        assert!(
            template
                .lines()
                .all(|line| line.is_empty() || line.starts_with("## "))
        );
    }
    assert_eq!(
        body_template(&Kind::Unknown {
            name: "meeting".to_owned(),
            format: None
        }),
        ""
    );
}

#[test]
fn hand_written_alternatives_are_read() {
    let text = DESIGN_EXAMPLE
        .replace("due = 2026-10-15", "due = \"2026-10-15\"")
        .replace(
            "\n[relations]\nparent = \"h8w2nc4d\"\nspawned_from = []\nblocks = []\n",
            "",
        )
        .replace(
            "created = 2026-10-12T09:14:00Z\n",
            "created = 2026-10-12T09:14:00Z\nrelations = { parent = \"h8w2nc4d\" }\n",
        );
    let document = parse_task_md(&text).unwrap();
    assert_eq!(document.task(Timestamp::EPOCH).due, example_task().due);
    assert_eq!(
        document.task(Timestamp::EPOCH).relations,
        example_task().relations
    );
    assert_eq!(render_task_md(&document), text);
}
