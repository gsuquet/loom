//! MR review sync end to end: a real `loomd` with two contexts and two forge hosts, polling a
//! local fake GitHub and a fake GitLab that serve `fixtures/forge/`.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::SystemTime;

use loom_api::{
    Actor, Event, QueueReason, ReviewQueue, ReviewStartResult, ReviewSync, ReviewSyncOutcome,
    ReviewSyncResult, Status, TaskDetail, TaskEvents, TaskList,
};
use serde_json::{Value, json};
use tempfile::TempDir;

mod common;

use common::{Daemon, ok, request};

const GITHUB_TOKEN: &str = "fake-github-token-0123";
const GITLAB_TOKEN: &str = "fake-gitlab-token-4567";
/// The GitHub token of another account.
const CLIENT_TOKEN: &str = "fake-client-token-89ab";
const V1: &str = "7ebf283bfee25ab48f95c0c55181a4ca66b4c990";
const V2: &str = "84280c47b6aa128c645b1fbea54aded8b32e8463";
const MIRROR: &str = "mrs/github-acme%2Fshop%23412";

fn fixture(path: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/forge")
        .join(path);
    fs::read_to_string(path).unwrap()
}

fn fixture_json(path: &str) -> Value {
    serde_json::from_str(&fixture(path)).unwrap()
}

/// One answer of the fake server.
#[derive(Clone)]
struct Route {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
    /// With an `If-None-Match` equal to it: 304.
    etag: Option<String>,
}

fn route(body: impl Into<String>) -> Route {
    Route {
        status: 200,
        headers: Vec::new(),
        body: body.into(),
        etag: None,
    }
}

fn with_etag(body: impl Into<String>, etag: &str) -> Route {
    Route {
        etag: Some(etag.to_owned()),
        ..route(body)
    }
}

type Routes = Arc<Mutex<HashMap<String, Route>>>;

/// A fake forge on `127.0.0.1`. A request is routed by its method and path; the query only
/// adds `+after` (an `updated_after` parameter) or `+page=<n>`. A route `<key>@<token>` answers
/// the requests with that token before `<key>`. Other requests get a 404.
/// `down` drops every connection without an answer.
struct Server {
    base: String,
    routes: Routes,
    down: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<String>>>,
}

fn route_key(method: &str, target: &str) -> String {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut key = format!("{method} {path}");
    for param in query.split('&') {
        if param.starts_with("updated_after=") {
            key.push_str("+after");
        }
        if let Some(page) = param.strip_prefix("page=") {
            let _ = write!(key, "+page={page}");
        }
    }
    key
}

impl Server {
    fn start(routes: HashMap<String, Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let routes: Routes = Arc::new(Mutex::new(routes));
        let down = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (served, is_down, recorded) =
            (Arc::clone(&routes), Arc::clone(&down), Arc::clone(&seen));
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                if is_down.load(Ordering::SeqCst) {
                    drop(stream);
                    continue;
                }
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut words = line.split_whitespace();
                let method = words.next().unwrap_or("").to_owned();
                let target = words.next().unwrap_or("").to_owned();
                let mut length = 0;
                let mut if_none_match = None;
                let mut token = String::new();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        let value = value.trim();
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.parse().unwrap();
                        } else if name.eq_ignore_ascii_case("if-none-match") {
                            if_none_match = Some(value.to_owned());
                        } else if name.eq_ignore_ascii_case("authorization") {
                            value
                                .rsplit(' ')
                                .next()
                                .unwrap_or("")
                                .clone_into(&mut token);
                        }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let key = route_key(&method, &target);
                recorded.lock().unwrap().push(key.clone());
                let found = {
                    let served = served.lock().unwrap();
                    served
                        .get(&format!("{key}@{token}"))
                        .or_else(|| served.get(&key))
                        .cloned()
                };
                let route = found.unwrap_or_else(|| Route {
                    status: 404,
                    ..route(r#"{"message":"Not Found"}"#)
                });
                let not_modified =
                    route.etag.is_some() && route.etag.as_deref() == if_none_match.as_deref();
                let (status, body) = if not_modified {
                    (304, String::new())
                } else {
                    (route.status, route.body.clone())
                };
                let mut answer = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                if let Some(etag) = &route.etag {
                    write!(answer, "ETag: {etag}\r\n").unwrap();
                }
                for (name, value) in &route.headers {
                    write!(answer, "{name}: {value}\r\n").unwrap();
                }
                answer.push_str("\r\n");
                answer.push_str(&body);
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        Self {
            base: format!("http://127.0.0.1:{port}"),
            routes,
            down,
            seen,
        }
    }

    fn set(&self, key: &str, route: Route) {
        self.routes.lock().unwrap().insert(key.to_owned(), route);
    }

    fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

fn github() -> Server {
    let pull = "/repos/acme/shop/pulls/412";
    Server::start(HashMap::from([
        ("GET /user".to_owned(), route(fixture("github/user.json"))),
        (
            "GET /search/issues".to_owned(),
            route(fixture("github/search-review-requested.json")),
        ),
        (
            format!("GET {pull}"),
            with_etag(fixture("github/pull-412.v1.json"), "\"v1\""),
        ),
        (
            format!("GET {pull}/reviews"),
            route(fixture("github/pull-412-reviews.json")),
        ),
        (
            "GET /repos/acme/shop/issues/412/comments".to_owned(),
            route(fixture("github/issue-412-comments.json")),
        ),
        (
            "POST /graphql".to_owned(),
            route(fixture("github/graphql-review-threads.json")),
        ),
    ]))
}

fn gitlab() -> Server {
    let mr = "/projects/77/merge_requests/88";
    let mut approvals = fixture_json("gitlab/approve.response.json");
    approvals["user_has_approved"] = json!(false);
    approvals["approved"] = json!(false);
    Server::start(HashMap::from([
        ("GET /user".to_owned(), route(fixture("gitlab/user.json"))),
        (
            "GET /merge_requests".to_owned(),
            route(fixture("gitlab/merge-requests-reviews-for-me.json")),
        ),
        (
            "GET /merge_requests+after".to_owned(),
            route(fixture("gitlab/merge-requests-reviews-for-me.empty.json")),
        ),
        (
            format!("GET {mr}"),
            with_etag(fixture("gitlab/merge-request-88.v2.json"), "W/\"88v2\""),
        ),
        (
            format!("GET {mr}/versions"),
            route(fixture("gitlab/merge-request-88-versions.json")),
        ),
        (format!("GET {mr}/approvals"), route(approvals.to_string())),
        (
            format!("GET {mr}/reviewers"),
            route(fixture("gitlab/merge-request-88-reviewers.json")),
        ),
        (
            format!("GET {mr}/discussions"),
            Route {
                headers: vec![("x-next-page", "2".to_owned())],
                ..route(fixture("gitlab/merge-request-88-discussions.page-1.json"))
            },
        ),
        (
            format!("GET {mr}/discussions+page=2"),
            route(fixture("gitlab/merge-request-88-discussions.page-2.json")),
        ),
    ]))
}

/// The GitHub MR belongs to `personal` (`github.com/acme`), the GitLab one to `company-a`.
/// Both contexts have a credential for both hosts.
const CONFIG: &str = r#"
[repos]
roots = []

[contexts.personal]
match.hosts = ["github.com/acme"]

[contexts.company-a]
match.hosts = ["gitlab.example.com/company-a"]

[forges."github.com"]
kind = "github"
token_env = "LOOM_TEST_GITHUB_TOKEN"

[forges."gitlab.example.com"]
kind = "gitlab"
token_env = "LOOM_TEST_GITLAB_TOKEN"
"#;

struct Sandbox {
    dir: TempDir,
    github: Server,
    gitlab: Server,
}

impl Sandbox {
    fn new() -> Self {
        // Under `/tmp`: the macOS default `$TMPDIR` is too long for a socket path.
        let dir = tempfile::Builder::new()
            .prefix("loomd-reviews-")
            .tempdir_in("/tmp")
            .unwrap();
        let sandbox = Self {
            dir,
            github: github(),
            gitlab: gitlab(),
        };
        fs::create_dir_all(sandbox.loom_home()).unwrap();
        fs::create_dir_all(sandbox.user_home()).unwrap();
        fs::write(sandbox.loom_home().join("config.toml"), CONFIG).unwrap();
        fs::write(
            sandbox.loom_home().join("machine.toml"),
            "id = \"laptop\"\n",
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

    fn socket(&self) -> PathBuf {
        self.loom_home().join("run/loomd.sock")
    }

    fn store(&self, context: &str) -> PathBuf {
        self.loom_home().join("contexts").join(context)
    }

    fn start(&self) -> Daemon {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
        command
            .env("LOOM_HOME", self.loom_home())
            .env("HOME", self.user_home())
            .env("XDG_CONFIG_HOME", self.user_home().join(".config"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("LOOM_TEST_GITHUB_TOKEN", GITHUB_TOKEN)
            .env("LOOM_TEST_GITLAB_TOKEN", GITLAB_TOKEN)
            .env("LOOM_TEST_CLIENT_TOKEN", CLIENT_TOKEN)
            .env(
                "LOOM_TEST_FORGE_API",
                format!(
                    "github.com={},gitlab.example.com={}",
                    self.github.base, self.gitlab.base
                ),
            )
            // Polls run on request only.
            .env("LOOM_TEST_REVIEW_START_DELAY", "1h")
            .arg("--color=never")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Daemon::start(&mut command, &self.socket())
    }

    fn sync(&self) -> ReviewSyncResult {
        ok(&request(
            &self.socket(),
            "POST",
            "/v1/reviews/sync",
            Some(&json!({})),
        ))
    }
}

fn poll<'a>(result: &'a ReviewSyncResult, context: &str, host: &str) -> &'a ReviewSync {
    result
        .forges
        .iter()
        .find(|forge| forge.context.as_str() == context && forge.host == host)
        .unwrap_or_else(|| panic!("no poll of {context} {host}: {result:?}"))
}

/// `(mrs, added, removed, changed, other_context, failed)` of a poll that succeeded.
fn counts(poll: &ReviewSync) -> (usize, usize, usize, usize, usize, usize) {
    match &poll.outcome {
        ReviewSyncOutcome::Synced {
            mrs,
            added,
            removed,
            changed,
            other_context,
            failed,
        } => (
            *mrs,
            added.len(),
            removed.len(),
            changed.len(),
            *other_context,
            failed.len(),
        ),
        outcome => panic!("not synced: {outcome:?}"),
    }
}

fn tasks(socket: &Path, context: &str) -> TaskList {
    ok(&request(
        socket,
        "GET",
        &format!("/v1/tasks?context={context}&status=active"),
        None,
    ))
}

fn task(socket: &Path, id: &str) -> TaskDetail {
    ok(&request(socket, "GET", &format!("/v1/tasks/{id}"), None))
}

/// Every file of `dir` with its modification time, recursively, `.git` left out.
fn files(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    let mut files = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                dirs.push(path);
            } else {
                files.push((path, entry.metadata().unwrap().modified().unwrap()));
            }
        }
    }
    files.sort();
    files
}

