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
use std::os::unix::fs::PermissionsExt as _;
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
use loom_store::mr_clones::MrClones;

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

    fn body(&self, key: &str) -> String {
        self.routes.lock().unwrap()[key].body.clone()
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
        self.start_on(None)
    }

    /// Starts loomd, archiving as on `today` (`LOOM_TEST_TODAY`) when given.
    fn start_on(&self, today: Option<&str>) -> Daemon {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
        if let Some(today) = today {
            command.env("LOOM_TEST_TODAY", today);
        }
        let path = std::env::join_paths(
            std::iter::once(self.logging_git_dir())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        command
            .env("PATH", path)
            .env("LOOM_TEST_GIT_LOG", self.git_log())
            .env("LOOM_TEST_GIT_FAIL", self.git_fail())
            .env("LOOM_TEST_MR_PASS_FILE", self.pass_file())
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

    fn pass_file(&self) -> PathBuf {
        self.dir.path().join("mr-passes")
    }

    /// A file with the message of a failure of the fetches of MR 500, when it exists.
    fn git_fail(&self) -> PathBuf {
        self.dir.path().join("git-fail")
    }

    fn git_log(&self) -> PathBuf {
        self.dir.path().join("git.log")
    }

    /// A directory with a `git` that logs where and how it runs, then runs the real one.
    fn logging_git_dir(&self) -> PathBuf {
        let dir = self.dir.path().join("bin");
        if !dir.exists() {
            fs::create_dir_all(&dir).unwrap();
            let which = Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .unwrap();
            let real = String::from_utf8(which.stdout).unwrap();
            let script = format!(
                "#!/bin/sh\nprintf '%s|%s\\n' \"$PWD\" \"$*\" >> \"$LOOM_TEST_GIT_LOG\"\ncase \"$*\" in *--no-write-fetch-head*) if [ -f \"$LOOM_TEST_GIT_FAIL.all\" ]; then cat \"$LOOM_TEST_GIT_FAIL.all\" >&2; exit 128; fi;; esac\ncase \"$*\" in *refs/pull/500/head*) if [ -f \"$LOOM_TEST_GIT_FAIL\" ]; then cat \"$LOOM_TEST_GIT_FAIL\" >&2; exit 128; fi;; esac\nexec '{}' \"$@\"\n",
                real.trim()
            );
            let program = dir.join("git");
            fs::write(&program, script).unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    /// The commands of the logging `git` that contain `what`.
    fn git_commands(&self, what: &str) -> usize {
        fs::read_to_string(self.git_log())
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(what))
            .count()
    }

    fn passes(&self) -> usize {
        fs::read_to_string(self.pass_file())
            .unwrap_or_default()
            .lines()
            .count()
    }

    /// A sync, then the end of the pass of MR refs of each poll that it ran.
    fn sync_and_settle(&self) -> ReviewSyncResult {
        let before = self.passes();
        let result = self.sync();
        let polls = result
            .forges
            .iter()
            .filter(|forge| matches!(forge.outcome, ReviewSyncOutcome::Synced { .. }))
            .count();
        wait_for("the pass of each poll", || self.passes() >= before + polls);
        result
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
    // 412, read before 413 stopped the poll, has its task.
    assert_eq!(tasks(&socket, "personal").tasks.len(), 1);

    // 413 is gone: only that MR fails; 412 is unchanged (its ETag matches), no second task.
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
        (2, 0, 0, 0, 1)
    );
    assert_eq!(tasks(&socket, "personal").tasks.len(), 1);

    daemon.stop();
}

#[test]
fn an_mr_whose_web_url_is_on_another_host_gets_no_task() {
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
            (1, 0, 0, 0, 1)
        );
    }
    assert_eq!(tasks(&socket, "company-a").tasks.len(), 0);
    daemon.stop();
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
    assert_eq!(tasks(&socket, "personal").tasks.len(), 1);
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
    let company = tasks(&socket, "company-a");
    assert!(
        company.tasks.iter().any(|task| task
            .task
            .as_ref()
            .is_some_and(|task| task.title.starts_with("Review orphan/app#5"))),
        "{company:?}"
    );
    // `personal` keeps acme/shop#412 only.
    assert_eq!(tasks(&socket, "personal").tasks.len(), 1);
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
    // Only acme/shop#412, which a rule assigns, has a task.
    assert_eq!(tasks(&socket, "personal").tasks.len(), 1);
    assert_eq!(tasks(&socket, "company-a").tasks.len(), 1);
    assert!(
        !sandbox
            .github
            .requests()
            .iter()
            .any(|request| request.contains("orphan"))
    );
    daemon.stop();
}

const M1: &str = "506ce3f338acb7125973db71a5c43fd062c4f046";
const M2: &str = "ee047f5164ae63069c7237b6f24c66d34f9eff26";

/// A clone of the fixture `scenario.sh` built, whose only remote names `github.com/acme/shop`
/// and is read from the local forge: `(home, forge, clone)`.
fn clone_of_the_scenario(sandbox: &Sandbox) -> (PathBuf, PathBuf, PathBuf) {
    let home = sandbox.user_home();
    let built = Command::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/forge/scenario.sh"),
    )
    .arg(sandbox.dir.path().join("forge"))
    .env("HOME", &home)
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .output()
    .unwrap();
    assert!(built.status.success(), "{built:?}");
    let forge = sandbox.dir.path().join("forge/shop.git");
    let at = |args: &[&str]| common::git(&home, &forge, args);
    at(&["update-ref", "refs/heads/main", M1]);
    at(&["update-ref", "refs/pull/412/head", V1]);
    at(&["symbolic-ref", "HEAD", "refs/heads/main"]);
    let clone = sandbox.dir.path().join("work/shop");
    fs::create_dir_all(clone.parent().unwrap()).unwrap();
    common::git(
        &home,
        sandbox.dir.path(),
        &[
            "clone",
            "-q",
            "-o",
            "upstream",
            forge.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let project = "git@github.com:acme/shop.git";
    common::git(&home, &clone, &["remote", "set-url", "upstream", project]);
    let rewrite = format!("url.{}.insteadOf", forge.display());
    common::git(&home, &clone, &["config", &rewrite, project]);
    (home, forge, clone)
}

/// The configuration of [`CONFIG`], with `work/` as a root of repositories.
fn index_the_clones(sandbox: &Sandbox) {
    let roots = format!(
        "roots = [\"{}\"]",
        sandbox.dir.path().join("work").display()
    );
    fs::write(
        sandbox.loom_home().join("config.toml"),
        CONFIG.replace("roots = []", &roots),
    )
    .unwrap();
}

fn scan(sandbox: &Sandbox) {
    let answer = request(&sandbox.socket(), "POST", "/v1/repos/scan", None);
    assert_eq!(answer.status, 200, "{}", answer.body);
}

/// Waits for `done`: the worker of the MR refs runs after the answer of a sync. The long wait
/// costs time only when a test fails, and a loaded machine can need it.
fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_mins(2);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "never: {what}");
        thread::sleep(common::POLL);
    }
}

