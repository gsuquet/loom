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
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::SystemTime;

use loom_api::{
    Actor, Event, ReviewSync, ReviewSyncOutcome, ReviewSyncResult, Status, TaskDetail, TaskEvents,
    TaskList,
};
use serde_json::{Value, json};
use tempfile::TempDir;

mod common;

use common::{Daemon, ok, request};

const GITHUB_TOKEN: &str = "fake-github-token-0123";
const GITLAB_TOKEN: &str = "fake-gitlab-token-4567";
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
/// adds `+after` (an `updated_after` parameter) or `+page=<n>`. Other requests get a 404.
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
                        }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let key = route_key(&method, &target);
                recorded.lock().unwrap().push(key.clone());
                let found = served.lock().unwrap().get(&key).cloned();
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

/// `(mrs, created, changed, other_context, failed)` of a poll that succeeded.
fn counts(poll: &ReviewSync) -> (usize, usize, usize, usize, usize) {
    match &poll.outcome {
        ReviewSyncOutcome::Synced {
            mrs,
            created,
            changed,
            other_context,
            failed,
        } => (
            *mrs,
            created.len(),
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
        &format!("/v1/tasks?context={context}&status=inbox"),
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
fn mrs_to_review_become_review_tasks_with_a_mirror() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();

    // 1. First poll: one Review task per forge, each in the store of its MR's context.
    let result = sandbox.sync();
    assert_eq!(result.forges.len(), 4, "{result:?}");
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (1, 1, 0, 0, 0)
    );
    assert_eq!(
        counts(poll(&result, "personal", "gitlab.example.com")),
        (0, 0, 0, 1, 0)
    );
    assert_eq!(
        counts(poll(&result, "company-a", "github.com")),
        (0, 0, 0, 1, 0)
    );
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (1, 1, 0, 0, 0)
    );
    let personal = tasks(&socket, "personal").tasks;
    let company = tasks(&socket, "company-a").tasks;
    assert_eq!((personal.len(), company.len()), (1, 1));
    let review = personal[0].task.clone().unwrap();
    assert_eq!(review.kind, loom_api::Kind::Review);
    assert_eq!(review.status, Status::Inbox);
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
        (1, 0, 0, 0, 0)
    );
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")),
        (1, 0, 0, 0, 0)
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
        (1, 0, 1, 0, 0)
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
        (1, 0, 1, 0, 0)
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
        (1, 0, 0, 0, 0)
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
    let id = tasks(&socket, "company-a").tasks[0].id.to_string();

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
        (1, 0, 0, 0, 0)
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
        (0, 0, 0, 0, 0)
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
fn a_poll_that_stops_reads_its_mrs_in_full_again() {
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
    assert_eq!(tasks(&socket, "personal").tasks.len(), 0);

    // 413 is gone: only that MR fails, and 412, unchanged on the forge (its ETag matches),
    // gets its task.
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
        (2, 1, 0, 0, 1)
    );
    assert_eq!(tasks(&socket, "personal").tasks.len(), 1);

    daemon.stop();
}