/// The commits of a store; 0 when it has none (or is no repository yet).
fn commits(store: &Path) -> usize {
    let output = common::git_run(
        &PathBuf::from("/tmp"),
        store,
        &["rev-list", "--count", "HEAD"],
    );
    if !output.status.success() {
        return 0;
    }
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
#[expect(clippy::too_many_lines, reason = "one scenario, step by step")]
fn mrs_to_review_are_entries_then_review_tasks_with_a_mirror() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();

    // 1. First poll: one entry per forge, each in the store of its MR's context. Then I start
    // on both.
    let result = sandbox.sync();
    assert_eq!(result.forges.len(), 4, "{result:?}");
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (1, 1, 0, 0, 0, 0)
    );
    assert_eq!(
        counts(poll(&result, "personal", "gitlab.example.com")),
        (0, 0, 0, 0, 1, 0)
    );
    assert_eq!(
        counts(poll(&result, "company-a", "github.com")),
        (0, 0, 0, 0, 1, 0)
    );
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (1, 1, 0, 0, 0, 0)
    );
    start(&socket, GITHUB_KEY);
    start(&socket, GITLAB_KEY);
    let personal = tasks(&socket, "personal").tasks;
    let company = tasks(&socket, "company-a").tasks;
    assert_eq!((personal.len(), company.len()), (1, 1));
    let review = personal[0].task.clone().unwrap();
    assert_eq!(review.kind, loom_api::Kind::Review);
    assert_eq!(review.status, Status::Active);
    assert_eq!(review.title, "Review acme/shop#412: Auth refactor");
    let id = review.id.to_string();
    let gitlab_id = company[0].id.to_string();
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(&id)
        .join(MIRROR);
    for name in [
        "mr.json",
        "description.md",
        "versions.json",
        "threads.json",
        "sync.json",
    ] {
        assert!(mirror.join(name).is_file(), "{name} missing");
    }
    assert_eq!(
        fs::read_to_string(mirror.join("description.md")).unwrap(),
        "Refactor the token checks.\n\nCloses #400."
    );
    let threads: Value =
        serde_json::from_str(&fs::read_to_string(mirror.join("threads.json")).unwrap()).unwrap();
    assert!(threads.as_array().unwrap().len() >= 3, "{threads}");
    let gitlab_mirror = sandbox
        .store("company-a")
        .join("tasks")
        .join(&gitlab_id)
        .join("mrs/gitlab-company-a%2Fcheckout%2188");
    let versions: Value =
        serde_json::from_str(&fs::read_to_string(gitlab_mirror.join("versions.json")).unwrap())
            .unwrap();
    assert_eq!(versions.as_array().unwrap().len(), 2);
    // The MR of another context is not in this store.
    assert!(!sandbox.store("company-a").join("tasks").join(&id).exists());
    assert!(
        !sandbox
            .store("personal")
            .join("tasks")
            .join(&gitlab_id)
            .exists()
    );
    // The MR key is an alias of the task.
    let alias = task(&socket, "acme%2Fshop%23412");
    assert_eq!(alias.summary.id.to_string(), id);

    // 2. Second poll: 304 and nothing new on GitLab, nothing written, nothing committed.
    let before = (
        files(&sandbox.store("personal")),
        files(&sandbox.store("company-a")),
    );
    let commits_before = (
        commits(&sandbox.store("personal")),
        commits(&sandbox.store("company-a")),
    );
    assert!(
        commits_before.0 >= 1 && commits_before.1 >= 1,
        "{commits_before:?}"
    );
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (1, 0, 0, 0, 0, 0)
    );
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (1, 0, 0, 0, 0, 0)
    );
    assert_eq!(
        (
            files(&sandbox.store("personal")),
            files(&sandbox.store("company-a"))
        ),
        before
    );
    assert_eq!(
        (
            commits(&sandbox.store("personal")),
            commits(&sandbox.store("company-a"))
        ),
        commits_before
    );
    // The second GitLab list asks for the changes only.
    assert!(
        sandbox
            .gitlab
            .requests()
            .contains(&"GET /merge_requests+after".to_owned())
    );

    // 3. Moving a Review task to another context is refused.
    let moved = request(
        &socket,
        "POST",
        &format!("/v1/tasks/{id}/move"),
        Some(&json!({"context": "company-a"})),
    );
    assert_eq!(moved.status, 400, "{}", moved.body);
    assert!(moved.body.contains("Review task"), "{}", moved.body);

    // 4. My review on V1: GitHub drops me from the requested reviewers and from the search.
    let mut reviewed = fixture_json("github/pull-412.v1.json");
    reviewed["requested_reviewers"] = json!([]);
    reviewed["updated_at"] = json!("2026-10-22T09:00:00Z");
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412",
        with_etag(reviewed.to_string(), "\"v1-reviewed\""),
    );
    let mut reviews = fixture_json("github/pull-412-reviews.json");
    let mut mine = reviews[0].clone();
    mine["node_id"] = json!("PRR_kwDOAAATic4AACarol");
    mine["user"]["login"] = json!("carol");
    mine["state"] = json!("APPROVED");
    mine["commit_id"] = json!(V1);
    mine["submitted_at"] = json!("2026-10-22T09:00:00Z");
    reviews.as_array_mut().unwrap().push(mine);
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412/reviews",
        route(reviews.to_string()),
    );
    sandbox.github.set(
        "GET /search/issues",
        route(fixture("github/search-review-requested.empty.json")),
    );
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (1, 0, 0, 1, 0, 0)
    );
    assert_eq!(
        task(&socket, &id).summary.task.unwrap().status,
        Status::Done
    );

    // 5. A new head (V2) after my verdict reopens the task, with an event, though I am no
    // longer a requested reviewer.
    let mut v2 = fixture_json("github/pull-412.v2.json");
    v2["requested_reviewers"] = json!([]);
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412",
        with_etag(v2.to_string(), "\"v2\""),
    );
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (1, 0, 0, 1, 0, 0)
    );
    assert_eq!(
        task(&socket, &id).summary.task.unwrap().status,
        Status::Ready
    );
    let log: TaskEvents = ok(&request(
        &socket,
        "GET",
        &format!("/v1/tasks/{id}/events"),
        None,
    ));
    let last = log.events.last().unwrap();
    assert_eq!(last.actor, Actor::Daemon);
    assert!(matches!(&last.event, Event::StatusChanged(change) if change.to == Status::Ready));
    let note = &log.events[log.events.len() - 2];
    assert!(
        matches!(&note.event, Event::NoteLogged { text } if text.contains("new head 84280c4")),
        "{note:?}"
    );
    let versions: Value =
        serde_json::from_str(&fs::read_to_string(mirror.join("versions.json")).unwrap()).unwrap();
    assert_eq!(versions.as_array().unwrap().len(), 2);

    // 6. GitHub unreachable: its polls fail and keep the mirror; GitLab goes on.
    let before = files(&sandbox.store("personal"));
    sandbox.github.down.store(true, Ordering::SeqCst);
    let result = sandbox.sync();
    let failed = poll(&result, "personal", "github.com");
    assert!(
        matches!(&failed.outcome, ReviewSyncOutcome::Failed { error } if !error.contains(GITHUB_TOKEN)),
        "{failed:?}"
    );
    assert!(failed.last_success.is_some());
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (1, 0, 0, 0, 0, 0)
    );
    assert_eq!(files(&sandbox.store("personal")), before);

    daemon.stop();
}

/// The GitLab MR of `gitlab()` on `head`, with `updated_at` and `state`, and its `ETag`.
fn gitlab_mr(head: &str, updated_at: &str, state: &str, etag: &str) -> Route {
    let mut mr = fixture_json("gitlab/merge-request-88.v2.json");
    mr["sha"] = json!(head);
    mr["diff_refs"]["head_sha"] = json!(head);
    mr["updated_at"] = json!(updated_at);
    mr["state"] = json!(state);
    with_etag(mr.to_string(), etag)
}

fn approvals(approved: bool) -> Route {
    let mut approvals = fixture_json("gitlab/approve.response.json");
    approvals["user_has_approved"] = json!(approved);
    approvals["approved"] = json!(approved);
    route(approvals.to_string())
}

fn status(socket: &Path, id: &str) -> Status {
    task(socket, id).summary.task.unwrap().status
}