fn clones_of(sandbox: &Sandbox, id: &str) -> MrClones {
    let reader = loom_store::Reader::open(&sandbox.loom_home().join("index.db")).unwrap();
    reader
        .mr_clones(&id.parse().unwrap(), "acme/shop#412")
        .unwrap()
}

fn assert_no_path_in_the_store(sandbox: &Sandbox, path: &Path) {
    let text = path.to_str().unwrap();
    for (file, _) in files(&sandbox.store("personal")) {
        let bytes = fs::read(&file).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(text),
            "{} holds {text}",
            file.display()
        );
    }
}

fn the_review_id(sandbox: &Sandbox) -> String {
    let list: TaskList = ok(&request(
        &sandbox.socket(),
        "GET",
        "/v1/tasks?context=personal",
        None,
    ));
    list.tasks[0].id.to_string()
}

fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// My verdict on V1, the next time the MR is read.
fn approve_v1(sandbox: &Sandbox) {
    let mut reviewed = fixture_json("github/pull-412.v1.json");
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
}

#[test]
#[expect(clippy::too_many_lines, reason = "one scenario, step by step")]
fn a_review_task_gets_the_refs_and_a_diff_per_version_of_its_mr() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, clone) = clone_of_the_scenario(&sandbox);
    let daemon = sandbox.start();
    scan(&sandbox);
    let git = |args: &[&str]| common::git(&home, &clone, args).trim().to_owned();

    // 1. V1: the head and the base are fetched, the patch is kept, and the clone is recorded.
    sandbox.sync_and_settle();
    let id = the_review_id(&sandbox);
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(&id)
        .join(MIRROR);
    let patch = |head: &str| mirror.join("diffs").join(format!("{head}.patch"));
    let store_commits = || commits(&sandbox.store("personal"));
    wait_for("the patch of V1", || patch(V1).is_file());
    wait_for("the record of V1", || {
        fs::read(mirror.join("versions.json"))
            .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains("\"diff\": \"local\""))
    });
    let chosen = clones_of(&sandbox, &id).clone.unwrap();
    assert_eq!(chosen.remote, "upstream");
    assert_eq!(chosen.path, clone);
    // A path of this machine: never in the store.
    assert_no_path_in_the_store(&sandbox, &clone);
    let versions = json_file(&mirror.join("versions.json"));
    assert_eq!(versions[0]["merge_base"], M1);
    assert_eq!(git(&["rev-parse", "refs/loom/mr/upstream/412/head"]), V1);
    assert_eq!(git(&["rev-parse", "refs/loom/mr/upstream/412/base"]), M1);

    // 2. A poll keeps the choice: no byte, no commit, and no fetch or diff.
    wait_for("a clean store", || {
        common::git_run(
            &home,
            &sandbox.store("personal"),
            &["status", "--porcelain"],
        )
        .stdout
        .is_empty()
    });
    let mirror_files = files(&mirror);
    let before = store_commits();
    let fetches = sandbox.git_commands("fetch --no-tags --no-write-fetch-head");
    let diffs = sandbox.git_commands(" diff -M ");
    assert_eq!(fetches, 1);
    assert_eq!(diffs, 1);
    sandbox.sync_and_settle();
    assert_eq!(files(&mirror), mirror_files);
    assert_eq!(store_commits(), before);
    assert_eq!(
        sandbox.git_commands("fetch --no-tags --no-write-fetch-head"),
        fetches
    );
    assert_eq!(sandbox.git_commands(" diff -M "), diffs);

    // 2b. The user deletes the refs: they come back, and the patch does not change.
    common::git(
        &home,
        &clone,
        &["update-ref", "-d", "refs/loom/mr/upstream/412/head"],
    );
    sandbox.sync_and_settle();
    assert_eq!(git(&["rev-parse", "refs/loom/mr/upstream/412/head"]), V1);
    assert_eq!(sandbox.git_commands(" diff -M "), diffs);
    assert_eq!(files(&mirror), mirror_files);

    // 3. My verdict on V1 points the reviewed ref at it.
    approve_v1(&sandbox);
    sandbox.sync_and_settle();
    wait_for("the reviewed ref", || {
        common::git_run(
            &home,
            &clone,
            &[
                "rev-parse",
                "-q",
                "--verify",
                "refs/loom/mr/upstream/412/reviewed",
            ],
        )
        .status
        .success()
    });
    assert_eq!(
        git(&["rev-parse", "refs/loom/mr/upstream/412/reviewed"]),
        V1
    );

    // 4. The force push: V2 has its own patch, and only the changed file is new.
    let at_forge = |args: &[&str]| common::git(&home, &forge, args);
    at_forge(&["update-ref", "refs/heads/main", M2]);
    at_forge(&["update-ref", "refs/pull/412/head", V2]);
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412",
        with_etag(fixture("github/pull-412.v2.json"), "\"v2\""),
    );
    sandbox.sync_and_settle();
    wait_for("the patch of V2", || patch(V2).is_file());
    wait_for("the head of V2", || {
        git(&["rev-parse", "refs/loom/mr/upstream/412/head"]) == V2
    });
    let since = loom_forge::diff::since_last_review(
        &fs::read(patch(V1)).unwrap(),
        &fs::read(patch(V2)).unwrap(),
    );
    assert_eq!(since, ["src/auth/jwt.rs"]);
    // Nothing but `refs/loom/` moved in the clone.
    assert_eq!(git(&["rev-parse", "refs/heads/main"]), M1);
    assert_eq!(git(&["rev-parse", "refs/remotes/upstream/main"]), M1);
    assert_eq!(
        git(&["rev-parse", "refs/loom/mr/upstream/412/reviewed"]),
        V1
    );

    // 5. Done, then archived: the refs go with the task.
    let mut verdict_v2 = fixture_json("github/pull-412-reviews.json");
    let mut mine = verdict_v2[0].clone();
    mine["node_id"] = json!("PRR_kwDOAAATic4AACarol2");
    mine["user"]["login"] = json!("carol");
    mine["state"] = json!("APPROVED");
    mine["commit_id"] = json!(V2);
    mine["submitted_at"] = json!("2026-10-23T09:00:00Z");
    verdict_v2.as_array_mut().unwrap().push(mine);
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412/reviews",
        route(verdict_v2.to_string()),
    );
    let mut merged = fixture_json("github/pull-412.v2.json");
    merged["updated_at"] = json!("2026-10-23T09:00:00Z");
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412",
        with_etag(merged.to_string(), "\"v2-reviewed\""),
    );
    sandbox.sync_and_settle();
    assert_eq!(
        task(&sandbox.socket(), &id).summary.task.unwrap().status,
        Status::Done
    );
    wait_for("the reviewed ref on V2", || {
        git(&["rev-parse", "refs/loom/mr/upstream/412/reviewed"]) == V2
    });
    daemon.stop();
    let daemon = sandbox.start_on(Some("2099-01-01"));
    wait_for("the archive of the task", || {
        !sandbox.store("personal").join("tasks").join(&id).exists()
    });
    sandbox.sync_and_settle();
    assert_eq!(git(&["for-each-ref", "refs/loom/"]), "");
    // Nothing of the task is left on this machine.
    assert_eq!(clones_of(&sandbox, &id), MrClones::default());
    daemon.stop();
}

