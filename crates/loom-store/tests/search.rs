//! `Reader::search` over repositories and tasks together: one ranking, by status group first.
//! Everything under a temporary directory.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::path::PathBuf;

use loom_api::{ContextAssignment, RepoIdentity, SearchKind, SearchResult, TaskSearchField};
use loom_core::{
    Actor, ContextName, ExternalRef, Kind, Link, MachineId, NewTask, Origin, Relations, Status,
    Task, Timestamp,
};
use loom_store::files::FileStore;
use loom_store::tasks::{TaskIndex, Tasks};
use loom_store::{Reader, Store};
use tempfile::TempDir;

const fn at(secs: u64) -> Timestamp {
    Timestamp::from_unix_secs(secs).unwrap()
}

fn context(name: &str) -> ContextName {
    name.parse().unwrap()
}

fn origin() -> Origin {
    Origin {
        actor: Actor::User,
        machine_id: MachineId("laptop".to_owned()),
    }
}

fn random(seed: u64) -> impl FnMut() -> u64 {
    let mut bits = seed;
    move || {
        bits = bits
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        bits
    }
}

fn jira(key: &str) -> Link {
    Link::Origin(ExternalRef {
        provider: "jira".to_owned(),
        key: key.to_owned(),
        url: None,
    })
}

fn merge_request(key: &str) -> Link {
    Link::Related(ExternalRef {
        provider: "gitlab".to_owned(),
        key: key.to_owned(),
        url: None,
    })
}

struct Fixture {
    dir: TempDir,
    store: Store,
    tasks: Tasks<FileStore>,
    seed: u64,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("index.db");
        let store = Store::open(&db).unwrap();
        let stores = ["work", "home"]
            .into_iter()
            .map(|name| FileStore::new(context(name), dir.path().join("contexts").join(name)))
            .collect();
        let tasks = Tasks::new(stores, TaskIndex::open(&db).unwrap());
        Self {
            dir,
            store,
            tasks,
            seed: 0,
        }
    }

    fn reader(&self) -> Reader {
        Reader::open(&self.dir.path().join("index.db")).unwrap()
    }

    fn repo(&mut self, slug: &str) {
        let (context_name, name) = slug.split_once('/').unwrap();
        let repo = RepoIdentity {
            path: PathBuf::from(format!("/w/{name}")),
            name: name.to_owned(),
            slug: slug.to_owned(),
            context: ContextAssignment::Default {
                context: context(context_name),
            },
            remotes: Vec::new(),
            primary_remote: None,
            forge: None,
        };
        self.store.register(repo, at(50), false).unwrap();
    }

    /// A task of `context_name` created at `created` with `status` and `links`.
    fn task(
        &mut self,
        title: &str,
        context_name: &str,
        status: Status,
        links: Vec<Link>,
        created: u64,
    ) -> Task {
        self.seed += 1;
        self.tasks
            .create(
                NewTask {
                    title: title.to_owned(),
                    kind: Kind::Chore,
                    context: context(context_name),
                    status,
                    importance: None,
                    due: None,
                    pinned: false,
                    links,
                    relations: Relations::default(),
                },
                at(created),
                &origin(),
                &mut random(self.seed),
            )
            .unwrap()
    }
}

/// `repo:<slug>` or `task:<title>` of each result, in order.
fn labels(
    reader: &Reader,
    query: &str,
    context: Option<&str>,
    kind: Option<SearchKind>,
) -> Vec<String> {
    reader
        .search(query, context, kind, 100)
        .unwrap()
        .iter()
        .map(|result| match result {
            SearchResult::Repo(repo) => format!("repo:{}", repo.slug),
            SearchResult::Task(task) => format!("task:{}", task.title),
        })
        .collect()
}

#[test]
fn current_tasks_then_repositories_then_inbox_then_closed_tasks() {
    let mut fixture = Fixture::new();
    fixture.repo("work/checkout");
    fixture.repo("work/checkout-api");
    fixture.task("Checkout done", "work", Status::Done, Vec::new(), 100);
    fixture.task("Checkout to triage", "work", Status::Inbox, Vec::new(), 100);
    fixture.task(
        "Checkout dropped",
        "home",
        Status::Cancelled,
        Vec::new(),
        300,
    );
    fixture.task("Checkout next", "work", Status::Ready, Vec::new(), 100);
    fixture.task(
        "Checkout blocked",
        "work",
        Status::Waiting {
            on: Some("ops".to_owned()),
            until: None,
        },
        Vec::new(),
        200,
    );
    fixture.task("Checkout now", "work", Status::Active, Vec::new(), 300);

    let reader = fixture.reader();
    assert_eq!(
        labels(&reader, "checkout", None, None),
        [
            // Current tasks, most recently changed first.
            "task:Checkout now",
            "task:Checkout blocked",
            "task:Checkout next",
            "repo:work/checkout",
            "repo:work/checkout-api",
            "task:Checkout to triage",
            "task:Checkout dropped",
            "task:Checkout done",
        ]
    );
    assert_eq!(
        labels(&reader, "checkout", None, Some(SearchKind::Repo)),
        ["repo:work/checkout", "repo:work/checkout-api"]
    );
    assert_eq!(
        labels(&reader, "checkout", Some("home"), Some(SearchKind::Task)),
        ["task:Checkout dropped"]
    );
    // The limit cuts the merged list.
    let first = reader.search("checkout", None, None, 4).unwrap();
    assert_eq!(first.len(), 4);
    assert_eq!(first[3].kind(), SearchKind::Repo);
}