#[test]
fn a_gitlab_approval_closes_once_a_new_head_reopens_and_a_merge_closes() {
    const MR: &str = "GET /projects/77/merge_requests/88";
    const V3: &str = "0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e";
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let id = start(&socket, GITLAB_KEY).task.summary.id.to_string();

    // My approval on the current head: done.
    sandbox
        .gitlab
        .set(&format!("{MR}/approvals"), approvals(true));
    sandbox.gitlab.set(
        MR,
        gitlab_mr(V2, "2026-10-24T08:00:00.000Z", "opened", "W/\"a\""),
    );
    sandbox.sync();
    assert_eq!(status(&socket, &id), Status::Done);

    // A push that keeps my approval (the project does not reset approvals): reopened.
    sandbox.gitlab.set(
        MR,
        gitlab_mr(V3, "2026-10-24T09:00:00.000Z", "opened", "W/\"b\""),
    );
    sandbox.sync();
    assert_eq!(status(&socket, &id), Status::Ready);

    // The MR changes again, my approval still shown: it is not a review of the push.
    sandbox.gitlab.set(
        MR,
        gitlab_mr(V3, "2026-10-24T10:00:00.000Z", "opened", "W/\"c\""),
    );
    sandbox.sync();
    assert_eq!(status(&socket, &id), Status::Ready);

    // After a restart, the first full read of an unchanged MR writes nothing.
    daemon.stop();
    let before = (
        files(&sandbox.store("company-a")),
        commits(&sandbox.store("company-a")),
    );
    let daemon = sandbox.start();
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (1, 0, 0, 0, 0, 0)
    );
    assert_eq!(
        (
            files(&sandbox.store("company-a")),
            commits(&sandbox.store("company-a"))
        ),
        before
    );
    assert_eq!(status(&socket, &id), Status::Ready);

    // Merged: done.
    sandbox.gitlab.set(
        MR,
        gitlab_mr(V3, "2026-10-24T11:00:00.000Z", "merged", "W/\"d\""),
    );
    sandbox.sync();
    assert_eq!(status(&socket, &id), Status::Done);
    // A closed task of a merged MR is no longer polled.
    let polled = sandbox.gitlab.requests().len();
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (0, 0, 0, 0, 0, 0)
    );
    assert!(
        !sandbox.gitlab.requests()[polled..]
            .iter()
            .any(|request| request.starts_with(MR)),
        "{:?}",
        &sandbox.gitlab.requests()[polled..]
    );

    daemon.stop();
}

#[test]
fn a_poll_that_stops_on_one_mr_applies_the_mrs_read_before_it() {
    const PULL_413: &str = "GET /repos/acme/shop/pulls/413";
    let sandbox = Sandbox::new();
    // Two MRs to review: 412 is read in full, then 413 stops the poll of the host.
    let mut search = fixture_json("github/search-review-requested.json");
    let mut second = search["items"][0].clone();
    second["number"] = json!(413);
    second["url"] = json!("https://api.github.com/repos/acme/shop/issues/413");
    second["html_url"] = json!("https://github.com/acme/shop/pull/413");
    second["pull_request"]["url"] = json!("https://api.github.com/repos/acme/shop/pulls/413");
    second["pull_request"]["html_url"] = json!("https://github.com/acme/shop/pull/413");
    search["items"].as_array_mut().unwrap().push(second);
    search["total_count"] = json!(2);
    sandbox
        .github
        .set("GET /search/issues", route(search.to_string()));
    sandbox.github.set(
        PULL_413,
        Route {
            status: 401,
            ..route(r#"{"message":"Bad credentials"}"#)
        },
    );
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    let result = sandbox.sync();
    let stopped = poll(&result, "personal", "github.com");
    assert!(
        matches!(stopped.outcome, ReviewSyncOutcome::Failed { .. }),
        "{stopped:?}"
    );
    // 412, read before 413 stopped the poll, has its entry.
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);

    // 413 is gone: only that MR fails; 412 is unchanged (its ETag matches), no second entry.
    sandbox.github.set(
        PULL_413,
        Route {
            status: 404,
            ..route(r#"{"message":"Not Found"}"#)
        },
    );
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (2, 0, 0, 0, 0, 1)
    );
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);

    daemon.stop();
}

#[test]
fn an_mr_whose_web_url_is_on_another_host_gets_no_entry() {
    let sandbox = Sandbox::new();
    let mut mr = fixture_json("gitlab/merge-request-88.v2.json");
    mr["web_url"] = json!("https://gitlab.public.example/company-a/checkout/-/merge_requests/88");
    sandbox.gitlab.set(
        "GET /projects/77/merge_requests/88",
        with_etag(mr.to_string(), "W/\"other\""),
    );
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    for _ in 0..2 {
        let result = sandbox.sync();
        assert_eq!(
            counts(poll(&result, "company-a", "gitlab.example.com")),
            (1, 0, 0, 0, 0, 1)
        );
    }
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    daemon.stop();
}

const GITHUB_KEY: &str = "acme%2Fshop%23412";
const GITLAB_KEY: &str = "company-a%2Fcheckout%2188";
const PULL: &str = "GET /repos/acme/shop/pulls/412";
const MR_88: &str = "GET /projects/77/merge_requests/88";
/// The two forges: `(context, percent-encoded key, key, entry file, mirror directory)`.
const FORGES: [(&str, &str, &str, &str, &str); 2] = [
    (
        "personal",
        GITHUB_KEY,
        "acme/shop#412",
        "github-acme%2Fshop%23412.json",
        MIRROR,
    ),
    (
        "company-a",
        GITLAB_KEY,
        "company-a/checkout!88",
        "gitlab-company-a%2Fcheckout%2188.json",
        "mrs/gitlab-company-a%2Fcheckout%2188",
    ),
];

/// The task directories of a store.
fn task_dirs(store: &Path) -> Vec<PathBuf> {
    fs::read_dir(store.join("tasks")).map_or_else(
        |_| Vec::new(),
        |entries| entries.map(|entry| entry.unwrap().path()).collect(),
    )
}

/// What a test compares to tell that nothing changed in a store.
fn snapshot(store: &Path) -> (Vec<(PathBuf, SystemTime)>, usize) {
    (files(store), commits(store))
}

/// The bytes of each entry file of a store.
fn entry_files(store: &Path) -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir(store.join("queue/mrs")) else {
        return Vec::new();
    };
    let mut found: Vec<(String, String)> = entries
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read_to_string(entry.path()).unwrap(),
            )
        })
        .collect();
    found.sort();
    found
}

/// The GitHub pull request with no reviewer, as a new version of it.
fn github_without_request(sandbox: &Sandbox) {
    let mut pull = fixture_json("github/pull-412.v1.json");
    pull["requested_reviewers"] = json!([]);
    sandbox
        .github
        .set(PULL, with_etag(pull.to_string(), "\"no-request\""));
    sandbox.github.set(
        "GET /search/issues",
        route(fixture("github/search-review-requested.empty.json")),
    );
}

/// The GitLab merge request with no reviewer, as a new version of it.
fn gitlab_without_request(sandbox: &Sandbox) {
    let mut mr = fixture_json("gitlab/merge-request-88.v2.json");
    mr["reviewers"] = json!([]);
    sandbox
        .gitlab
        .set(MR_88, with_etag(mr.to_string(), "W/\"no-request\""));
    sandbox.gitlab.set(
        "GET /merge_requests",
        route(fixture("gitlab/merge-requests-reviews-for-me.empty.json")),
    );
}

/// The GitHub pull request in `state`, as a new version of it.
fn github_in_state(sandbox: &Sandbox, state: &str, merged: bool) {
    let mut pull = fixture_json("github/pull-412.v1.json");
    pull["state"] = json!(state);
    pull["merged"] = json!(merged);
    if merged {
        pull["merged_at"] = json!("2026-10-23T09:00:00Z");
    }
    pull["closed_at"] = json!("2026-10-23T09:00:00Z");
    sandbox
        .github
        .set(PULL, with_etag(pull.to_string(), &format!("\"{state}\"")));
    sandbox.github.set(
        "GET /search/issues",
        route(fixture("github/search-review-requested.empty.json")),
    );
}

#[test]
fn a_poll_that_finds_a_requested_mr_writes_an_entry_no_task_and_no_mirror() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    let result = sandbox.sync();
    for (context, host) in [
        ("personal", "github.com"),
        ("company-a", "gitlab.example.com"),
    ] {
        let ReviewSyncOutcome::Synced {
            added,
            removed,
            changed,
            ..
        } = &poll(&result, context, host).outcome
        else {
            panic!("{result:?}");
        };
        assert_eq!(
            (added.len(), removed.len(), changed.len()),
            (1, 0, 0),
            "{result:?}"
        );
    }
    for (context, _, key, file, _) in FORGES {
        let store = sandbox.store(context);
        assert_eq!(
            task_dirs(&store),
            Vec::<PathBuf>::new(),
            "{context}: a task"
        );
        assert_eq!(tasks(&socket, context).tasks.len(), 0);
        // One file per MR, committed.
        let entries = entry_files(&store);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].0, file);
        assert!(commits(&store) >= 1, "{context}: no commit");
        let entry: Value = serde_json::from_str(&entries[0].1).unwrap();
        assert_eq!(entry["key"], key);
        assert_eq!(entry["reasons"], json!(["review_requested"]));
        for field in [
            "title",
            "author",
            "web_url",
            "head_sha",
            "updated_at",
            "forge_ids",
        ] {
            assert!(entry.get(field).is_some(), "{field} missing: {entry}");
        }
        // The API lists it, with its context.
        let listed = queue(&socket, Some(context));
        assert_eq!(listed.entries.len(), 1, "{listed:?}");
        assert_eq!(listed.entries[0].key, key);
        assert_eq!(listed.entries[0].context.as_str(), context);
        assert_eq!(
            listed.entries[0].reasons,
            vec![QueueReason::ReviewRequested]
        );
    }
    assert_eq!(queue(&socket, None).entries.len(), 2);
    // An entry has no mirror: nothing reads the threads or versions of the MR.
    assert!(
        !sandbox
            .github
            .requests()
            .iter()
            .any(|line| line.contains("graphql") || line.contains("comments")),
        "{:?}",
        sandbox.github.requests()
    );
    assert!(
        !sandbox
            .gitlab
            .requests()
            .iter()
            .any(|line| line.contains("discussions") || line.contains("versions")),
        "{:?}",
        sandbox.gitlab.requests()
    );
    daemon.stop();
}