#[test]
fn a_git_failure_never_fails_the_sync_and_the_next_poll_does_the_work() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, clone) = clone_of_the_scenario(&sandbox);
    let moved = forge.with_extension("moved");
    fs::rename(&forge, &moved).unwrap();
    let daemon = sandbox.start();
    scan(&sandbox);
    let result = sandbox.sync_and_settle();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (1, 1, 0, 0, 0)
    );
    let id = the_review_id(&sandbox);
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(&id)
        .join(MIRROR);
    assert!(mirror.join("mr.json").is_file());
    assert!(!mirror.join("diffs").exists());
    let refs = common::git(&home, &clone, &["for-each-ref", "refs/loom/"]);
    assert_eq!(refs, "");

    // A busy lock is the same: then the MR does not change (304), and the work is done.
    fs::rename(&moved, &forge).unwrap();
    let lock = clone.join(".git/refs/loom/mr/upstream/412");
    fs::create_dir_all(&lock).unwrap();
    fs::write(lock.join("head.lock"), "").unwrap();
    let result = sandbox.sync_and_settle();
    assert!(
        poll(&result, "personal", "github.com")
            .last_success
            .is_some()
    );
    assert!(!mirror.join("diffs").exists());
    assert!(lock.join("head.lock").exists());
    fs::remove_file(lock.join("head.lock")).unwrap();
    sandbox.sync_and_settle();
    wait_for("the patch after the failures", || {
        mirror.join("diffs").join(format!("{V1}.patch")).is_file()
    });
    daemon.stop();
}

#[test]
fn without_a_clone_the_diff_comes_from_the_forge_and_is_reduced_until_a_clone_comes() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    fs::create_dir_all(sandbox.dir.path().join("work")).unwrap();
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412",
        with_etag(fixture("github/pull-412.v2.json"), "\"v2\""),
    );
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412/files",
        route(fixture("github/pull-412-files.v2.json")),
    );
    sandbox.gitlab.set(
        "GET /projects/77/merge_requests/88/diffs",
        route(fixture("gitlab/merge-request-88-diffs.json")),
    );
    let daemon = sandbox.start();
    sandbox.sync_and_settle();

    let github = sandbox
        .store("personal")
        .join("tasks")
        .join(the_review_id(&sandbox))
        .join(MIRROR);
    let reduced = |mirror: &Path| {
        fs::read(mirror.join("versions.json"))
            .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains("\"diff\": \"reduced\""))
    };
    wait_for("the reduced diff on GitHub", || reduced(&github));
    let versions = json_file(&github.join("versions.json"));
    assert!(versions[0]["merge_base"].is_null(), "{versions}");
    let id = the_review_id(&sandbox);
    assert_eq!(clones_of(&sandbox, &id), MrClones::default());
    let patch = fs::read(github.join("diffs").join(format!("{V2}.patch"))).unwrap();
    let paths: Vec<String> = loom_forge::diff::file_hashes(&patch)
        .into_iter()
        .map(|file| file.path)
        .collect();
    assert_eq!(paths, ["Cargo.lock", "src/api/login.rs", "src/auth/jwt.rs"]);

    let list: TaskList = ok(&request(
        &sandbox.socket(),
        "GET",
        "/v1/tasks?context=company-a",
        None,
    ));
    let gitlab = sandbox
        .store("company-a")
        .join("tasks")
        .join(list.tasks[0].id.to_string())
        .join("mrs/gitlab-company-a%2Fcheckout%2188");
    wait_for("the reduced diff on GitLab", || reduced(&gitlab));
    let gitlab_patch = fs::read(gitlab.join("diffs").join(format!("{V2}.patch"))).unwrap();
    assert_eq!(
        loom_forge::diff::file_hashes(&gitlab_patch),
        loom_forge::diff::file_hashes(&patch)
    );

    // No new write until something changes.
    let requests = || {
        sandbox
            .github
            .requests()
            .iter()
            .filter(|request| request.ends_with("/files"))
            .count()
    };
    let read = requests();
    sandbox.sync_and_settle();
    assert_eq!(requests(), read);

    // A clone comes: the full diff replaces the reduced one.
    let (home, forge, clone) = clone_of_the_scenario(&sandbox);
    common::git(&home, &forge, &["update-ref", "refs/heads/main", M2]);
    common::git(&home, &forge, &["update-ref", "refs/pull/412/head", V2]);
    scan(&sandbox);
    sandbox.sync_and_settle();
    wait_for("the full diff", || {
        fs::read(github.join("versions.json"))
            .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains("\"diff\": \"local\""))
    });
    assert_eq!(clones_of(&sandbox, &id).clone.unwrap().path, clone);
    assert_eq!(
        json_file(&github.join("versions.json"))[0]["merge_base"],
        M2
    );
    daemon.stop();
}