#[test]
fn an_external_key_finds_its_task_first() {
    let mut fixture = Fixture::new();
    fixture.repo("work/core");
    // A task that mentions the key without holding it ranks after the one that holds it.
    fixture.task(
        "Follow-up of CORE-1042",
        "work",
        Status::Active,
        Vec::new(),
        900,
    );
    let holder = fixture.task(
        "Login timeout on mobile checkout",
        "work",
        Status::Ready,
        vec![jira("CORE-1042"), merge_request("!88")],
        100,
    );
    fixture.task("Review 88 comments", "work", Status::Ready, Vec::new(), 100);

    let reader = fixture.reader();
    for query in ["CORE-1042", "core-1042", "!88"] {
        let results = reader.search(query, None, None, 100).unwrap();
        let SearchResult::Task(first) = &results[0] else {
            panic!("{query}: {results:?}");
        };
        assert_eq!(first.id, holder.id, "{query}");
        assert_eq!(first.link_keys, ["CORE-1042", "!88"]);
        assert_eq!(first.status.as_deref(), Some("ready"));
        assert!(first.matched_fields.contains(&TaskSearchField::LinkKeys));
    }
    assert_eq!(
        labels(&reader, "CORE-1042", None, None),
        [
            "task:Login timeout on mobile checkout",
            "task:Follow-up of CORE-1042",
        ]
    );
    // `core` alone is a prefix: the repository and both tasks.
    assert_eq!(
        labels(&reader, "core", None, None),
        [
            "task:Follow-up of CORE-1042",
            "task:Login timeout on mobile checkout",
            "repo:work/core",
        ]
    );
}

#[test]
fn task_matches_carry_their_fields_and_rank_title_matches_first() {
    let mut fixture = Fixture::new();
    // Each chore starts with its template sections: `notes` matches every task.
    let in_title = fixture.task("Rotate the tokens", "work", Status::Ready, Vec::new(), 100);
    let in_notes = fixture.task("Something else", "work", Status::Ready, Vec::new(), 200);
    let reader = fixture.reader();
    let results = reader.search("rotate", None, None, 100).unwrap();
    assert_eq!(results.len(), 1);
    let SearchResult::Task(task) = &results[0] else {
        panic!("{results:?}");
    };
    assert_eq!(task.id, in_title.id);
    assert_eq!(task.context, context("work"));
    assert_eq!(task.title, "Rotate the tokens");
    assert_eq!(task.matched_fields, [TaskSearchField::Title]);
    assert!(task.score.is_finite());

    // Every chore starts its notes with `## Notes`: a title match ranks first although it is
    // the oldest, then the most recently changed.
    let in_both = fixture.task("Notes to sort", "work", Status::Ready, Vec::new(), 50);
    let results = reader.search("notes", None, None, 100).unwrap();
    let found: Vec<_> = results
        .iter()
        .map(|result| match result {
            SearchResult::Task(task) => (task.id.clone(), task.matched_fields.clone()),
            SearchResult::Repo(repo) => panic!("unexpected repository {}", repo.slug),
        })
        .collect();
    assert_eq!(
        found,
        [
            (
                in_both.id,
                vec![TaskSearchField::Title, TaskSearchField::Notes]
            ),
            (in_notes.id, vec![TaskSearchField::Notes]),
            (in_title.id, vec![TaskSearchField::Notes]),
        ]
    );
}

#[test]
fn an_invalid_task_file_is_found_by_its_text_with_the_inbox() {
    let mut fixture = Fixture::new();
    fixture.repo("work/parser");
    fixture.task("Parser done", "work", Status::Done, Vec::new(), 100);
    let dir = fixture.dir.path().join("contexts/work/tasks/zzzzzzzz");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("task.md"), "+++\nbroken parser\n+++\n").unwrap();
    fixture.tasks.reindex().unwrap();

    let reader = fixture.reader();
    assert_eq!(
        labels(&reader, "parser", None, None),
        ["repo:work/parser", "task:", "task:Parser done"]
    );
    let results = reader
        .search("broken", None, Some(SearchKind::Task), 100)
        .unwrap();
    let SearchResult::Task(invalid) = &results[0] else {
        panic!("{results:?}");
    };
    assert_eq!(invalid.id.as_str(), "zzzzzzzz");
    assert_eq!(invalid.status, None);
    assert_eq!(invalid.title, "");
    assert_eq!(invalid.matched_fields, [TaskSearchField::Notes]);
}

#[test]
fn nothing_to_search_finds_nothing() {
    let mut fixture = Fixture::new();
    fixture.repo("work/x");
    fixture.task("x", "work", Status::Ready, Vec::new(), 100);
    let reader = fixture.reader();
    for query in ["", "  ", " - * \"", "zzz"] {
        assert_eq!(reader.search(query, None, None, 10).unwrap(), [], "{query}");
    }
}