#[test]
fn a_start_creates_the_active_task_with_its_mirror_and_removes_the_entry() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    for (context, encoded, key, _, mirror) in FORGES {
        let store = sandbox.store(context);
        let commits_before = commits(&store);
        let started = start(&socket, encoded);
        assert!(started.created, "{key}");
        let task = started.task.summary.task.clone().unwrap();
        assert_eq!(task.status, Status::Active, "{key}");
        assert_eq!(task.kind, loom_api::Kind::Review);
        assert_eq!(task.context.as_str(), context);
        let id = started.task.summary.id.to_string();
        // Without waiting for a poll.
        let dir = store.join("tasks").join(&id).join(mirror);
        for name in [
            "mr.json",
            "description.md",
            "versions.json",
            "threads.json",
            "sync.json",
        ] {
            assert!(dir.join(name).is_file(), "{key}: {name} missing");
        }
        assert_eq!(entry_files(&store), Vec::<(String, String)>::new(), "{key}");
        assert_eq!(queue(&socket, Some(context)).entries.len(), 0);
        // The task and the removal of the entry are committed: the tree is clean.
        assert!(commits(&store) > commits_before, "{key}");
        let status = common::git(&PathBuf::from("/tmp"), &store, &["status", "--porcelain"]);
        assert_eq!(status.trim(), "", "{key}");
        // The start is my act: the creation is not of the daemon.
        let log: TaskEvents = ok(&request(
            &socket,
            "GET",
            &format!("/v1/tasks/{id}/events"),
            None,
        ));
        assert_ne!(log.events[0].actor, Actor::Daemon, "{key}");
        // The key is an alias of the task.
        assert_eq!(task_summary_id(&socket, encoded), id);

        // A second start: the same task, nothing written.
        let before = snapshot(&store);
        let again = start(&socket, encoded);
        assert!(!again.created, "{key}");
        assert_eq!(again.task.summary.id.to_string(), id);
        assert_eq!(snapshot(&store), before, "{key}");
        assert_eq!(task_dirs(&store).len(), 1, "{key}");
    }
    // The next poll has nothing to change, and gives no entry to a task.
    let before = (
        snapshot(&sandbox.store("personal")),
        snapshot(&sandbox.store("company-a")),
    );
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (1, 0, 0, 0, 0, 0)
    );
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (1, 0, 0, 0, 0, 0)
    );
    assert_eq!(
        (
            snapshot(&sandbox.store("personal")),
            snapshot(&sandbox.store("company-a"))
        ),
        before
    );
    daemon.stop();
}

#[test]
fn starts_racing_a_poll_give_one_task() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    for (context, encoded, key, _, _) in FORGES {
        // Two starts and a poll at once, released together.
        let gate = Arc::new(Barrier::new(3));
        #[expect(
            clippy::needless_collect,
            reason = "both threads must run before the first join"
        )]
        let starters: Vec<_> = (0..2)
            .map(|_| {
                let (gate, socket) = (Arc::clone(&gate), socket.clone());
                thread::spawn(move || {
                    gate.wait();
                    start(&socket, encoded)
                })
            })
            .collect();
        let poller = {
            let (gate, socket) = (Arc::clone(&gate), socket.clone());
            thread::spawn(move || {
                gate.wait();
                let _: ReviewSyncResult = ok(&request(
                    &socket,
                    "POST",
                    "/v1/reviews/sync",
                    Some(&json!({})),
                ));
            })
        };
        let started: Vec<_> = starters
            .into_iter()
            .map(|one| one.join().unwrap())
            .collect();
        poller.join().unwrap();
        assert_eq!(
            started[0].task.summary.id, started[1].task.summary.id,
            "{key}"
        );
        assert_eq!(
            started.iter().filter(|one| one.created).count(),
            1,
            "{key}: {started:?}"
        );
        // One more poll sees the task, and adds no entry.
        sandbox.sync();
        let store = sandbox.store(context);
        assert_eq!(task_dirs(&store).len(), 1, "{key}");
        assert_eq!(entry_files(&store), Vec::<(String, String)>::new(), "{key}");
    }
    daemon.stop();
}

#[test]
fn a_start_with_the_forge_down_changes_nothing_and_can_be_done_again() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    for (context, encoded, key, _, _) in FORGES {
        let server = if context == "personal" {
            &sandbox.github
        } else {
            &sandbox.gitlab
        };
        let store = sandbox.store(context);
        let before = snapshot(&store);
        server.down.store(true, Ordering::SeqCst);
        let refused = start_response(&socket, encoded);
        assert_eq!(refused.status, 502, "{key}: {}", refused.body);
        assert!(refused.body.contains("\"forge\""), "{}", refused.body);
        for token in [GITHUB_TOKEN, GITLAB_TOKEN] {
            assert!(!refused.body.contains(token), "{}", refused.body);
        }
        assert_eq!(snapshot(&store), before, "{key}");
        server.down.store(false, Ordering::SeqCst);
        let started = start(&socket, encoded);
        assert!(started.created, "{key}");
        assert_eq!(task_dirs(&store).len(), 1);
    }
    daemon.stop();
}

#[test]
fn a_start_of_an_mr_merged_since_the_last_poll_is_refused_and_removes_the_entry() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    github_in_state(&sandbox, "closed", true);
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-24T11:00:00.000Z", "closed", "W/\"closed\""),
    );
    for (context, encoded, key, _, _) in FORGES {
        let refused = start_response(&socket, encoded);
        assert_eq!(refused.status, 409, "{key}: {}", refused.body);
        assert!(refused.body.contains("\"conflict\""), "{}", refused.body);
        let store = sandbox.store(context);
        assert_eq!(task_dirs(&store), Vec::<PathBuf>::new(), "{key}");
        assert_eq!(entry_files(&store), Vec::<(String, String)>::new(), "{key}");
    }
    daemon.stop();
}

#[test]
fn a_start_is_refused_without_an_entry_with_a_bad_key_and_on_a_machine_that_does_not_poll() {
    let sandbox = Sandbox::new();
    // `personal` is held here, but polled by the first machine of its list.
    let config = CONFIG.replace(
        "[contexts.personal]\n",
        "[contexts.personal]\nmachines = [\"work\", \"laptop\"]\n",
    );
    fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
    let daemon = sandbox.start();
    let socket = sandbox.socket();

    // Nothing polled yet: no entry, no task. The forge is not called.
    let before = sandbox.gitlab.requests().len();
    let missing = start_response(&socket, GITLAB_KEY);
    assert_eq!(missing.status, 404, "{}", missing.body);
    assert_eq!(sandbox.gitlab.requests().len(), before);
    assert_eq!(start_response(&socket, "not-a-key").status, 400);

    // An entry that came with a store sync from the machine that polls.
    let dir = sandbox.store("personal").join("queue/mrs");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(FORGES[0].3),
        json!({
            "v": 1,
            "key": "acme/shop#412",
            "forge_ids": {"forge": "github", "node_id": "PR_x", "id": 1},
            "title": "Auth refactor",
            "author": "alice",
            "web_url": "https://github.com/acme/shop/pull/412",
            "head_sha": V1,
            "updated_at": "2026-10-22T09:00:00Z",
            "reasons": ["review_requested"]
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);
    let refused = start_response(&socket, GITHUB_KEY);
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert!(refused.body.contains("first machine"), "{}", refused.body);
    assert_eq!(task_dirs(&sandbox.store("personal")), Vec::<PathBuf>::new());
    daemon.stop();
}

#[test]
fn an_mr_with_my_verdict_on_the_current_head_has_no_entry() {
    let sandbox = Sandbox::new();
    // GitHub: my review on the head, and a request through a team: the search lists me, and I
    // am not in the reviewers, since GitHub removes me when I submit. A known limit.
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412/reviews",
        reviews_with_mine_on(V1),
    );
    let mut pull = fixture_json("github/pull-412.v1.json");
    pull["requested_reviewers"] = json!([]);
    sandbox
        .github
        .set(PULL, with_etag(pull.to_string(), "\"team\""));
    // GitLab lists the MRs that I approved.
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    assert_eq!(queue(&socket, None).entries.len(), 0);
    for (context, _, _, _, _) in FORGES {
        assert_eq!(task_dirs(&sandbox.store(context)), Vec::<PathBuf>::new());
    }
    // A push without my verdict on it.
    let mut v2 = fixture_json("github/pull-412.v2.json");
    v2["requested_reviewers"] = json!([]);
    v2["updated_at"] = json!("2026-10-23T09:00:00Z");
    sandbox
        .github
        .set(PULL, with_etag(v2.to_string(), "\"v2\""));
    sandbox.sync();
    let listed = queue(&socket, None);
    assert_eq!(listed.entries.len(), 1, "{listed:?}");
    assert_eq!(listed.entries[0].key, "acme/shop#412");
    assert_eq!(listed.entries[0].head_sha, V2);
    daemon.stop();
}

#[test]
fn my_verdict_after_the_entry_removes_it() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    assert_eq!(queue(&socket, None).entries.len(), 2);
    let mut reviews = fixture_json("github/pull-412-reviews.json");
    let mut mine = reviews[0].clone();
    mine["node_id"] = json!("PRR_mine");
    mine["user"]["login"] = json!("carol");
    mine["state"] = json!("COMMENTED");
    mine["commit_id"] = json!(V1);
    mine["submitted_at"] = json!("2026-10-22T09:00:00Z");
    reviews.as_array_mut().unwrap().push(mine);
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412/reviews",
        route(reviews.to_string()),
    );
    // GitHub removes me from the reviewers; the search still lists me through a team.
    let mut pull = fixture_json("github/pull-412.v1.json");
    pull["requested_reviewers"] = json!([]);
    pull["updated_at"] = json!("2026-10-22T09:00:00Z");
    sandbox
        .github
        .set(PULL, with_etag(pull.to_string(), "\"reviewed\""));
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-24T08:00:00.000Z", "opened", "W/\"a\""),
    );
    let result = sandbox.sync();
    assert_eq!(counts(poll(&result, "personal", "github.com")).2, 1);
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")).2,
        1
    );
    assert_eq!(queue(&socket, None).entries.len(), 0);
    // It stays gone at the next polls, though GitLab still lists the MR.
    sandbox.gitlab.set(
        "GET /merge_requests+after",
        route(fixture("gitlab/merge-requests-reviews-for-me.json")),
    );
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")).1,
        0,
        "{result:?}"
    );
    assert_eq!(queue(&socket, None).entries.len(), 0);
    daemon.stop();
}