/// The search lists 412 and each of `numbers`, the same pull request but its number and base.
fn more_mrs(sandbox: &Sandbox, numbers: &[(u64, &str)]) {
    let mut search = fixture_json("github/search-review-requested.json");
    for (number, base) in numbers {
        let html = format!("https://github.com/acme/shop/pull/{number}");
        let api = format!("https://api.github.com/repos/acme/shop/pulls/{number}");
        let mut item = search["items"][0].clone();
        item["number"] = json!(number);
        item["url"] = json!(format!(
            "https://api.github.com/repos/acme/shop/issues/{number}"
        ));
        item["html_url"] = json!(html);
        item["pull_request"]["url"] = json!(api);
        item["pull_request"]["html_url"] = json!(html);
        search["items"].as_array_mut().unwrap().push(item);
        let mut pull = fixture_json("github/pull-412.v1.json");
        pull["number"] = json!(number);
        pull["html_url"] = json!(html);
        pull["base"]["ref"] = json!(base);
        let path = format!("/repos/acme/shop/pulls/{number}");
        sandbox
            .github
            .set(&format!("GET {path}"), route(pull.to_string()));
        sandbox.github.set(
            &format!("GET {path}/reviews"),
            route(fixture("github/pull-412-reviews.json")),
        );
        sandbox.github.set(
            &format!("GET /repos/acme/shop/issues/{number}/comments"),
            route(fixture("github/issue-412-comments.json")),
        );
    }
    sandbox
        .github
        .set("GET /search/issues", route(search.to_string()));
}

#[test]
fn one_mr_that_fails_costs_the_others_nothing_and_backs_off() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, _clone) = clone_of_the_scenario(&sandbox);
    // 413 to 415 have a base branch that is gone; 416 has no head ref on the forge.
    for number in [413, 414, 415] {
        common::git(
            &home,
            &forge,
            &["update-ref", &format!("refs/pull/{number}/head"), V1],
        );
    }
    more_mrs(
        &sandbox,
        &[
            (413, "gone-branch"),
            (414, "gone-branch"),
            (415, "gone-branch"),
            (416, "main"),
        ],
    );
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/416/files",
        route(fixture("github/pull-412-files.v2.json")),
    );
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();

    let list: TaskList = ok(&request(
        &sandbox.socket(),
        "GET",
        "/v1/tasks?context=personal",
        None,
    ));
    assert_eq!(list.tasks.len(), 5);
    let tasks_dir = sandbox.store("personal").join("tasks");
    let mirror_of = |number: u64| {
        list.tasks.iter().find_map(|summary| {
            let dir = tasks_dir
                .join(summary.id.as_str())
                .join(format!("mrs/github-acme%2Fshop%23{number}"));
            dir.is_dir().then_some(dir)
        })
    };
    let patch_of = |number: u64| {
        mirror_of(number)
            .unwrap()
            .join("diffs")
            .join(format!("{V1}.patch"))
            .is_file()
    };
    // Whatever the order of the tasks: each MR with a head has its diff after the first poll.
    assert!([412, 413, 414, 415].into_iter().all(patch_of));
    // 416 has the patches of the forge.
    assert!(patch_of(416));
    let versions = json_file(&mirror_of(416).unwrap().join("versions.json"));
    assert_eq!(versions[0]["diff"], "reduced");

    // 416 is not fetched at each poll: of polls 2 to 4, only 2 and 4 try it.
    let tries = || sandbox.git_commands("refs/pull/416/head");
    assert_eq!(tries(), 1);
    for _ in 0..3 {
        sandbox.sync_and_settle();
    }
    assert_eq!(tries(), 3);
    // Its tries do not read the forge again: it has these patches.
    let reads = sandbox
        .github
        .requests()
        .iter()
        .filter(|request| request.ends_with("/pulls/416/files"))
        .count();
    assert_eq!(reads, 1);
    daemon.stop();
}

#[test]
fn the_policy_off_stops_updates_and_not_the_refs_of_an_mr() {
    let sandbox = Sandbox::new();
    let roots = format!(
        "roots = [\"{}\"]",
        sandbox.dir.path().join("work").display()
    );
    let config = CONFIG.replace("roots = []", &roots).replace(
        "[contexts.personal]\n",
        "[contexts.personal]\nupdate = \"off\"\n",
    );
    fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
    let (home, _forge, clone) = clone_of_the_scenario(&sandbox);
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(the_review_id(&sandbox))
        .join(MIRROR);
    let versions = fs::read_to_string(mirror.join("versions.json")).unwrap();
    assert!(versions.contains("\"diff\": \"local\""), "{versions}");
    let head = common::git(
        &home,
        &clone,
        &["rev-parse", "refs/loom/mr/upstream/412/head"],
    );
    assert_eq!(head.trim(), V1);
    // Automatic updates still run nothing: the only fetch is the one of the MR.
    assert_eq!(sandbox.git_commands("fetch "), 1);
    daemon.stop();
}

#[test]
fn a_new_clone_gets_the_refs_and_the_old_one_loses_them() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, _forge, first) = clone_of_the_scenario(&sandbox);
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(the_review_id(&sandbox))
        .join(MIRROR);
    let id = the_review_id(&sandbox);
    assert_eq!(clones_of(&sandbox, &id).clone.unwrap().path, first);
    let mr_file = fs::read(mirror.join("mr.json")).unwrap();
    let head = "refs/loom/mr/upstream/412/head";
    assert_eq!(common::git(&home, &first, &["rev-parse", head]).trim(), V1);

    // A second clone, then the first no longer has the remote of the project.
    let second = sandbox.dir.path().join("work/other");
    common::git(
        &home,
        sandbox.dir.path(),
        &[
            "clone",
            "-q",
            "-o",
            "upstream",
            first.to_str().unwrap(),
            second.to_str().unwrap(),
        ],
    );
    let url = common::git(&home, &first, &["config", "--get-regexp", "^url\\."]);
    let (name, value) = url.trim().split_once(' ').unwrap();
    common::git(&home, &second, &["config", name, value]);
    common::git(&home, &second, &["remote", "set-url", "upstream", value]);
    common::git(
        &home,
        &first,
        &["remote", "rename", "upstream", "elsewhere"],
    );
    common::git(
        &home,
        &first,
        &[
            "remote",
            "set-url",
            "elsewhere",
            "git@github.com:me/shop.git",
        ],
    );
    scan(&sandbox);
    sandbox.sync_and_settle();

    assert_eq!(clones_of(&sandbox, &id).clone.unwrap().path, second);
    // The store does not change with the clone.
    assert_eq!(fs::read(mirror.join("mr.json")).unwrap(), mr_file);
    // The patch is local already: only the refs had to come.
    assert_eq!(common::git(&home, &second, &["rev-parse", head]).trim(), V1);
    assert_eq!(
        common::git(&home, &first, &["for-each-ref", "refs/loom/"]),
        ""
    );
    assert_eq!(clones_of(&sandbox, &id).retired, []);
    daemon.stop();
}