/// The entry is gone, no task and no event came of it, and its removal is committed.
fn assert_left_the_queue(sandbox: &Sandbox, context: &str, host: &str, result: &ReviewSyncResult) {
    let ReviewSyncOutcome::Synced { removed, added, .. } = &poll(result, context, host).outcome
    else {
        panic!("{result:?}");
    };
    assert_eq!((added.len(), removed.len()), (0, 1), "{result:?}");
    let store = sandbox.store(context);
    assert_eq!(entry_files(&store), Vec::<(String, String)>::new());
    assert_eq!(
        task_dirs(&store),
        Vec::<PathBuf>::new(),
        "no task, so no event"
    );
    let status = common::git(&PathBuf::from("/tmp"), &store, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "");
    let log = common::git(
        &PathBuf::from("/tmp"),
        &store,
        &["log", "-1", "--format=%s"],
    );
    assert_eq!(log.trim(), "queue: 0 added, 0 updated, 1 removed");
}

#[test]
fn the_entry_goes_when_the_request_is_removed() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    github_without_request(&sandbox);
    gitlab_without_request(&sandbox);
    let result = sandbox.sync();
    assert_left_the_queue(&sandbox, "personal", "github.com", &result);
    assert_left_the_queue(&sandbox, "company-a", "gitlab.example.com", &result);
    daemon.stop();
}

#[test]
fn the_entry_goes_when_the_mr_is_merged() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    github_in_state(&sandbox, "closed", true);
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-24T11:00:00.000Z", "merged", "W/\"m\""),
    );
    let result = sandbox.sync();
    assert_left_the_queue(&sandbox, "personal", "github.com", &result);
    assert_left_the_queue(&sandbox, "company-a", "gitlab.example.com", &result);
    daemon.stop();
}

#[test]
fn the_entry_goes_when_the_mr_is_closed() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    github_in_state(&sandbox, "closed", false);
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-24T11:00:00.000Z", "closed", "W/\"c\""),
    );
    let result = sandbox.sync();
    assert_left_the_queue(&sandbox, "personal", "github.com", &result);
    assert_left_the_queue(&sandbox, "company-a", "gitlab.example.com", &result);
    daemon.stop();
}

#[test]
fn a_request_that_comes_back_after_the_entry_left_gives_a_new_entry() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    github_without_request(&sandbox);
    sandbox.sync();
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 0);
    // Same pull request answer (a team request does not change it): no 304 hides the request.
    sandbox.github.set(
        "GET /search/issues",
        route(fixture("github/search-review-requested.json")),
    );
    sandbox.sync();
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);
    daemon.stop();
}

#[test]
fn an_entry_follows_the_mr_and_keeps_its_file_when_nothing_changes() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let mut v2 = fixture_json("github/pull-412.v2.json");
    v2["title"] = json!("Auth refactor, part 2");
    sandbox
        .github
        .set(PULL, with_etag(v2.to_string(), "\"v2\""));
    let result = sandbox.sync();
    // Not added, not removed: its fields follow.
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (1, 0, 0, 0, 0, 0)
    );
    let listed = queue(&socket, Some("personal"));
    assert_eq!(listed.entries[0].title, "Auth refactor, part 2");
    assert_eq!(listed.entries[0].head_sha, V2);
    let log = common::git(
        &PathBuf::from("/tmp"),
        &sandbox.store("personal"),
        &["log", "-1", "--format=%s"],
    );
    assert_eq!(log.trim(), "queue: 0 added, 1 updated, 0 removed");
    daemon.stop();
}

#[test]
fn an_incomplete_search_removes_no_entry() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let mut search = fixture_json("github/search-review-requested.empty.json");
    search["incomplete_results"] = json!(true);
    sandbox
        .github
        .set("GET /search/issues", route(search.to_string()));
    let mut pull = fixture_json("github/pull-412.v1.json");
    pull["requested_reviewers"] = json!([]);
    sandbox
        .github
        .set(PULL, with_etag(pull.to_string(), "\"team\""));
    sandbox.sync();
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);
    daemon.stop();
}

#[test]
fn a_gitlab_entry_is_read_by_its_project_id_after_a_restart() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    daemon.stop();
    // The list no longer shows it, and loomd has no memory of the project.
    sandbox.gitlab.set(
        "GET /merge_requests",
        route(fixture("gitlab/merge-requests-reviews-for-me.empty.json")),
    );
    let daemon = sandbox.start();
    let before = snapshot(&sandbox.store("company-a"));
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (1, 0, 0, 0, 0, 0)
    );
    assert_eq!(snapshot(&sandbox.store("company-a")), before);
    // The start too reads it by that ID.
    assert!(start(&sandbox.socket(), GITLAB_KEY).created);
    daemon.stop();
}

#[test]
fn a_failed_list_or_read_changes_no_entry() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    let stores = || {
        (
            snapshot(&sandbox.store("personal")),
            snapshot(&sandbox.store("company-a")),
        )
    };
    let before = stores();
    let broken = || Route {
        status: 500,
        ..route(r#"{"message":"boom"}"#)
    };

    // The lists fail.
    sandbox.github.set("GET /search/issues", broken());
    sandbox.gitlab.set("GET /merge_requests+after", broken());
    let result = sandbox.sync();
    for (context, host) in [
        ("personal", "github.com"),
        ("company-a", "gitlab.example.com"),
    ] {
        assert!(
            matches!(
                poll(&result, context, host).outcome,
                ReviewSyncOutcome::Failed { .. }
            ),
            "{result:?}"
        );
    }
    assert_eq!(stores(), before);

    // The lists answer, with the MRs gone, and the reads fail.
    sandbox.github.set(
        "GET /search/issues",
        route(fixture("github/search-review-requested.empty.json")),
    );
    sandbox.gitlab.set(
        "GET /merge_requests+after",
        route(fixture("gitlab/merge-requests-reviews-for-me.empty.json")),
    );
    sandbox.github.set(PULL, broken());
    sandbox.gitlab.set(MR_88, broken());
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (1, 0, 0, 0, 0, 1)
    );
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (1, 0, 0, 0, 0, 1)
    );
    assert_eq!(stores(), before);

    // The host is unreachable.
    sandbox.github.down.store(true, Ordering::SeqCst);
    sandbox.gitlab.down.store(true, Ordering::SeqCst);
    sandbox.sync();
    assert_eq!(stores(), before);
    daemon.stop();
}

#[test]
fn a_poll_with_no_change_writes_and_commits_nothing() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    let stores = || {
        (
            snapshot(&sandbox.store("personal")),
            snapshot(&sandbox.store("company-a")),
        )
    };
    let before = stores();
    assert!(before.0.1 >= 1 && before.1.1 >= 1, "{before:?}");
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (1, 0, 0, 0, 0, 0)
    );
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (1, 0, 0, 0, 0, 0)
    );
    assert_eq!(stores(), before);
    // After a restart, a full read of each MR: the same bytes.
    daemon.stop();
    let daemon = sandbox.start();
    sandbox.sync();
    assert_eq!(stores(), before);
    daemon.stop();
}

#[test]
fn an_mr_of_another_context_writes_nothing() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let result = sandbox.sync();
    assert_eq!(counts(poll(&result, "personal", "gitlab.example.com")).4, 1);
    assert_eq!(counts(poll(&result, "company-a", "github.com")).4, 1);
    for (context, _, _, file, _) in FORGES {
        let names: Vec<String> = entry_files(&sandbox.store(context))
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, vec![file.to_owned()], "{context}");
    }
    daemon.stop();
}

#[test]
fn an_entry_whose_mr_now_belongs_to_another_context_leaves_this_store() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    daemon.stop();
    // `acme` now belongs to `company-a`.
    let config = CONFIG
        .replace(
            "match.hosts = [\"github.com/acme\"]",
            "match.hosts = [\"github.com/nobody\"]",
        )
        .replace(
            "match.hosts = [\"gitlab.example.com/company-a\"]",
            "match.hosts = [\"gitlab.example.com/company-a\", \"github.com/acme\"]",
        );
    fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
    let daemon = sandbox.start();
    sandbox.sync();
    assert_eq!(
        entry_files(&sandbox.store("personal")),
        Vec::<(String, String)>::new()
    );
    let names: Vec<String> = entry_files(&sandbox.store("company-a"))
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(names.contains(&FORGES[0].3.to_owned()), "{names:?}");
    daemon.stop();
}

#[test]
fn my_own_mr_gets_no_entry_and_no_task() {
    let sandbox = Sandbox::new();
    let login = fixture_json("gitlab/user.json")["username"].clone();
    let mut mr = fixture_json("gitlab/merge-request-88.v2.json");
    mr["author"]["username"] = login;
    sandbox
        .gitlab
        .set(MR_88, with_etag(mr.to_string(), "W/\"mine\""));
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    assert_eq!(start_response(&socket, GITLAB_KEY).status, 404);
    assert_eq!(
        task_dirs(&sandbox.store("company-a")),
        Vec::<PathBuf>::new()
    );
    daemon.stop();
}

#[test]
fn an_existing_review_task_behaves_as_before_and_has_no_entry() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    // A Review task of before the queue: in `inbox`, created by a poll, with no mirror yet.
    let created: TaskDetail = ok(&request(
        &socket,
        "POST",
        "/v1/tasks",
        Some(&json!({
            "title": "Review acme/shop#412: Auth refactor",
            "kind": "review",
            "context": "personal",
            "status": "inbox",
            "links": [{"role": "origin", "provider": "github", "key": "acme/shop#412", "url": "https://github.com/acme/shop/pull/412"}],
        })),
    ));
    let id = created.summary.id.to_string();

    // It stays tracked, in its status, gets its mirror, and the MR has no entry.
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (1, 0, 0, 1, 0, 0)
    );
    assert_eq!(status(&socket, &id), Status::Inbox);
    assert_eq!(
        entry_files(&sandbox.store("personal")),
        Vec::<(String, String)>::new()
    );
    assert_eq!(task_dirs(&sandbox.store("personal")).len(), 1);
    assert!(
        sandbox
            .store("personal")
            .join("tasks")
            .join(&id)
            .join(MIRROR)
            .join("mr.json")
            .is_file()
    );

    // My verdict closes it.
    let mut reviews = fixture_json("github/pull-412-reviews.json");
    let mut mine = reviews[0].clone();
    mine["node_id"] = json!("PRR_mine");
    mine["user"]["login"] = json!("carol");
    mine["state"] = json!("APPROVED");
    mine["commit_id"] = json!(V1);
    mine["submitted_at"] = json!("2026-10-22T09:00:00Z");
    reviews.as_array_mut().unwrap().push(mine);
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412/reviews",
        route(reviews.to_string()),
    );
    let mut reviewed = fixture_json("github/pull-412.v1.json");
    reviewed["updated_at"] = json!("2026-10-22T09:00:00Z");
    sandbox
        .github
        .set(PULL, with_etag(reviewed.to_string(), "\"reviewed\""));
    sandbox.sync();
    assert_eq!(status(&socket, &id), Status::Done);
    assert_eq!(
        entry_files(&sandbox.store("personal")),
        Vec::<(String, String)>::new()
    );

    // A new head reopens it; still no entry, still one task.
    sandbox.github.set(
        PULL,
        with_etag(fixture("github/pull-412.v2.json"), "\"v2\""),
    );
    sandbox.sync();
    assert_eq!(status(&socket, &id), Status::Ready);
    assert_eq!(
        entry_files(&sandbox.store("personal")),
        Vec::<(String, String)>::new()
    );
    assert_eq!(task_dirs(&sandbox.store("personal")).len(), 1);
    daemon.stop();
}

#[test]
fn an_entry_left_by_a_crash_in_a_start_goes_at_the_next_poll() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let entries = entry_files(&sandbox.store("personal"));
    let started = start(&socket, GITHUB_KEY);
    // The task exists, and the entry is back, as after a crash before its removal.
    let dir = sandbox.store("personal").join("queue/mrs");
    fs::write(dir.join(&entries[0].0), &entries[0].1).unwrap();
    // A start still gives the task.
    let again = start(&socket, GITHUB_KEY);
    assert!(!again.created);
    assert_eq!(again.task.summary.id, started.task.summary.id);
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")).2,
        1,
        "{result:?}"
    );
    assert_eq!(
        entry_files(&sandbox.store("personal")),
        Vec::<(String, String)>::new()
    );
    assert_eq!(task_dirs(&sandbox.store("personal")).len(), 1);
    daemon.stop();
}

#[test]
fn an_mr_of_an_archived_review_task_can_return_as_an_entry() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let id = start(&socket, GITHUB_KEY).task.summary.id.to_string();
    // Close it, then archive it: loomd indexes the move at its next start.
    let done = request(
        &socket,
        "POST",
        &format!("/v1/tasks/{id}/actions/done"),
        None,
    );
    assert_eq!(done.status, 200, "{}", done.body);
    daemon.stop();
    let store = sandbox.store("personal");
    fs::create_dir_all(store.join("archive/2026")).unwrap();
    fs::rename(
        store.join("tasks").join(&id),
        store.join("archive/2026").join(&id),
    )
    .unwrap();

    // Same head, still requested: nothing is new.
    let daemon = sandbox.start();
    assert_eq!(status(&socket, &id), Status::Done);
    sandbox.sync();
    assert_eq!(entry_files(&store), Vec::<(String, String)>::new());
    assert_eq!(task_dirs(&store), Vec::<PathBuf>::new());

    // A new head: an entry, and the archived task does not change.
    let archived_task =
        fs::read_to_string(store.join("archive/2026").join(&id).join("task.md")).unwrap();
    sandbox.github.set(
        PULL,
        with_etag(fixture("github/pull-412.v2.json"), "\"v2\""),
    );
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")).1,
        1,
        "{result:?}"
    );
    assert_eq!(queue(&socket, Some("personal")).entries[0].head_sha, V2);
    assert_eq!(
        fs::read_to_string(store.join("archive/2026").join(&id).join("task.md")).unwrap(),
        archived_task
    );

    // A start gives a second task for a new round of review, in `active`.
    let started = start(&socket, GITHUB_KEY);
    assert!(started.created);
    let new_id = started.task.summary.id.to_string();
    assert_ne!(new_id, id);
    assert_eq!(entry_files(&store), Vec::<(String, String)>::new());
    // The new mirror knows the versions of the archived one.
    let versions: Value = serde_json::from_str(
        &fs::read_to_string(
            store
                .join("tasks")
                .join(&new_id)
                .join(MIRROR)
                .join("versions.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(versions.as_array().unwrap().len(), 2, "{versions}");
    daemon.stop();
}

/// `mine` as my review of `head`, added to the reviews of the fixture.
fn reviews_with_mine_on(head: &str) -> Route {
    let mut reviews = fixture_json("github/pull-412-reviews.json");
    let mut mine = reviews[0].clone();
    mine["node_id"] = json!("PRR_mine");
    mine["user"]["login"] = json!("carol");
    mine["state"] = json!("COMMENTED");
    mine["commit_id"] = json!(head);
    mine["submitted_at"] = json!("2026-10-22T09:00:00Z");
    reviews.as_array_mut().unwrap().push(mine);
    route(reviews.to_string())
}

/// The state of my reviewer on the GitLab MR.
fn my_reviewer_state(state: &str) -> Route {
    let mut reviewers = fixture_json("gitlab/merge-request-88-reviewers.json");
    reviewers[0]["state"] = json!(state);
    route(reviewers.to_string())
}

/// The GitLab list shows the MR again, as it does for an MR that changed.
fn gitlab_lists_it(sandbox: &Sandbox) {
    sandbox.gitlab.set(
        "GET /merge_requests+after",
        route(fixture("gitlab/merge-requests-reviews-for-me.json")),
    );
}

#[test]
fn a_github_request_again_without_a_push_gives_an_entry() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    // I review at the forge: GitHub removes me from the reviewers.
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412/reviews",
        reviews_with_mine_on(V1),
    );
    github_without_request(&sandbox);
    sandbox.sync();
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 0);
    // The author requests me again, with no push: I am a reviewer again, on a reviewed head.
    let mut pull = fixture_json("github/pull-412.v1.json");
    pull["updated_at"] = json!("2026-10-24T09:00:00Z");
    sandbox
        .github
        .set(PULL, with_etag(pull.to_string(), "\"again\""));
    sandbox.github.set(
        "GET /search/issues",
        route(fixture("github/search-review-requested.json")),
    );
    sandbox.sync();
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);
    // It stays at the next poll.
    sandbox.sync();
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);
    daemon.stop();
}

#[test]
fn a_gitlab_push_on_a_kept_approval_gives_an_entry_and_the_head_survives_a_restart() {
    const V3: &str = "0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e";
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    // I approve at the forge: the entry goes.
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-23T09:00:00.000Z", "opened", "W/\"approved\""),
    );
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    // The head of the approval is machine-local: no store commit for it.
    let before = snapshot(&sandbox.store("company-a"));
    sandbox.sync();
    assert_eq!(snapshot(&sandbox.store("company-a")), before);

    // After a restart, loomd still knows the head of my approval.
    daemon.stop();
    let daemon = sandbox.start();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);

    // A push: the project keeps the approval, and I did not review the push.
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V3, "2026-10-24T09:00:00.000Z", "opened", "W/\"pushed\""),
    );
    gitlab_lists_it(&sandbox);
    sandbox.sync();
    let listed = queue(&socket, Some("company-a"));
    assert_eq!(listed.entries.len(), 1, "{listed:?}");
    assert_eq!(listed.entries[0].head_sha, V3);
    // It stays: the approval is still of the older head.
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    daemon.stop();
}

#[test]
fn an_approval_with_no_row_counts_for_the_head_of_the_poll() {
    const V3: &str = "0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e";
    let sandbox = Sandbox::new();
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    daemon.stop();
    // A rebuilt index has lost the head of the approval.
    for name in ["index.db", "index.db-wal", "index.db-shm"] {
        let _ = fs::remove_file(sandbox.loom_home().join(name));
    }
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V3, "2026-10-24T09:00:00.000Z", "opened", "W/\"pushed\""),
    );
    let daemon = sandbox.start();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    daemon.stop();
}

#[test]
fn an_approval_reset_by_a_push_gives_an_entry_and_a_new_approval_counts_on_the_new_head() {
    const V3: &str = "0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e";
    let sandbox = Sandbox::new();
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    // A push resets the approval.
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(false));
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V3, "2026-10-24T09:00:00.000Z", "opened", "W/\"reset\""),
    );
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    // I approve again: this time on V3, not on the head of the first approval.
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V3, "2026-10-24T10:00:00.000Z", "opened", "W/\"again\""),
    );
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    daemon.stop();
}

#[test]
fn an_approved_mr_is_polled_until_it_leaves_and_then_no_more() {
    let sandbox = Sandbox::new();
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    let daemon = sandbox.start();
    sandbox.sync();
    // Not listed any more, still read: only that shows that it leaves.
    let seen = sandbox.gitlab.requests().len();
    sandbox.sync();
    assert!(
        sandbox.gitlab.requests()[seen..].contains(&MR_88.to_owned()),
        "{:?}",
        &sandbox.gitlab.requests()[seen..]
    );
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-24T11:00:00.000Z", "merged", "W/\"m\""),
    );
    sandbox.sync();
    let seen = sandbox.gitlab.requests().len();
    sandbox.sync();
    assert!(
        !sandbox.gitlab.requests()[seen..].contains(&MR_88.to_owned()),
        "{:?}",
        &sandbox.gitlab.requests()[seen..]
    );
    daemon.stop();
}