#[test]
fn git_does_not_inherit_the_forge_tokens() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (_home, _forge, clone) = clone_of_the_scenario(&sandbox);
    let seen = sandbox.dir.path().join("hook-env");
    let hook = clone.join(".git/hooks/reference-transaction");
    // Only the fetch of the MR: automatic updates of the repository use another runner.
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nif grep -q refs/loom/; then env >> '{}'; fi\n",
            seen.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();
    let env = fs::read_to_string(&seen).unwrap();
    assert!(env.contains("GIT_TERMINAL_PROMPT=0"), "{env}");
    for secret in [
        GITHUB_TOKEN,
        GITLAB_TOKEN,
        "LOOM_TEST_GITHUB_TOKEN",
        "LOOM_TEST_GITLAB_TOKEN",
    ] {
        assert!(!env.contains(secret), "{secret} in the environment of git");
    }
    daemon.stop();
}

#[test]
fn a_retargeted_mr_gets_a_new_diff_with_the_same_head() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (_home, _forge, _clone) = clone_of_the_scenario(&sandbox);
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(the_review_id(&sandbox))
        .join(MIRROR);
    assert_eq!(
        json_file(&mirror.join("versions.json"))[0]["merge_base"],
        M1
    );
    assert_eq!(sandbox.git_commands(" diff -M "), 1);

    // The same head on another target branch.
    let mut moved = fixture_json("github/pull-412.v1.json");
    moved["base"]["ref"] = json!("release");
    moved["base"]["sha"] = json!(V1);
    moved["updated_at"] = json!("2026-10-22T09:00:00Z");
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412",
        with_etag(moved.to_string(), "\"v1-retargeted\""),
    );
    sandbox.sync_and_settle();
    let versions = json_file(&mirror.join("versions.json"));
    assert_eq!(versions.as_array().unwrap().len(), 1);
    assert_eq!(versions[0]["base_sha"], V1);
    assert_eq!(versions[0]["merge_base"], V1);
    assert_eq!(versions[0]["diff"], "local");
    assert_eq!(sandbox.git_commands(" diff -M "), 2);
    daemon.stop();
}

#[test]
fn a_moved_tip_of_the_github_target_branch_does_nothing() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (_home, _forge, _clone) = clone_of_the_scenario(&sandbox);
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(the_review_id(&sandbox))
        .join(MIRROR);
    let versions = fs::read(mirror.join("versions.json")).unwrap();
    let commits_before = commits(&sandbox.store("personal"));
    let fetches = sandbox.git_commands("fetch --no-tags --no-write-fetch-head");

    // No push and no retarget: GitHub shows the new tip of `main`.
    let mut moved = fixture_json("github/pull-412.v1.json");
    moved["base"]["sha"] = json!(M2);
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412",
        with_etag(moved.to_string(), "\"v1-tip\""),
    );
    sandbox.sync_and_settle();
    assert_eq!(fs::read(mirror.join("versions.json")).unwrap(), versions);
    assert_eq!(commits(&sandbox.store("personal")), commits_before);
    assert_eq!(
        sandbox.git_commands("fetch --no-tags --no-write-fetch-head"),
        fetches
    );
    assert_eq!(sandbox.git_commands(" diff -M "), 1);
    daemon.stop();
}

#[test]
fn a_moved_tip_of_the_gitlab_target_branch_does_nothing() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    fs::create_dir_all(sandbox.dir.path().join("work")).unwrap();
    sandbox.gitlab.set(
        "GET /projects/77/merge_requests/88/diffs",
        route(fixture("gitlab/merge-request-88-diffs.json")),
    );
    let daemon = sandbox.start();
    sandbox.sync_and_settle();
    let list: TaskList = ok(&request(
        &sandbox.socket(),
        "GET",
        "/v1/tasks?context=company-a",
        None,
    ));
    let mirror = sandbox
        .store("company-a")
        .join("tasks")
        .join(list.tasks[0].id.to_string())
        .join("mrs/gitlab-company-a%2Fcheckout%2188");
    let versions = fs::read(mirror.join("versions.json")).unwrap();
    assert!(String::from_utf8_lossy(&versions).contains("\"diff\": \"reduced\""));
    let reads = || {
        sandbox
            .gitlab
            .requests()
            .iter()
            .filter(|request| request.ends_with("/diffs"))
            .count()
    };
    let read = reads();

    // The MR changed, on the same branch and with the same versions: its `diff_refs` moved.
    let mut moved = fixture_json("gitlab/merge-request-88.v2.json");
    moved["diff_refs"]["start_sha"] = json!(V1);
    moved["updated_at"] = json!("2026-10-24T09:00:00Z");
    sandbox.gitlab.set(
        "GET /projects/77/merge_requests/88",
        with_etag(moved.to_string(), "W/\"88tip\""),
    );
    sandbox.sync_and_settle();
    assert_eq!(fs::read(mirror.join("versions.json")).unwrap(), versions);
    assert_eq!(reads(), read);
    daemon.stop();
}

#[test]
fn a_patch_of_the_forge_is_recorded_on_the_base_of_versions_json_after_a_moved_tip() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    fs::create_dir_all(sandbox.dir.path().join("work")).unwrap();
    // No patch at the first poll.
    let daemon = sandbox.start();
    sandbox.sync_and_settle();
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(the_review_id(&sandbox))
        .join(MIRROR);
    assert!(!mirror.join("diffs").exists());
    // Then the tip moves, and the forge answers: the record keeps its first base.
    let mut moved = fixture_json("github/pull-412.v1.json");
    moved["base"]["sha"] = json!(M2);
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412",
        with_etag(moved.to_string(), "\"v1-tip\""),
    );
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412/files",
        route(fixture("github/pull-412-files.v2.json")),
    );
    sandbox.sync_and_settle();
    let versions = json_file(&mirror.join("versions.json"));
    assert_eq!(versions[0]["base_sha"], M1);
    assert_eq!(versions[0]["diff"], "reduced");
    daemon.stop();
}