#[test]
fn a_gitlab_request_again_on_an_approved_mr_gives_an_entry() {
    let sandbox = Sandbox::new();
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    // Removed and added again: my approval stays, and my reviewer state is `unreviewed`.
    sandbox.gitlab.set(
        &format!("{MR_88}/reviewers"),
        my_reviewer_state("unreviewed"),
    );
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-24T09:00:00.000Z", "opened", "W/\"again\""),
    );
    gitlab_lists_it(&sandbox);
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    // The reviewers are read only for an approved MR without a task.
    let seen = sandbox.gitlab.requests().len();
    let id = start(&socket, GITLAB_KEY).task.summary.id.to_string();
    assert!(
        !sandbox.gitlab.requests()[seen..]
            .iter()
            .any(|line| line.ends_with("/reviewers")),
        "{id}"
    );
    daemon.stop();
}

#[test]
fn the_reviewers_are_not_read_without_an_approval() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    assert!(
        !sandbox
            .gitlab
            .requests()
            .iter()
            .any(|line| line.ends_with("/reviewers"))
    );
    daemon.stop();
}

#[test]
fn a_failed_read_of_the_reviewers_changes_nothing() {
    let sandbox = Sandbox::new();
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    sandbox.gitlab.set(
        &format!("{MR_88}/reviewers"),
        Route {
            status: 404,
            ..route(r#"{"message":"404 Not Found"}"#)
        },
    );
    let daemon = sandbox.start();
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (1, 0, 0, 0, 0, 0)
    );
    assert_eq!(queue(&sandbox.socket(), Some("company-a")).entries.len(), 0);
    daemon.stop();
}

/// A sandbox on GitLab only, where `personal` holds the clones under `~/work`, so a clone of
/// `company-a/checkout` moves its MR to `personal`. `personal` has `credential`.
fn sandbox_for_a_move(credential: &str) -> Sandbox {
    let sandbox = Sandbox::new();
    let config = format!(
        r#"
[repos]
roots = []

[contexts.personal]
match.paths = ["{}"]

[contexts.company-a]

[forges."gitlab.example.com"]
kind = "gitlab"
token_env = "LOOM_TEST_GITLAB_TOKEN"
{credential}
"#,
        sandbox.user_home().join("work").display()
    );
    fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
    sandbox
}

/// A clone of `company-a/checkout`, held by `personal`.
fn register_the_clone(sandbox: &Sandbox) {
    let repo = sandbox.user_home().join("work/checkout");
    fs::create_dir_all(&repo).unwrap();
    let home = sandbox.user_home();
    common::git(&home, &repo, &["init", "-q"]);
    common::git(
        &home,
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://gitlab.example.com/company-a/checkout.git",
        ],
    );
    let registered = request(
        &sandbox.socket(),
        "POST",
        "/v1/repos/register",
        Some(&json!({ "path": repo })),
    );
    assert_eq!(registered.status, 200, "{}", registered.body);
}

#[test]
fn a_moved_entry_goes_to_the_store_of_its_context_when_the_same_account_polls_it() {
    let sandbox = sandbox_for_a_move("");
    // Two contexts on one account find the same MRs, so a rule gives the MR its context. A repo
    // override beats that rule once the clone exists.
    let home = sandbox.user_home();
    let config = fs::read_to_string(sandbox.loom_home().join("config.toml"))
        .unwrap()
        .replace(
            "[contexts.company-a]\n",
            &format!(
                "[contexts.company-a]\nmatch.hosts = [\"gitlab.example.com/company-a\"]\n\n[repos.\"{}\"]\ncontext = \"personal\"\n",
                home.join("work/checkout").display()
            ),
        );
    fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    register_the_clone(&sandbox);

    // No restart: `updated_after` of `personal` does not list this unchanged MR.
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    let moved = queue(&socket, Some("personal"));
    assert_eq!(moved.entries.len(), 1, "{moved:?}");
    assert_eq!(moved.entries[0].key, "company-a/checkout!88");
    // Both stores are committed, and it stays at the next poll.
    for context in ["personal", "company-a"] {
        let status = common::git(
            &PathBuf::from("/tmp"),
            &sandbox.store(context),
            &["status", "--porcelain"],
        );
        assert_eq!(status.trim(), "", "{context}");
    }
    sandbox.sync();
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    daemon.stop();
}

#[test]
fn a_moved_entry_stays_and_is_reported_when_another_account_polls_its_context() {
    let sandbox = sandbox_for_a_move(
        "\n[contexts.personal.forges.\"gitlab.example.com\"]\ntoken_env = \"LOOM_TEST_CLIENT_TOKEN\"\n",
    );
    let mut other = fixture_json("gitlab/user.json");
    other["username"] = json!("carol-client");
    sandbox.gitlab.set(
        &format!("GET /user@{CLIENT_TOKEN}"),
        route(other.to_string()),
    );
    sandbox.gitlab.set(
        &format!("GET /merge_requests@{CLIENT_TOKEN}"),
        route(fixture("gitlab/merge-requests-reviews-for-me.empty.json")),
    );
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    register_the_clone(&sandbox);

    let result = sandbox.sync();
    let ReviewSyncOutcome::Synced { failed, .. } =
        &poll(&result, "company-a", "gitlab.example.com").outcome
    else {
        panic!("{result:?}");
    };
    assert_eq!(failed.len(), 1, "{result:?}");
    assert!(failed[0].error.contains("personal"), "{failed:?}");
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 0);

    // It is still read: a merge removes it.
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-24T11:00:00.000Z", "merged", "W/\"m\""),
    );
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    daemon.stop();
}

#[test]
fn a_404_on_an_entry_means_the_mr_is_gone() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    sandbox.github.routes.lock().unwrap().remove(PULL);
    sandbox.gitlab.routes.lock().unwrap().remove(MR_88);
    sandbox.github.set(
        "GET /search/issues",
        route(fixture("github/search-review-requested.empty.json")),
    );
    let result = sandbox.sync();
    assert_left_the_queue(&sandbox, "personal", "github.com", &result);
    assert_left_the_queue(&sandbox, "company-a", "gitlab.example.com", &result);
    for (context, host) in [
        ("personal", "github.com"),
        ("company-a", "gitlab.example.com"),
    ] {
        assert_eq!(counts(poll(&result, context, host)).5, 0, "{result:?}");
    }
    daemon.stop();
}

#[test]
fn a_403_on_an_entry_stays_a_failure() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    let before = snapshot(&sandbox.store("personal"));
    sandbox.github.set(
        PULL,
        Route {
            status: 403,
            ..route(r#"{"message":"Must have admin rights"}"#)
        },
    );
    sandbox.github.set(
        "GET /search/issues",
        route(fixture("github/search-review-requested.empty.json")),
    );
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")).5,
        1,
        "{result:?}"
    );
    assert_eq!(snapshot(&sandbox.store("personal")), before);
    daemon.stop();
}

#[test]
fn a_404_at_a_start_removes_the_entry_and_answers_not_found() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    sandbox.github.routes.lock().unwrap().remove(PULL);
    sandbox.gitlab.routes.lock().unwrap().remove(MR_88);
    for (context, encoded, key, _, _) in FORGES {
        let refused = start_response(&socket, encoded);
        assert_eq!(refused.status, 404, "{key}: {}", refused.body);
        assert!(refused.body.contains("\"not_found\""), "{}", refused.body);
        let store = sandbox.store(context);
        assert_eq!(entry_files(&store), Vec::<(String, String)>::new(), "{key}");
        assert_eq!(task_dirs(&store), Vec::<PathBuf>::new(), "{key}");
    }
    daemon.stop();
}

#[test]
fn a_404_on_a_review_task_stays_a_failure() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let github = start(&socket, GITHUB_KEY).task.summary.id.to_string();
    let gitlab = start(&socket, GITLAB_KEY).task.summary.id.to_string();
    // No list shows them any more, and the forge answers 404.
    sandbox.github.routes.lock().unwrap().remove(PULL);
    sandbox.github.set(
        "GET /search/issues",
        route(fixture("github/search-review-requested.empty.json")),
    );
    sandbox.gitlab.routes.lock().unwrap().remove(MR_88);
    let before = (
        snapshot(&sandbox.store("personal")),
        snapshot(&sandbox.store("company-a")),
    );
    let result = sandbox.sync();
    for (context, host, id) in [
        ("personal", "github.com", &github),
        ("company-a", "gitlab.example.com", &gitlab),
    ] {
        assert_eq!(counts(poll(&result, context, host)).5, 1, "{result:?}");
        assert_eq!(status(&socket, id), Status::Active);
    }
    assert_eq!(
        (
            snapshot(&sandbox.store("personal")),
            snapshot(&sandbox.store("company-a"))
        ),
        before
    );
    daemon.stop();
}

#[test]
fn a_task_in_another_context_means_no_entry_and_no_second_task() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let first = start(&socket, GITHUB_KEY).task.summary.id;
    daemon.stop();
    // `acme` now belongs to `company-a`: the task stays in `personal`.
    let config = CONFIG
        .replace(
            "match.hosts = [\"github.com/acme\"]",
            "match.hosts = [\"github.com/nobody\"]",
        )
        .replace(
            "match.hosts = [\"gitlab.example.com/company-a\"]",
            "match.hosts = [\"gitlab.example.com/company-a\", \"github.com/acme\"]",
        );
    fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
    let daemon = sandbox.start();
    sandbox.sync();
    assert!(
        !queue(&socket, Some("company-a"))
            .entries
            .iter()
            .any(|entry| entry.key == "acme/shop#412")
    );
    // Without a context, and with the context that the rule now names: the same task.
    let plain = start(&socket, GITHUB_KEY);
    assert!(!plain.created);
    assert_eq!(plain.task.summary.id, first);
    let named: ReviewStartResult = ok(&request(
        &socket,
        "POST",
        &format!("/v1/reviews/queue/{GITHUB_KEY}/start"),
        Some(&json!({"context": "company-a"})),
    ));
    assert!(!named.created);
    assert_eq!(named.task.summary.id, first);
    assert_eq!(task_dirs(&sandbox.store("company-a")).len(), 0);
    assert_eq!(task_dirs(&sandbox.store("personal")).len(), 1);
    daemon.stop();
}

#[test]
fn an_entry_of_another_host_with_this_key_is_not_touched_and_the_mr_is_reported() {
    let sandbox = Sandbox::new();
    let dir = sandbox.store("company-a").join("queue/mrs");
    fs::create_dir_all(&dir).unwrap();
    let foreign = json!({
        "v": 1,
        "key": "company-a/checkout!88",
        "forge_ids": {"forge": "gitlab", "id": 1, "iid": 88, "project_id": 5},
        "title": "On another host",
        "author": "alice",
        "web_url": "https://gitlab.other.example/company-a/checkout/-/merge_requests/88",
        "head_sha": V1,
        "updated_at": "2026-10-22T09:00:00Z",
        "reasons": ["review_requested"]
    })
    .to_string();
    fs::write(dir.join(FORGES[1].3), &foreign).unwrap();
    let daemon = sandbox.start();
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (1, 0, 0, 0, 0, 1)
    );
    assert_eq!(fs::read_to_string(dir.join(FORGES[1].3)).unwrap(), foreign);
    daemon.stop();
}

#[test]
fn an_entry_that_fails_does_not_hold_back_updated_after() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    daemon.stop();
    // After a restart the list has no `updated_after`, and only the entry is read.
    sandbox.gitlab.set(
        "GET /merge_requests",
        route(fixture("gitlab/merge-requests-reviews-for-me.empty.json")),
    );
    sandbox.gitlab.set(
        MR_88,
        Route {
            status: 500,
            ..route(r#"{"message":"boom"}"#)
        },
    );
    let daemon = sandbox.start();
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")).5,
        1
    );
    let seen = sandbox.gitlab.requests().len();
    sandbox.sync();
    let requests = sandbox.gitlab.requests();
    // `personal` polls this host too: both contexts must ask for the changes only.
    let lists = requests[seen..]
        .iter()
        .filter(|line| *line == "GET /merge_requests+after")
        .count();
    assert_eq!(lists, 2, "{:?}", &requests[seen..]);
    daemon.stop();
}

#[test]
fn a_start_while_the_forge_limits_the_rate_does_not_call_the_forge() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    sandbox.github.set(
        "GET /search/issues",
        Route {
            status: 429,
            headers: vec![("retry-after", "3600".to_owned())],
            ..route(r#"{"message":"slow down"}"#)
        },
    );
    let result = sandbox.sync();
    assert!(matches!(
        poll(&result, "personal", "github.com").outcome,
        ReviewSyncOutcome::RateLimited { .. }
    ));
    let seen = sandbox.github.requests().len();
    let refused = start_response(&socket, GITHUB_KEY);
    assert_eq!(refused.status, 502, "{}", refused.body);
    assert!(refused.body.contains("limits the rate"), "{}", refused.body);
    assert_eq!(sandbox.github.requests().len(), seen);
    assert_eq!(entry_files(&sandbox.store("personal")).len(), 1);
    daemon.stop();
}

#[test]
fn a_failed_task_sync_leaves_its_entry() {
    use std::os::unix::fs::PermissionsExt as _;
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let entries = entry_files(&sandbox.store("personal"));
    let id = start(&socket, GITHUB_KEY).task.summary.id.to_string();
    // The entry is back, as after a crash, and the mirror of the task cannot be written.
    let dir = sandbox.store("personal").join("queue/mrs");
    fs::write(dir.join(&entries[0].0), &entries[0].1).unwrap();
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(&id)
        .join(MIRROR);
    fs::set_permissions(&mirror, fs::Permissions::from_mode(0o500)).unwrap();
    sandbox.github.set(
        PULL,
        with_etag(fixture("github/pull-412.v2.json"), "\"v2\""),
    );
    let result = sandbox.sync();
    fs::set_permissions(&mirror, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")).5,
        1,
        "{result:?}"
    );
    assert_eq!(entry_files(&sandbox.store("personal")).len(), 1);
    // The next poll writes the mirror and removes the entry.
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")).2,
        1,
        "{result:?}"
    );
    assert_eq!(entry_files(&sandbox.store("personal")).len(), 0);
    daemon.stop();
}

fn queue(socket: &Path, context: Option<&str>) -> ReviewQueue {
    let path = context.map_or_else(
        || "/v1/reviews/queue".to_owned(),
        |context| format!("/v1/reviews/queue?context={context}"),
    );
    ok(&request(socket, "GET", &path, None))
}

fn start_response(socket: &Path, encoded_key: &str) -> common::HttpResponse {
    request(
        socket,
        "POST",
        &format!("/v1/reviews/queue/{encoded_key}/start"),
        Some(&json!({})),
    )
}

fn start(socket: &Path, encoded_key: &str) -> ReviewStartResult {
    ok(&start_response(socket, encoded_key))
}

fn task_summary_id(socket: &Path, encoded_key: &str) -> String {
    task(socket, encoded_key).summary.id.to_string()
}

/// `orphan/app#5`: no clone and no `match.hosts` rule.
fn orphan(json: &str) -> String {
    json.replace("acme/shop", "orphan/app")
        .replace("/412", "/5")
        .replace("\"number\": 412", "\"number\": 5")
        .replace("\"number\":412", "\"number\":5")
}

/// The search of `GITHUB_TOKEN`, with `orphan/app#5`.
fn with_orphan() -> Value {
    let mut search = fixture_json("github/search-review-requested.json");
    let item: Value = serde_json::from_str(&orphan(&search["items"][0].to_string())).unwrap();
    search["items"].as_array_mut().unwrap().push(item);
    search["total_count"] = json!(2);
    search
}

#[test]
fn an_mr_that_no_rule_assigns_goes_to_no_store_when_another_machine_polls_the_other_context() {
    let sandbox = Sandbox::new();
    // This machine polls `personal` only, and `work` can poll `company-a` with the same token.
    let config = CONFIG.replace(
        "[contexts.company-a]\n",
        "[contexts.company-a]\nmachines = [\"work\"]\n",
    );
    fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
    sandbox
        .github
        .set("GET /search/issues", route(with_orphan().to_string()));
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    let result = sandbox.sync();
    let ReviewSyncOutcome::Synced { failed, .. } = &poll(&result, "personal", "github.com").outcome
    else {
        panic!("{result:?}");
    };
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].key, "orphan/app#5");
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);
    daemon.stop();
}

#[test]
fn an_mr_that_no_rule_assigns_goes_to_the_context_whose_account_found_it() {
    let sandbox = Sandbox::new();
    let config = format!(
        "{CONFIG}\n[contexts.company-a.forges.\"github.com\"]\ntoken_env = \"LOOM_TEST_CLIENT_TOKEN\"\n"
    );
    fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
    // The client account is `dave`, and only `orphan/app#5` requests it.
    let mut user = fixture_json("github/user.json");
    user["login"] = json!("dave");
    let mut search = with_orphan();
    search["items"].as_array_mut().unwrap().remove(0);
    search["total_count"] = json!(1);
    let client = |key: &str| format!("{key}@{CLIENT_TOKEN}");
    sandbox
        .github
        .set(&client("GET /user"), route(user.to_string()));
    sandbox
        .github
        .set(&client("GET /search/issues"), route(search.to_string()));
    for (path, file) in [
        ("GET /repos/orphan/app/pulls/5", "github/pull-412.v1.json"),
        (
            "GET /repos/orphan/app/pulls/5/reviews",
            "github/pull-412-reviews.json",
        ),
        (
            "GET /repos/orphan/app/issues/5/comments",
            "github/issue-412-comments.json",
        ),
    ] {
        sandbox.github.set(path, route(orphan(&fixture(file))));
    }
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    // A login is read at the first poll of its context: the first sync may not tell yet.
    sandbox.sync();
    let second = sandbox.sync();
    let ReviewSyncOutcome::Synced { failed, .. } =
        &poll(&second, "company-a", "github.com").outcome
    else {
        panic!("{second:?}");
    };
    assert!(failed.is_empty(), "{second:?}");
    let company = queue(&socket, Some("company-a"));
    assert!(
        company
            .entries
            .iter()
            .any(|entry| entry.key == "orphan/app#5"),
        "{company:?}"
    );
    // `personal` keeps acme/shop#412 only.
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);
    daemon.stop();
}

#[test]
fn an_mr_that_no_rule_assigns_goes_to_no_store_when_the_token_is_shared() {
    an_mr_that_no_rule_assigns_goes_to_no_store(CONFIG);
}

#[test]
fn an_mr_that_no_rule_assigns_goes_to_no_store_when_each_context_has_a_token() {
    // Two tokens of one account find the same MRs.
    let config = format!(
        "{CONFIG}\n[contexts.company-a.forges.\"github.com\"]\ntoken_env = \"LOOM_TEST_GITHUB_TOKEN\"\n"
    );
    an_mr_that_no_rule_assigns_goes_to_no_store(&config);
}

/// `orphan/app` has no clone and no `match.hosts` rule, and both contexts poll github.com:
/// neither is its context.
fn an_mr_that_no_rule_assigns_goes_to_no_store(config: &str) {
    let sandbox = Sandbox::new();
    fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
    sandbox
        .github
        .set("GET /search/issues", route(with_orphan().to_string()));
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    let result = sandbox.sync();
    for context in ["personal", "company-a"] {
        let ReviewSyncOutcome::Synced { failed, .. } =
            &poll(&result, context, "github.com").outcome
        else {
            panic!("{result:?}");
        };
        assert_eq!(failed.len(), 1, "{context}: {failed:?}");
        assert_eq!(failed[0].key, "orphan/app#5");
        assert!(failed[0].error.contains("match.hosts"), "{failed:?}");
    }
    // Only acme/shop#412, which a rule assigns, has an entry; each MR of a rule, one.
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    assert!(
        !sandbox
            .github
            .requests()
            .iter()
            .any(|request| request.contains("orphan"))
    );
    daemon.stop();
}