#[test]
fn a_gitlab_head_missing_from_the_versions_is_still_a_version() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    fs::create_dir_all(sandbox.dir.path().join("work")).unwrap();
    // The background job did not add the head yet.
    let mut listed = fixture_json("gitlab/merge-request-88-versions.json");
    listed
        .as_array_mut()
        .unwrap()
        .retain(|version| version["head_commit_sha"] != V2);
    sandbox.gitlab.set(
        "GET /projects/77/merge_requests/88/versions",
        route(listed.to_string()),
    );
    let daemon = sandbox.start();
    sandbox.sync_and_settle();
    let list: TaskList = ok(&request(
        &sandbox.socket(),
        "GET",
        "/v1/tasks?context=company-a",
        None,
    ));
    let versions = json_file(
        &sandbox
            .store("company-a")
            .join("tasks")
            .join(list.tasks[0].id.to_string())
            .join("mrs/gitlab-company-a%2Fcheckout%2188/versions.json"),
    );
    let heads: Vec<&str> = versions
        .as_array()
        .unwrap()
        .iter()
        .map(|version| version["head_sha"].as_str().unwrap())
        .collect();
    assert_eq!(heads, [V1, V2]);
    daemon.stop();
}

#[test]
fn a_diff_over_the_cap_falls_back_to_the_forge_and_says_so() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, clone) = clone_of_the_scenario(&sandbox);
    // A head on M1 with a text file that is larger than the cap of a patch.
    let big = clone.join("big.txt");
    fs::write(&big, "line\n".repeat(1_200_000)).unwrap();
    let git = |args: &[&str]| common::git(&home, &clone, args).trim().to_owned();
    git(&["checkout", "-q", "-b", "big", M1]);
    git(&["add", "big.txt"]);
    git(&[
        "-c",
        "user.name=a",
        "-c",
        "user.email=a@a",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "big",
    ]);
    let head = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-q", "main"]);
    let forge_url = forge.to_str().unwrap().to_owned();
    git(&[
        "push",
        "-q",
        &forge_url,
        &format!("+{head}:refs/pull/412/head"),
    ]);
    let mut pull = fixture_json("github/pull-412.v1.json");
    pull["head"]["sha"] = json!(head);
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412",
        with_etag(pull.to_string(), "\"big\""),
    );
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412/files",
        route(fixture("github/pull-412-files.v2.json")),
    );
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(the_review_id(&sandbox))
        .join(MIRROR);
    let versions = json_file(&mirror.join("versions.json"));
    assert_eq!(versions[0]["diff"], "reduced_too_large");
    let patch = fs::read(mirror.join("diffs").join(format!("{head}.patch"))).unwrap();
    assert!(patch.starts_with(b"# loom: the local diff is over the limit"));
    assert_eq!(loom_forge::diff::file_hashes(&patch).len(), 3);

    // A new try would give the same result.
    let diffs = sandbox.git_commands(" diff -M ");
    sandbox.sync_and_settle();
    assert_eq!(sandbox.git_commands(" diff -M "), diffs);
    daemon.stop();
}

/// A second clone of the forge, whose remote names `github.com/acme/private`.
fn clone_of_the_private_project(sandbox: &Sandbox, home: &Path, forge: &Path) -> PathBuf {
    let clone = sandbox.dir.path().join("work/private");
    common::git(
        home,
        sandbox.dir.path(),
        &[
            "clone",
            "-q",
            "-o",
            "upstream",
            forge.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let project = "git@github.com:acme/private.git";
    common::git(home, &clone, &["remote", "set-url", "upstream", project]);
    let rewrite = format!("url.{}.insteadOf", forge.display());
    common::git(home, &clone, &["config", &rewrite, project]);
    clone
}

/// The search lists `acme/private#500` with the MRs of `more_mrs`.
fn private_mr(sandbox: &Sandbox, numbers: &[(u64, &str)]) {
    more_mrs(sandbox, numbers);
    let mut search = fixture_json("github/search-review-requested.json");
    let as_private = |text: String| {
        text.replace("acme/shop", "acme/private")
            .replace("/412", "/500")
            .replace("\"number\":412", "\"number\":500")
    };
    let item: Value = serde_json::from_str(&as_private(search["items"][0].to_string())).unwrap();
    search = serde_json::from_str(&sandbox.github.body("GET /search/issues")).unwrap();
    search["items"].as_array_mut().unwrap().push(item);
    sandbox
        .github
        .set("GET /search/issues", route(search.to_string()));
    let pull: Value =
        serde_json::from_str(&as_private(fixture("github/pull-412.v1.json"))).unwrap();
    let mut pull = pull;
    pull["number"] = json!(500);
    let path = "/repos/acme/private";
    sandbox
        .github
        .set(&format!("GET {path}/pulls/500"), route(pull.to_string()));
    sandbox.github.set(
        &format!("GET {path}/pulls/500/reviews"),
        route(fixture("github/pull-412-reviews.json")),
    );
    sandbox.github.set(
        &format!("GET {path}/issues/500/comments"),
        route(fixture("github/issue-412-comments.json")),
    );
    sandbox.github.set(
        &format!("GET {path}/pulls/500/files"),
        route(fixture("github/pull-412-files.v2.json")),
    );
}

/// The directories of the mirror of each task, by the number of its MR.
fn mirrors(sandbox: &Sandbox) -> HashMap<u64, PathBuf> {
    let list: TaskList = ok(&request(
        &sandbox.socket(),
        "GET",
        "/v1/tasks?context=personal",
        None,
    ));
    let mut found = HashMap::new();
    for summary in &list.tasks {
        let dir = sandbox
            .store("personal")
            .join("tasks")
            .join(summary.id.as_str())
            .join("mrs");
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let number = name.rsplit("%23").next().unwrap().parse().unwrap();
            found.insert(number, entry.path());
        }
    }
    found
}

fn fetches_of_500(sandbox: &Sandbox) -> usize {
    sandbox.git_commands("refs/pull/500/head")
}

#[test]
fn an_http_refusal_for_one_project_stops_only_its_clone() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, _clone) = clone_of_the_scenario(&sandbox);
    clone_of_the_private_project(&sandbox, &home, &forge);
    common::git(&home, &forge, &["update-ref", "refs/pull/500/head", V1]);
    for number in [413, 414] {
        common::git(
            &home,
            &forge,
            &["update-ref", &format!("refs/pull/{number}/head"), V1],
        );
    }
    private_mr(&sandbox, &[(413, "main"), (414, "main")]);
    fs::write(
        sandbox.git_fail(),
        "fatal: unable to access 'https://github.com/acme/private.git/': The requested URL returned error: 403\n",
    )
    .unwrap();
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();

    // Whatever the order of the tasks: the same endpoint serves the others in the same poll.
    let mirrors = mirrors(&sandbox);
    assert_eq!(mirrors.len(), 4);
    for number in [412, 413, 414] {
        assert_eq!(
            json_file(&mirrors[&number].join("versions.json"))[0]["diff"],
            "local",
            "{number}"
        );
    }
    // 500 has the patches of the forge, and backs off.
    assert_eq!(
        json_file(&mirrors[&500].join("versions.json"))[0]["diff"],
        "reduced"
    );
    for _ in 0..5 {
        sandbox.sync_and_settle();
    }
    // Polls 1, 2 and 4 of 6.
    assert_eq!(fetches_of_500(&sandbox), 3);
    daemon.stop();
}

#[test]
fn a_network_failure_stops_its_endpoint_for_one_poll_and_its_mr_cannot_hold_the_others() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, _clone) = clone_of_the_scenario(&sandbox);
    clone_of_the_private_project(&sandbox, &home, &forge);
    common::git(&home, &forge, &["update-ref", "refs/pull/500/head", V1]);
    private_mr(&sandbox, &[]);
    fs::write(
        sandbox.git_fail(),
        "fatal: unable to access 'https://github.com/': Could not resolve host: github.com\n",
    )
    .unwrap();
    let daemon = sandbox.start();
    scan(&sandbox);
    // Poll 1 and 2 may try 500 first, and stop 412 behind it. At poll 3, 500 waits.
    for _ in 0..3 {
        sandbox.sync_and_settle();
    }
    let mirrors = mirrors(&sandbox);
    assert_eq!(
        json_file(&mirrors[&412].join("versions.json"))[0]["diff"],
        "local"
    );
    assert_eq!(
        json_file(&mirrors[&500].join("versions.json"))[0]["diff"],
        "reduced"
    );
    assert!(fetches_of_500(&sandbox) <= 2);
    daemon.stop();
}

#[test]
fn a_remote_that_is_gone_is_tried_once_for_all_the_mrs_of_its_clone() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (_home, forge, _clone) = clone_of_the_scenario(&sandbox);
    fs::rename(&forge, forge.with_extension("moved")).unwrap();
    more_mrs(&sandbox, &[(413, "main"), (414, "main")]);
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();
    assert_eq!(
        sandbox.git_commands("fetch --no-tags --no-write-fetch-head"),
        1
    );
    daemon.stop();
}

#[test]
fn a_forge_that_refuses_the_token_is_read_once_for_all_the_mrs_without_a_clone() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    fs::create_dir_all(sandbox.dir.path().join("work")).unwrap();
    more_mrs(&sandbox, &[(413, "main"), (414, "main")]);
    for number in [412, 413, 414] {
        sandbox.github.set(
            &format!("GET /repos/acme/shop/pulls/{number}/files"),
            Route {
                status: 401,
                ..route(r#"{"message":"Bad credentials"}"#)
            },
        );
    }
    let daemon = sandbox.start();
    sandbox.sync_and_settle();
    let reads = sandbox
        .github
        .requests()
        .iter()
        .filter(|request| request.ends_with("/files"))
        .count();
    assert_eq!(reads, 1);
    daemon.stop();
}

#[test]
fn the_cleanup_leaves_the_refs_of_a_remote_that_now_names_another_project() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, _forge, clone) = clone_of_the_scenario(&sandbox);
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();
    let id = the_review_id(&sandbox);
    approve_v1(&sandbox);
    sandbox.sync_and_settle();
    assert_eq!(
        task(&sandbox.socket(), &id).summary.task.unwrap().status,
        Status::Done
    );
    common::git(
        &home,
        &clone,
        &[
            "remote",
            "set-url",
            "upstream",
            "git@github.com:acme/other.git",
        ],
    );
    daemon.stop();
    let daemon = sandbox.start_on(Some("2099-01-01"));
    wait_for("the archive of the task", || {
        !sandbox.store("personal").join("tasks").join(&id).exists()
    });
    scan(&sandbox);
    sandbox.sync_and_settle();
    let refs = common::git(
        &home,
        &clone,
        &["rev-parse", "refs/loom/mr/upstream/412/head"],
    );
    assert_eq!(refs.trim(), V1);
    daemon.stop();
}

#[test]
fn a_new_index_chooses_the_clone_again_and_the_cleanup_still_finds_the_refs() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, _forge, clone) = clone_of_the_scenario(&sandbox);
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();
    let id = the_review_id(&sandbox);
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(&id)
        .join(MIRROR);
    let before = files(&mirror);

    // A reindex rebuilds the tasks, and leaves the choice.
    let answer = request(&sandbox.socket(), "POST", "/v1/tasks/reindex", None);
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(clones_of(&sandbox, &id).clone.unwrap().path, clone);
    daemon.stop();

    // A new `index.db`: the same rule gives the same clone, and the store does not change.
    for name in ["index.db", "index.db-wal", "index.db-shm"] {
        let _ = fs::remove_file(sandbox.loom_home().join(name));
    }
    let daemon = sandbox.start();
    scan(&sandbox);
    wait_for("the task in the new index", || {
        let list: TaskList = ok(&request(
            &sandbox.socket(),
            "GET",
            "/v1/tasks?context=personal",
            None,
        ));
        !list.tasks.is_empty()
    });
    sandbox.sync_and_settle();
    assert_eq!(clones_of(&sandbox, &id).clone.unwrap().path, clone);
    assert_eq!(files(&mirror), before);
    approve_v1(&sandbox);
    sandbox.sync_and_settle();
    daemon.stop();

    // A new `index.db` again, then the archive: no row knows the clone.
    for name in ["index.db", "index.db-wal", "index.db-shm"] {
        let _ = fs::remove_file(sandbox.loom_home().join(name));
    }
    let daemon = sandbox.start_on(Some("2099-01-01"));
    wait_for("the archive of the task", || {
        !sandbox.store("personal").join("tasks").join(&id).exists()
    });
    scan(&sandbox);
    sandbox.sync_and_settle();
    assert_eq!(
        common::git(&home, &clone, &["for-each-ref", "refs/loom/"]),
        ""
    );
    daemon.stop();
}

#[test]
fn an_older_version_over_the_cap_keeps_a_note_and_the_head_its_diff() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, clone) = clone_of_the_scenario(&sandbox);
    let git = |args: &[&str]| common::git(&home, &clone, args).trim().to_owned();
    // The project is the one of the GitLab merge request.
    let project = "git@gitlab.example.com:company-a/checkout.git";
    git(&["remote", "set-url", "upstream", project]);
    git(&[
        "config",
        &format!("url.{}.insteadOf", forge.display()),
        project,
    ]);
    common::git(&home, &forge, &["update-ref", "refs/heads/main", M2]);
    let big = clone.join("big.txt");
    fs::write(&big, "line\n".repeat(1_200_000)).unwrap();
    git(&["checkout", "-q", "-b", "big", M1]);
    git(&["add", "big.txt"]);
    git(&[
        "-c",
        "user.name=a",
        "-c",
        "user.email=a@a",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "big",
    ]);
    let old_head = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-q", "main"]);
    let mut listed = fixture_json("gitlab/merge-request-88-versions.json");
    for version in listed.as_array_mut().unwrap() {
        if version["head_commit_sha"] == V1 {
            version["head_commit_sha"] = json!(old_head);
            version["base_commit_sha"] = json!(M1);
        }
    }
    sandbox.gitlab.set(
        "GET /projects/77/merge_requests/88/versions",
        route(listed.to_string()),
    );
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();
    let list: TaskList = ok(&request(
        &sandbox.socket(),
        "GET",
        "/v1/tasks?context=company-a",
        None,
    ));
    let mirror = sandbox
        .store("company-a")
        .join("tasks")
        .join(list.tasks[0].id.to_string())
        .join("mrs/gitlab-company-a%2Fcheckout%2188");
    let versions = json_file(&mirror.join("versions.json"));
    let diff_of = |head: &str| {
        versions
            .as_array()
            .unwrap()
            .iter()
            .find(|version| version["head_sha"] == head)
            .unwrap()["diff"]
            .clone()
    };
    assert_eq!(diff_of(V2), "local");
    assert_eq!(diff_of(&old_head), "reduced_too_large");
    let note = fs::read(mirror.join("diffs").join(format!("{old_head}.patch"))).unwrap();
    assert!(note.starts_with(b"# loom: the local diff is over the limit"));
    assert_eq!(loom_forge::diff::file_hashes(&note), []);
    daemon.stop();
}

#[test]
fn a_merged_mr_keeps_its_local_diff_when_its_base_moves() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (_home, _forge, _clone) = clone_of_the_scenario(&sandbox);
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412/files",
        route(fixture("github/pull-412-files.v2.json")),
    );
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(the_review_id(&sandbox))
        .join(MIRROR);
    let before = fs::read(mirror.join("diffs").join(format!("{V1}.patch"))).unwrap();
    // Refs that go after the merge do not come back: no fetch for a merged MR.
    let (home, clone) = (sandbox.user_home(), sandbox.dir.path().join("work/shop"));
    let head = "refs/loom/mr/upstream/412/head";
    let mut merged = fixture_json("github/pull-412.v1.json");
    merged["state"] = json!("closed");
    merged["merged"] = json!(true);
    merged["merged_at"] = json!("2026-10-22T09:00:00Z");
    merged["base"]["sha"] = json!(V1);
    merged["updated_at"] = json!("2026-10-22T09:00:00Z");
    sandbox.github.set(
        "GET /repos/acme/shop/pulls/412",
        with_etag(merged.to_string(), "\"merged\""),
    );
    sandbox.sync_and_settle();
    let versions = json_file(&mirror.join("versions.json"));
    assert_eq!(versions[0]["diff"], "local");
    assert_eq!(versions[0]["merge_base"], M1);
    assert_eq!(
        fs::read(mirror.join("diffs").join(format!("{V1}.patch"))).unwrap(),
        before
    );
    common::git(&home, &clone, &["update-ref", "-d", head]);
    let fetches = sandbox.git_commands("fetch --no-tags --no-write-fetch-head");
    sandbox.sync_and_settle();
    assert_eq!(
        sandbox.git_commands("fetch --no-tags --no-write-fetch-head"),
        fetches
    );
    daemon.stop();
}

#[test]
fn a_host_that_cannot_be_reached_is_tried_once_for_all_its_clones() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, _clone) = clone_of_the_scenario(&sandbox);
    clone_of_the_private_project(&sandbox, &home, &forge);
    private_mr(&sandbox, &[]);
    fs::write(
        format!("{}.all", sandbox.git_fail().display()),
        "fatal: unable to access 'https://github.com/': Could not resolve host: github.com\n",
    )
    .unwrap();
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_settle();
    assert_eq!(
        sandbox.git_commands("fetch --no-tags --no-write-fetch-head"),
        1
    );
    daemon.stop();
}

#[test]
fn a_success_ends_the_backoff_of_an_mr() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, _clone) = clone_of_the_scenario(&sandbox);
    let private = clone_of_the_private_project(&sandbox, &home, &forge);
    common::git(&home, &forge, &["update-ref", "refs/pull/500/head", V1]);
    private_mr(&sandbox, &[]);
    let refused = "fatal: The requested URL returned error: 403\n";
    fs::write(sandbox.git_fail(), refused).unwrap();
    let daemon = sandbox.start();
    scan(&sandbox);
    // Tries at polls 1 and 2; poll 3 waits; a success at poll 4.
    for _ in 0..3 {
        sandbox.sync_and_settle();
    }
    assert_eq!(fetches_of_500(&sandbox), 2);
    fs::remove_file(sandbox.git_fail()).unwrap();
    sandbox.sync_and_settle();
    let head = "refs/loom/mr/upstream/500/head";
    assert_eq!(
        common::git(&home, &private, &["rev-parse", head]).trim(),
        V1
    );
    let mirror = &mirrors(&sandbox)[&500];
    let versions = json_file(&mirror.join("versions.json"));
    assert_eq!(versions[0]["diff"], "local");

    // A new failure starts from the first step: a try at each of the next two polls.
    let before = fetches_of_500(&sandbox);
    common::git(&home, &private, &["update-ref", "-d", head]);
    fs::write(sandbox.git_fail(), refused).unwrap();
    for _ in 0..2 {
        sandbox.sync_and_settle();
    }
    assert_eq!(fetches_of_500(&sandbox), before + 2);
    daemon.stop();
}
