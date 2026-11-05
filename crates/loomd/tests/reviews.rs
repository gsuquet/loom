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
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::SystemTime;

use loom_api::{
    Actor, Event, QueueReason, ReviewList, ReviewListFailure, ReviewListFailureKind, ReviewQueue,
    ReviewSeenResult, ReviewStartResult, ReviewSync, ReviewSyncOutcome, ReviewSyncResult, Status,
    TaskDetail, TaskEvents, TaskList,
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
/// adds `+after` (an `updated_after` parameter), `+page=<n>`, `+scope=<scope>` (not for
/// `reviews_for_me`) or `+action=<action>`. A route `<key>@<token>` answers
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
        // The other GitLab lists: `reviews_for_me` keeps the plain route.
        if let Some(scope) = param.strip_prefix("scope=")
            && scope != "reviews_for_me"
        {
            let _ = write!(key, "+scope={scope}");
        }
        if let Some(action) = param.strip_prefix("action=") {
            let _ = write!(key, "+action={action}");
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

const EMPTY_LIST: &str = "[]";
const ASSIGNED: &str = "GET /merge_requests+scope=assigned_to_me";
const OWN: &str = "GET /merge_requests+scope=created_by_me";
const MENTIONED: &str = "GET /todos+action=mentioned";
const ADDRESSED: &str = "GET /todos+action=directly_addressed";

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
        (ASSIGNED.to_owned(), route(EMPTY_LIST)),
        (OWN.to_owned(), route(EMPTY_LIST)),
        (
            MENTIONED.to_owned(),
            route(fixture("gitlab/todos.empty.json")),
        ),
        (
            ADDRESSED.to_owned(),
            route(fixture("gitlab/todos.empty.json")),
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
        self.start_on(None)
    }

    /// Starts loomd, archiving as on `today` (`LOOM_TEST_TODAY`) when given.
    fn start_on(&self, today: Option<&str>) -> Daemon {
        self.start_with(today, "LOOM_TEST_FULL_LIST_PERIOD", "1h")
    }

    /// Starts loomd, with this time between two full GitLab lists.
    fn start_with_full_list_period(&self, period: &str) -> Daemon {
        self.start_with(None, "LOOM_TEST_FULL_LIST_PERIOD", period)
    }

    /// Starts loomd, with this test hook.
    fn start_with_env(&self, name: &str, value: &str) -> Daemon {
        self.start_with(None, name, value)
    }

    fn start_with(&self, today: Option<&str>, name: &str, value: &str) -> Daemon {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
        command.env(name, value);
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

    /// The passes of MR refs asked for by this trigger: `poll` or `start`.
    fn passes_of(&self, trigger: &str) -> usize {
        fs::read_to_string(self.pass_file())
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == trigger)
            .count()
    }

    /// A sync, then the end of the pass of MR refs of each poll that it ran.
    fn sync_and_settle(&self) -> ReviewSyncResult {
        let before = self.passes_of("poll");
        let result = self.sync();
        let polls = result
            .forges
            .iter()
            .filter(|forge| matches!(forge.outcome, ReviewSyncOutcome::Synced { .. }))
            .count();
        wait_for("the pass of each poll", || {
            self.passes_of("poll") >= before + polls
        });
        result
    }

    /// No start asked for a pass of MR refs since `before` starts: a poll queues its pass behind
    /// the pass of a start of the same context and host, so it is written by then.
    fn assert_no_start_pass_since(&self, before: usize) {
        self.sync_and_settle();
        assert_eq!(self.passes_of("start"), before);
    }

    /// A sync, then a start on each entry, each with the end of its pass of MR refs.
    fn sync_and_start(&self) -> ReviewSyncResult {
        let result = self.sync_and_settle();
        for entry in queue(&self.socket(), None).entries {
            let encoded = entry
                .key
                .replace('/', "%2F")
                .replace('#', "%23")
                .replace('!', "%21");
            let before = self.passes_of("start");
            start(&self.socket(), &encoded);
            wait_for("the pass of the start", || self.passes_of("start") > before);
        }
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
            ..
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
    // No `own_` reason here: only that a request of myself is none.
    set_notes(&sandbox, &[]);
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
    // Its version has a patch, in the archived task.
    let archived_versions = store
        .join("archive/2026")
        .join(&id)
        .join(MIRROR)
        .join("versions.json");
    let mut versions = json_file(&archived_versions);
    versions[0]["diff"] = json!("local");
    fs::write(&archived_versions, versions.to_string()).unwrap();

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
    // Its patch stays in the archive: the new mirror does not claim it.
    assert!(versions[0].get("diff").is_none(), "{versions}");
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
    sandbox.sync_and_settle();
    sandbox.github.routes.lock().unwrap().remove(PULL);
    sandbox.gitlab.routes.lock().unwrap().remove(MR_88);
    let passes = sandbox.passes_of("start");
    for (context, encoded, key, _, _) in FORGES {
        let refused = start_response(&socket, encoded);
        assert_eq!(refused.status, 404, "{key}: {}", refused.body);
        assert!(refused.body.contains("\"not_found\""), "{}", refused.body);
        let store = sandbox.store(context);
        assert_eq!(entry_files(&store), Vec::<(String, String)>::new(), "{key}");
        assert_eq!(task_dirs(&store), Vec::<PathBuf>::new(), "{key}");
    }
    // A start that creates nothing runs no pass of MR refs.
    sandbox.assert_no_start_pass_since(passes);
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

/// Each git command of loomd that writes refs writes under `refs/loom/`: no merge, no fetch into
/// another namespace.
fn assert_only_loom_refs_written(sandbox: &Sandbox) {
    let log = fs::read_to_string(sandbox.git_log()).unwrap();
    for line in log.lines() {
        // `<dir>|<arguments>`: the commit of a store is not of a clone.
        let (_, command) = line.split_once('|').unwrap();
        assert!(
            !command.starts_with("merge ") && !command.starts_with("pull"),
            "{line}"
        );
        if command.starts_with("update-ref ") {
            assert!(command.contains(" refs/loom/"), "{line}");
        }
        if command.starts_with("fetch ") {
            assert!(command.contains("--refmap="), "{line}");
            let refspecs = command.rsplit("-- ").next().unwrap().split(' ').skip(1);
            for refspec in refspecs {
                let target = refspec.split(':').nth(1).unwrap_or("");
                assert!(target.starts_with("refs/loom/"), "{line}");
            }
        }
    }
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
    // No automatic update: with `ff`, its first update is due at once, and it fast-forwards `main`
    // if it runs after the forge moved below. That is correct, and this test is of the MR refs.
    let config = fs::read_to_string(sandbox.loom_home().join("config.toml"))
        .unwrap()
        .replace(
            "[contexts.personal]\n",
            "[contexts.personal]\nupdate = \"off\"\n",
        );
    fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
    let (home, forge, clone) = clone_of_the_scenario(&sandbox);
    let daemon = sandbox.start();
    scan(&sandbox);
    let git = |args: &[&str]| common::git(&home, &clone, args).trim().to_owned();

    // 1. V1: the head and the base are fetched, the patch is kept, and the clone is recorded.
    sandbox.sync_and_start();
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
    // And no command could have moved anything else.
    assert_only_loom_refs_written(&sandbox);
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
fn an_entry_gets_no_refs_and_a_start_fetches_them_without_a_poll() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, _forge, clone) = clone_of_the_scenario(&sandbox);
    let daemon = sandbox.start();
    scan(&sandbox);

    // An entry: a pass runs after the poll, with no Review task to fetch for.
    sandbox.sync_and_settle();
    assert_eq!(
        sandbox.git_commands("fetch --no-tags --no-write-fetch-head"),
        0
    );
    assert_eq!(queue(&sandbox.socket(), Some("personal")).entries.len(), 1);

    // The start runs a pass, with no poll.
    let polls = sandbox
        .github
        .requests()
        .iter()
        .filter(|line| *line == "GET /search/issues")
        .count();
    let before = sandbox.passes();
    let started = start(&sandbox.socket(), GITHUB_KEY);
    wait_for("the pass of the start", || sandbox.passes() > before);
    assert_eq!(
        sandbox
            .github
            .requests()
            .iter()
            .filter(|line| *line == "GET /search/issues")
            .count(),
        polls
    );
    assert_eq!(
        sandbox.git_commands("fetch --no-tags --no-write-fetch-head"),
        1
    );
    let refs = common::git(&home, &clone, &["for-each-ref", "refs/loom/mr"]);
    assert!(refs.contains("/412/head"), "{refs}");
    let mirror = sandbox
        .store("personal")
        .join("tasks")
        .join(started.task.summary.id.to_string())
        .join(MIRROR);
    let versions = json_file(&mirror.join("versions.json"));
    assert_eq!(versions[0]["diff"], "local", "{versions}");
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
    let result = sandbox.sync_and_start();
    assert_eq!(
        counts(poll(&result, "personal", "github.com")),
        (1, 1, 0, 0, 0, 0)
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
    sandbox.sync_and_start();

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
    sandbox.sync_and_start();

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
    sandbox.sync_and_start();
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
    sandbox.sync_and_start();
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
    sandbox.sync_and_start();
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
    sandbox.sync_and_start();
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
    sandbox.sync_and_start();
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
    sandbox.sync_and_start();
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
    sandbox.sync_and_start();
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
    sandbox.sync_and_start();
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
    sandbox.sync_and_start();
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
fn the_cleanup_leaves_the_refs_of_a_remote_that_now_names_another_project() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, _forge, clone) = clone_of_the_scenario(&sandbox);
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_start();
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
    sandbox.sync_and_start();
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
    sandbox.sync_and_start();
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
    sandbox.sync_and_start();
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

/// A new head on the GitHub MR `project#number`: it needs a patch again, as a first head does.
fn new_head_on(sandbox: &Sandbox, project: &str, number: u64) {
    let path = format!("GET /repos/{project}/pulls/{number}");
    let mut pull: Value = serde_json::from_str(&sandbox.github.body(&path)).unwrap();
    pull["head"]["sha"] = json!(V2);
    pull["updated_at"] = json!("2026-10-30T09:00:00Z");
    sandbox.github.set(
        &path,
        with_etag(pull.to_string(), &format!("\"new{number}\"")),
    );
}

#[test]
fn a_host_that_cannot_be_reached_is_tried_once_for_all_its_clones() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, _clone) = clone_of_the_scenario(&sandbox);
    clone_of_the_private_project(&sandbox, &home, &forge);
    common::git(&home, &forge, &["update-ref", "refs/pull/500/head", V1]);
    private_mr(&sandbox, &[]);
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_start();
    let before = sandbox.git_commands("fetch --no-tags --no-write-fetch-head");
    fs::write(
        format!("{}.all", sandbox.git_fail().display()),
        "fatal: unable to access 'https://github.com/': Could not resolve host: github.com\n",
    )
    .unwrap();
    new_head_on(&sandbox, "acme/shop", 412);
    new_head_on(&sandbox, "acme/private", 500);
    sandbox.sync_and_settle();
    assert_eq!(
        sandbox.git_commands("fetch --no-tags --no-write-fetch-head"),
        before + 1
    );
    daemon.stop();
}

#[test]
fn a_remote_that_is_gone_is_tried_once_for_all_the_mrs_of_its_clone() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, _clone) = clone_of_the_scenario(&sandbox);
    common::git(&home, &forge, &["update-ref", "refs/pull/413/head", V1]);
    common::git(&home, &forge, &["update-ref", "refs/pull/414/head", V1]);
    more_mrs(&sandbox, &[(413, "main"), (414, "main")]);
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_start();
    let before = sandbox.git_commands("fetch --no-tags --no-write-fetch-head");
    fs::rename(&forge, forge.with_extension("moved")).unwrap();
    for number in [412, 413, 414] {
        new_head_on(&sandbox, "acme/shop", number);
    }
    sandbox.sync_and_settle();
    assert_eq!(
        sandbox.git_commands("fetch --no-tags --no-write-fetch-head"),
        before + 1
    );
    daemon.stop();
}

#[test]
fn a_forge_that_refuses_the_token_is_read_once_for_all_the_mrs_without_a_clone() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    fs::create_dir_all(sandbox.dir.path().join("work")).unwrap();
    more_mrs(&sandbox, &[(413, "main"), (414, "main")]);
    let daemon = sandbox.start();
    sandbox.sync_and_start();
    let reads = |sandbox: &Sandbox| {
        sandbox
            .github
            .requests()
            .iter()
            .filter(|request| request.ends_with("/files"))
            .count()
    };
    let before = reads(&sandbox);
    for number in [412, 413, 414] {
        sandbox.github.set(
            &format!("GET /repos/acme/shop/pulls/{number}/files"),
            Route {
                status: 401,
                ..route(r#"{"message":"Bad credentials"}"#)
            },
        );
        new_head_on(&sandbox, "acme/shop", number);
    }
    sandbox.sync_and_settle();
    assert_eq!(reads(&sandbox), before + 1);
    daemon.stop();
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
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_start();
    let at_the_starts = fetches_of_500(&sandbox);
    for number in [412, 413, 414, 500] {
        common::git(
            &home,
            &forge,
            &["update-ref", &format!("refs/pull/{number}/head"), V2],
        );
    }
    fs::write(
        sandbox.git_fail(),
        "fatal: unable to access 'https://github.com/acme/private.git/': The requested URL returned error: 403\n",
    )
    .unwrap();
    for number in [412, 413, 414] {
        new_head_on(&sandbox, "acme/shop", number);
    }
    new_head_on(&sandbox, "acme/private", 500);
    sandbox.sync_and_settle();

    // Whatever the order of the tasks: the same endpoint serves the others in the same poll.
    let mirrors = mirrors(&sandbox);
    assert_eq!(mirrors.len(), 4);
    for number in [412, 413, 414] {
        assert_eq!(
            json_file(&mirrors[&number].join("versions.json"))[1]["diff"],
            "local",
            "{number}"
        );
    }
    // 500 has the patches of the forge, and backs off.
    assert_eq!(
        json_file(&mirrors[&500].join("versions.json"))[1]["diff"],
        "reduced"
    );
    for _ in 0..5 {
        sandbox.sync_and_settle();
    }
    // Polls 1, 2 and 4 of 6.
    assert_eq!(fetches_of_500(&sandbox) - at_the_starts, 3);
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
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_start();
    let at_the_starts = fetches_of_500(&sandbox);
    common::git(&home, &forge, &["update-ref", "refs/pull/500/head", V2]);
    common::git(&home, &forge, &["update-ref", "refs/pull/412/head", V2]);
    fs::write(
        sandbox.git_fail(),
        "fatal: unable to access 'https://github.com/': Could not resolve host: github.com\n",
    )
    .unwrap();
    new_head_on(&sandbox, "acme/shop", 412);
    new_head_on(&sandbox, "acme/private", 500);
    // Poll 1 and 2 may try 500 first, and stop 412 behind it. At poll 3, 500 waits.
    for _ in 0..3 {
        sandbox.sync_and_settle();
    }
    let mirrors = mirrors(&sandbox);
    assert_eq!(
        json_file(&mirrors[&412].join("versions.json"))[1]["diff"],
        "local"
    );
    assert_eq!(
        json_file(&mirrors[&500].join("versions.json"))[1]["diff"],
        "reduced"
    );
    assert!(fetches_of_500(&sandbox) - at_the_starts <= 2);
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
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_start();
    let at_the_starts = fetches_of_500(&sandbox);
    common::git(&home, &forge, &["update-ref", "refs/pull/500/head", V2]);
    let refused = "fatal: The requested URL returned error: 403\n";
    fs::write(sandbox.git_fail(), refused).unwrap();
    new_head_on(&sandbox, "acme/private", 500);
    // Tries at polls 1 and 2; poll 3 waits; a success at poll 4.
    for _ in 0..3 {
        sandbox.sync_and_settle();
    }
    assert_eq!(fetches_of_500(&sandbox) - at_the_starts, 2);
    fs::remove_file(sandbox.git_fail()).unwrap();
    sandbox.sync_and_settle();
    let head = "refs/loom/mr/upstream/500/head";
    assert_eq!(
        common::git(&home, &private, &["rev-parse", head]).trim(),
        V2
    );
    let mirror = &mirrors(&sandbox)[&500];
    let versions = json_file(&mirror.join("versions.json"));
    assert_eq!(versions[1]["diff"], "local");

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

/// The start with the host down, as a user sees it: the patches of the forge, then the local diff.
#[test]
fn a_start_with_failing_git_gets_the_patches_of_the_forge_then_recovers() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, _clone) = clone_of_the_scenario(&sandbox);
    clone_of_the_private_project(&sandbox, &home, &forge);
    common::git(&home, &forge, &["update-ref", "refs/pull/500/head", V1]);
    private_mr(&sandbox, &[]);
    fs::write(
        sandbox.git_fail(),
        "fatal: The requested URL returned error: 403\n",
    )
    .unwrap();
    let daemon = sandbox.start();
    scan(&sandbox);
    sandbox.sync_and_start();
    let started = mirrors(&sandbox);
    assert_eq!(
        json_file(&started[&412].join("versions.json"))[0]["diff"],
        "local"
    );
    assert_eq!(
        json_file(&started[&500].join("versions.json"))[0]["diff"],
        "reduced"
    );
    fs::remove_file(sandbox.git_fail()).unwrap();
    for _ in 0..4 {
        sandbox.sync_and_settle();
    }
    assert_eq!(
        json_file(&mirrors(&sandbox)[&500].join("versions.json"))[0]["diff"],
        "local"
    );
    daemon.stop();
}

#[test]
fn a_start_on_a_kept_approval_dates_the_verdict_to_the_approved_head() {
    const V3: &str = "0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e";
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    // I approve V2 at the forge. The project keeps approvals, and the author pushes V3.
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-23T09:00:00.000Z", "opened", "W/\"approved\""),
    );
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V3, "2026-10-24T09:00:00.000Z", "opened", "W/\"pushed\""),
    );
    gitlab_lists_it(&sandbox);
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);

    let id = start(&socket, GITLAB_KEY).task.summary.id.to_string();
    let sync_file = sandbox
        .store("company-a")
        .join("tasks")
        .join(&id)
        .join(FORGES[1].4)
        .join("sync.json");
    let state = json_file(&sync_file);
    assert_eq!(state["verdict"]["sha"], V2, "{state}");
    assert_eq!(state["shown"], true, "{state}");

    // The approval still shows at the next read: it stays on V2.
    let mut mr = fixture_json("gitlab/merge-request-88.v2.json");
    mr["sha"] = json!(V3);
    mr["diff_refs"]["head_sha"] = json!(V3);
    mr["updated_at"] = json!("2026-10-25T09:00:00.000Z");
    sandbox
        .gitlab
        .set(MR_88, with_etag(mr.to_string(), "W/\"changed\""));
    sandbox.sync();
    assert_eq!(json_file(&sync_file)["verdict"]["sha"], V2);
    assert_eq!(status(&socket, &id), Status::Active);

    // My request goes: V3 is still to review, so the task stays open.
    mr["reviewers"] = json!([]);
    mr["updated_at"] = json!("2026-10-26T09:00:00.000Z");
    sandbox
        .gitlab
        .set(MR_88, with_etag(mr.to_string(), "W/\"norequest\""));
    sandbox.sync();
    assert_eq!(status(&socket, &id), Status::Active);
    daemon.stop();
}

#[test]
fn a_start_on_a_kept_approval_leaves_the_reviewed_ref_on_the_approved_head() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, _) = clone_of_the_scenario(&sandbox);
    common::git(
        &home,
        &forge,
        &["update-ref", "refs/merge-requests/88/head", V1],
    );
    // A clone of the GitLab project, which has both commits.
    let clone = sandbox.dir.path().join("work/checkout");
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
    let project = "git@gitlab.example.com:company-a/checkout.git";
    common::git(&home, &clone, &["remote", "set-url", "upstream", project]);
    let rewrite = format!("url.{}.insteadOf", forge.display());
    common::git(&home, &clone, &["config", &rewrite, project]);
    common::git(
        &home,
        &clone,
        &["fetch", "-q", "upstream", "refs/merge-requests/88/head"],
    );
    common::git(&home, &clone, &["update-ref", "refs/keep/v1", V1]);

    // I approve V1 at the forge, and the entry goes.
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V1, "2026-10-23T09:00:00.000Z", "opened", "W/\"approved\""),
    );
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    scan(&sandbox);
    sandbox.sync_and_settle();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);

    // The author pushes V2, and the approval stays.
    common::git(
        &home,
        &forge,
        &["update-ref", "refs/merge-requests/88/head", V2],
    );
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-24T09:00:00.000Z", "opened", "W/\"pushed\""),
    );
    gitlab_lists_it(&sandbox);
    sandbox.sync_and_settle();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);

    let before = sandbox.passes();
    start(&socket, GITLAB_KEY);
    wait_for("the pass of the start", || sandbox.passes() > before);
    let git = |args: &[&str]| common::git(&home, &clone, args).trim().to_owned();
    assert_eq!(git(&["rev-parse", "refs/loom/mr/upstream/88/head"]), V2);
    assert_eq!(git(&["rev-parse", "refs/loom/mr/upstream/88/reviewed"]), V1);
    daemon.stop();
}

#[test]
fn a_gitlab_404_on_an_entry_lists_in_full_so_the_entry_comes_back_with_the_access() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    let body = sandbox.gitlab.body(MR_88);
    // The access goes for one poll: GitLab answers 404, and the list does not show the MR.
    sandbox.gitlab.set(
        MR_88,
        Route {
            status: 404,
            ..route(r#"{"message":"404 Project Not Found"}"#)
        },
    );
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    // The access is back, and the MR did not change: `updated_after` would not list it.
    sandbox.gitlab.set(MR_88, route(body));
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    // That full list was one: the next one asks for the changes only.
    let seen = sandbox.gitlab.requests().len();
    sandbox.sync();
    let requests = sandbox.gitlab.requests();
    assert!(
        !requests[seen..].contains(&"GET /merge_requests".to_owned()),
        "{:?}",
        &requests[seen..]
    );
    daemon.stop();
}

#[test]
fn a_gitlab_404_at_a_start_lists_in_full_at_the_next_poll() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let body = sandbox.gitlab.body(MR_88);
    sandbox.gitlab.routes.lock().unwrap().remove(MR_88);
    assert_eq!(start_response(&socket, GITLAB_KEY).status, 404);
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    sandbox.gitlab.set(MR_88, route(body));
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    daemon.stop();
}

#[test]
fn a_404_on_a_sub_read_keeps_the_entry_and_is_a_failure() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let before = (
        entry_files(&sandbox.store("personal")),
        entry_files(&sandbox.store("company-a")),
    );
    // The MRs exist and changed; only their reviews and approvals answer 404, and no list shows
    // them this time.
    let not_found = || Route {
        status: 404,
        ..route(r#"{"message":"Not Found"}"#)
    };
    sandbox.github.set(
        "GET /search/issues",
        route(fixture("github/search-review-requested.empty.json")),
    );
    let mut pull = fixture_json("github/pull-412.v1.json");
    pull["updated_at"] = json!("2026-10-30T09:00:00Z");
    sandbox
        .github
        .set(PULL, with_etag(pull.to_string(), "\"changed\""));
    sandbox
        .github
        .set("GET /repos/acme/shop/pulls/412/reviews", not_found());
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-30T09:00:00.000Z", "opened", "W/\"changed\""),
    );
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), not_found());
    let result = sandbox.sync();
    for (context, host) in [
        ("personal", "github.com"),
        ("company-a", "gitlab.example.com"),
    ] {
        let (mrs, _, removed, _, _, failed) = counts(poll(&result, context, host));
        assert_eq!((mrs, removed, failed), (1, 0, 1), "{result:?}");
    }
    assert_eq!(
        (
            entry_files(&sandbox.store("personal")),
            entry_files(&sandbox.store("company-a"))
        ),
        before
    );
    // A start on such an MR is a failure too: the entry stays, for the next try.
    for (context, encoded, key, _, _) in FORGES {
        let refused = start_response(&socket, encoded);
        assert_eq!(refused.status, 502, "{key}: {}", refused.body);
        assert_eq!(entry_files(&sandbox.store(context)).len(), 1, "{key}");
        assert_eq!(task_dirs(&sandbox.store(context)).len(), 0, "{key}");
    }
    daemon.stop();
}

#[test]
fn a_404_on_a_listed_mr_stays_a_failure() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    let before = (
        entry_files(&sandbox.store("personal")),
        entry_files(&sandbox.store("company-a")),
    );
    // The lists still show them: a search can lag behind a deletion.
    gitlab_lists_it(&sandbox);
    sandbox.github.routes.lock().unwrap().remove(PULL);
    sandbox.gitlab.routes.lock().unwrap().remove(MR_88);
    let result = sandbox.sync();
    for (context, host) in [
        ("personal", "github.com"),
        ("company-a", "gitlab.example.com"),
    ] {
        assert_eq!(counts(poll(&result, context, host)).5, 1, "{result:?}");
    }
    assert_eq!(
        (
            entry_files(&sandbox.store("personal")),
            entry_files(&sandbox.store("company-a"))
        ),
        before
    );
    daemon.stop();
}

#[test]
fn a_start_after_my_direct_request_is_gone_is_refused_and_removes_the_entry() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync_and_settle();
    github_without_request(&sandbox);
    let mut mr = fixture_json("gitlab/merge-request-88.v2.json");
    mr["reviewers"] = json!([]);
    sandbox
        .gitlab
        .set(MR_88, with_etag(mr.to_string(), "W/\"no-request\""));
    let passes = sandbox.passes_of("start");
    for (context, encoded, key, _, _) in FORGES {
        let refused = start_response(&socket, encoded);
        assert_eq!(refused.status, 409, "{key}: {}", refused.body);
        assert!(refused.body.contains("\"conflict\""), "{}", refused.body);
        let store = sandbox.store(context);
        assert_eq!(task_dirs(&store), Vec::<PathBuf>::new(), "{key}");
        assert_eq!(entry_files(&store), Vec::<(String, String)>::new(), "{key}");
    }
    // A start that creates nothing runs no pass of MR refs.
    sandbox.assert_no_start_pass_since(passes);
    daemon.stop();
}

/// A known limit: a read does not show a request through a team, so the start creates the task.
#[test]
fn a_start_after_a_team_request_is_gone_creates_the_task_and_the_next_poll_cancels_it() {
    let sandbox = Sandbox::new();
    // A request through a team: I am not in the reviewers, and the search lists me.
    let mut pull = fixture_json("github/pull-412.v1.json");
    pull["requested_reviewers"] = json!([]);
    sandbox
        .github
        .set(PULL, with_etag(pull.to_string(), "\"team\""));
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    sandbox.github.set(
        "GET /search/issues",
        route(fixture("github/search-review-requested.empty.json")),
    );
    let started = start(&socket, GITHUB_KEY);
    assert!(started.created);
    let id = started.task.summary.id.to_string();
    sandbox.sync();
    assert_eq!(status(&socket, &id), Status::Cancelled);
    daemon.stop();
}

#[test]
fn a_held_entry_that_a_list_shows_does_not_hold_back_updated_after() {
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
    sandbox.gitlab.set(
        &format!("GET /merge_requests+after@{CLIENT_TOKEN}"),
        route(fixture("gitlab/merge-requests-reviews-for-me.empty.json")),
    );
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    register_the_clone(&sandbox);
    // A restart: no `updated_after`, so the full list of `company-a` shows the held MR.
    daemon.stop();
    let daemon = sandbox.start();
    let result = sandbox.sync();
    let ReviewSyncOutcome::Synced { failed, .. } =
        &poll(&result, "company-a", "gitlab.example.com").outcome
    else {
        panic!("{result:?}");
    };
    assert_eq!(failed.len(), 1, "{result:?}");
    sandbox.sync();
    // Both contexts now ask for the changes only.
    let seen = sandbox.gitlab.requests().len();
    sandbox.sync();
    let requests = sandbox.gitlab.requests();
    let lists = requests[seen..]
        .iter()
        .filter(|line| *line == "GET /merge_requests+after")
        .count();
    assert_eq!(lists, 2, "{:?}", &requests[seen..]);
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    daemon.stop();
}

const NOT_FOUND_PROJECT: &str = r#"{"message":"404 Project Not Found"}"#;

/// The full list of `company-a` lists the MR; a list limited by `updated_after` does not.
fn gitlab_lists_it_in_full_only(sandbox: &Sandbox) {
    sandbox.gitlab.set(
        "GET /merge_requests",
        route(fixture("gitlab/merge-requests-reviews-for-me.json")),
    );
    sandbox.gitlab.set(
        "GET /merge_requests+after",
        route(fixture("gitlab/merge-requests-reviews-for-me.empty.json")),
    );
}

/// The lists of GitLab, full or limited, do not show the MR, and its read answers 404.
fn gitlab_access_lost(sandbox: &Sandbox) {
    sandbox.gitlab.set(
        MR_88,
        Route {
            status: 404,
            ..route(NOT_FOUND_PROJECT)
        },
    );
    sandbox.gitlab.set(
        "GET /merge_requests",
        route(fixture("gitlab/merge-requests-reviews-for-me.empty.json")),
    );
}

#[test]
fn a_loss_of_access_for_two_polls_comes_back_at_the_next_full_list() {
    let sandbox = Sandbox::new();
    // Each full list is due at once: the hour has passed.
    let daemon = sandbox.start_with_full_list_period("0s");
    let socket = sandbox.socket();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    let body = sandbox.gitlab.body(MR_88);
    gitlab_access_lost(&sandbox);
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    sandbox.sync();
    // The access is back, and the MR did not change.
    sandbox.gitlab.set(MR_88, route(body));
    gitlab_lists_it_in_full_only(&sandbox);
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    daemon.stop();
}

#[test]
fn inside_the_period_a_longer_loss_of_access_waits_for_the_next_full_list() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let body = sandbox.gitlab.body(MR_88);
    gitlab_access_lost(&sandbox);
    sandbox.sync();
    // Still lost: this is the full list that the 404 asked for, and it counts.
    sandbox.sync();
    sandbox.gitlab.set(MR_88, route(body));
    gitlab_lists_it_in_full_only(&sandbox);
    // The bound of the design: not before the hour, and each list is limited until then.
    let seen = sandbox.gitlab.requests().len();
    sandbox.sync();
    let requests = sandbox.gitlab.requests();
    assert!(
        !requests[seen..].contains(&"GET /merge_requests".to_owned()),
        "{:?}",
        &requests[seen..]
    );
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    daemon.stop();
}

#[test]
fn a_full_list_whose_read_fails_does_not_count_as_a_full_list() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let body = sandbox.gitlab.body(MR_88);
    sandbox.gitlab.set(
        MR_88,
        Route {
            status: 404,
            ..route(NOT_FOUND_PROJECT)
        },
    );
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    // The full list shows the MR, and its read fails.
    sandbox.gitlab.set(
        MR_88,
        Route {
            status: 500,
            ..route(r#"{"message":"boom"}"#)
        },
    );
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")).5,
        1
    );
    // The next list is full again, and the read works.
    sandbox.gitlab.set(MR_88, route(body));
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    daemon.stop();
}

#[test]
fn a_poll_that_stops_does_not_count_as_a_full_list() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let body = sandbox.gitlab.body(MR_88);
    sandbox.gitlab.set(
        MR_88,
        Route {
            status: 404,
            ..route(NOT_FOUND_PROJECT)
        },
    );
    sandbox.sync();
    // The full list shows the MR, and the token is refused at its read: the poll stops.
    sandbox.gitlab.set(
        MR_88,
        Route {
            status: 401,
            ..route(r#"{"message":"401 Unauthorized"}"#)
        },
    );
    let result = sandbox.sync();
    assert!(
        matches!(
            poll(&result, "company-a", "gitlab.example.com").outcome,
            ReviewSyncOutcome::Failed { .. }
        ),
        "{result:?}"
    );
    sandbox.gitlab.set(MR_88, route(body));
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    daemon.stop();
}

#[test]
fn a_full_list_comes_again_after_the_period_and_not_inside_it() {
    // Inside the period: the first list is full, and the next ones are limited.
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    let seen = sandbox.gitlab.requests().len();
    sandbox.sync();
    let requests = sandbox.gitlab.requests();
    assert!(
        !requests[seen..].contains(&"GET /merge_requests".to_owned()),
        "{:?}",
        &requests[seen..]
    );
    daemon.stop();

    // After the period: a full list, also with no 404 and no change of the MR.
    let sandbox = Sandbox::new();
    let daemon = sandbox.start_with_full_list_period("0s");
    sandbox.sync();
    let seen = sandbox.gitlab.requests().len();
    sandbox.sync();
    let requests = sandbox.gitlab.requests();
    assert!(
        requests[seen..].contains(&"GET /merge_requests".to_owned()),
        "{:?}",
        &requests[seen..]
    );
    assert!(
        !requests[seen..].contains(&"GET /merge_requests+after".to_owned()),
        "{:?}",
        &requests[seen..]
    );
    daemon.stop();
}

#[test]
fn a_reviewed_ref_after_a_rebase_with_no_earlier_fetch_is_absent() {
    let sandbox = Sandbox::new();
    index_the_clones(&sandbox);
    let (home, forge, _) = clone_of_the_scenario(&sandbox);
    common::git(
        &home,
        &forge,
        &["update-ref", "refs/merge-requests/88/head", V1],
    );
    // A clone that does not have the approved commit, as no fetch came before the rebase.
    let clone = sandbox.dir.path().join("work/checkout");
    common::git(
        &home,
        sandbox.dir.path(),
        &[
            "clone",
            "-q",
            "--no-local",
            "-o",
            "upstream",
            forge.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let project = "git@gitlab.example.com:company-a/checkout.git";
    common::git(&home, &clone, &["remote", "set-url", "upstream", project]);
    let rewrite = format!("url.{}.insteadOf", forge.display());
    common::git(&home, &clone, &["config", &rewrite, project]);
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V1, "2026-10-23T09:00:00.000Z", "opened", "W/\"approved\""),
    );
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    scan(&sandbox);
    sandbox.sync_and_settle();
    common::git(
        &home,
        &forge,
        &["update-ref", "refs/merge-requests/88/head", V2],
    );
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-24T09:00:00.000Z", "opened", "W/\"pushed\""),
    );
    gitlab_lists_it(&sandbox);
    sandbox.sync_and_settle();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);

    let before = sandbox.passes();
    let id = start(&socket, GITLAB_KEY).task.summary.id.to_string();
    wait_for("the pass of the start", || sandbox.passes() > before);
    sandbox.sync_and_settle();
    let refs = common::git(&home, &clone, &["for-each-ref", "refs/loom"]);
    assert!(refs.contains("/88/head"), "{refs}");
    assert!(!refs.contains("/88/reviewed"), "{refs}");
    // The verdict is not lost: `sync.json` has it.
    let state = json_file(
        &sandbox
            .store("company-a")
            .join("tasks")
            .join(&id)
            .join(FORGES[1].4)
            .join("sync.json"),
    );
    assert_eq!(state["verdict"]["sha"], V1, "{state}");
    daemon.stop();
}

#[test]
fn a_retry_after_a_failed_start_still_sees_that_my_direct_request_is_gone() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    sandbox.github.set(
        PULL,
        Route {
            status: 500,
            ..route(r#"{"message":"boom"}"#)
        },
    );
    assert_eq!(start_response(&socket, GITHUB_KEY).status, 502);
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);
    let mut pull = fixture_json("github/pull-412.v1.json");
    pull["requested_reviewers"] = json!([]);
    sandbox
        .github
        .set(PULL, with_etag(pull.to_string(), "\"gone\""));
    let retried = start_response(&socket, GITHUB_KEY);
    assert_eq!(retried.status, 409, "{}", retried.body);
    assert_eq!(task_dirs(&sandbox.store("personal")).len(), 0);
    daemon.stop();
}

#[test]
fn a_failed_poll_read_keeps_what_tells_that_a_request_is_direct() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    sandbox.github.set(
        PULL,
        Route {
            status: 500,
            ..route(r#"{"message":"boom"}"#)
        },
    );
    let result = sandbox.sync();
    assert_eq!(counts(poll(&result, "personal", "github.com")).5, 1);
    let mut pull = fixture_json("github/pull-412.v1.json");
    pull["requested_reviewers"] = json!([]);
    sandbox
        .github
        .set(PULL, with_etag(pull.to_string(), "\"gone\""));
    assert_eq!(start_response(&socket, GITHUB_KEY).status, 409);
    daemon.stop();
}

/// A known limit: a read shows only that the direct request is gone.
#[test]
fn a_direct_request_replaced_by_a_team_is_refused_and_heals_at_the_next_poll() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let mut pull = fixture_json("github/pull-412.v1.json");
    pull["requested_reviewers"] = json!([]);
    sandbox
        .github
        .set(PULL, with_etag(pull.to_string(), "\"team\""));
    assert_eq!(start_response(&socket, GITHUB_KEY).status, 409);
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 0);
    sandbox.sync();
    assert_eq!(queue(&socket, Some("personal")).entries.len(), 1);
    daemon.stop();
}

/// The state that a crash or a failed mirror write in a start leaves: an active task with no
/// `sync.json`, and the head of my approval still in the index.
#[test]
fn a_task_with_no_sync_state_dates_a_kept_approval_from_the_index() {
    const V3: &str = "0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e";
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-23T09:00:00.000Z", "opened", "W/\"approved\""),
    );
    sandbox.sync();
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V3, "2026-10-24T09:00:00.000Z", "opened", "W/\"pushed\""),
    );
    gitlab_lists_it(&sandbox);
    sandbox.sync();
    let created: TaskDetail = ok(&request(
        &socket,
        "POST",
        "/v1/tasks",
        Some(&json!({
            "title": "Review company-a/checkout!88",
            "kind": "review",
            "context": "company-a",
            "status": "active",
            "links": [{
                "role": "origin",
                "provider": "gitlab",
                "key": "company-a/checkout!88",
                "url": "https://gitlab.example.com/company-a/checkout/-/merge_requests/88",
            }],
        })),
    ));
    let id = created.summary.id.to_string();
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V3, "2026-10-25T09:00:00.000Z", "opened", "W/\"changed\""),
    );
    sandbox.sync();
    let state = json_file(
        &sandbox
            .store("company-a")
            .join("tasks")
            .join(&id)
            .join(FORGES[1].4)
            .join("sync.json"),
    );
    assert_eq!(state["verdict"]["sha"], V2, "{state}");
    assert_eq!(status(&socket, &id), Status::Active);
    daemon.stop();
}

#[test]
fn a_gitlab_start_after_a_restart_still_sees_that_my_request_is_gone() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync();
    daemon.stop();
    // GitLab has no team requests: a read always tells, with nothing in memory.
    let mut mr = fixture_json("gitlab/merge-request-88.v2.json");
    mr["reviewers"] = json!([]);
    sandbox
        .gitlab
        .set(MR_88, with_etag(mr.to_string(), "W/\"no-request\""));
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    let refused = start_response(&socket, GITLAB_KEY);
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert_eq!(entry_files(&sandbox.store("company-a")).len(), 0);
    daemon.stop();
}

#[test]
fn a_404_at_a_start_whose_entry_cannot_be_removed_says_so() {
    use std::os::unix::fs::PermissionsExt as _;
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    sandbox.github.routes.lock().unwrap().remove(PULL);
    let dir = sandbox.store("personal").join("queue/mrs");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let refused = start_response(&socket, GITHUB_KEY);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(refused.status, 500, "{}", refused.body);
    assert!(refused.body.contains("cannot delete"), "{}", refused.body);
    assert_eq!(entry_files(&sandbox.store("personal")).len(), 1);
    daemon.stop();
}

#[test]
fn a_404_in_a_poll_with_a_full_list_still_asks_for_the_next_full_list() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let body = sandbox.gitlab.body(MR_88);
    daemon.stop();
    // A restart: the first list is full, and the entry answers 404 in that same poll.
    gitlab_access_lost(&sandbox);
    let daemon = sandbox.start();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    // The access is back, and the MR did not change.
    sandbox.gitlab.set(MR_88, route(body));
    gitlab_lists_it_in_full_only(&sandbox);
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    daemon.stop();
}

#[test]
fn a_limited_list_does_not_put_off_the_next_full_list() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start_with_full_list_period("2s");
    sandbox.sync();
    let seen = sandbox.gitlab.requests().len();
    // Polls come faster than the period: if a limited one counted, no full list would come.
    wait_for("a full list after the period", || {
        sandbox.sync();
        sandbox.gitlab.requests()[seen..].contains(&"GET /merge_requests".to_owned())
    });
    daemon.stop();
}

/// Approve V2, and a push to V3 on a project that keeps approvals: an entry on V3.
fn kept_approval_with_an_entry(sandbox: &Sandbox) {
    const V3: &str = "0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e";
    sandbox.sync();
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-23T09:00:00.000Z", "opened", "W/\"approved\""),
    );
    sandbox.sync();
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V3, "2026-10-24T09:00:00.000Z", "opened", "W/\"pushed\""),
    );
    gitlab_lists_it(sandbox);
    sandbox.sync();
    assert_eq!(queue(&sandbox.socket(), Some("company-a")).entries.len(), 1);
}

fn approval_heads(sandbox: &Sandbox) -> Vec<String> {
    loom_store::Reader::open(&sandbox.loom_home().join("index.db"))
        .unwrap()
        .mr_approvals(&"company-a".parse().unwrap(), "gitlab.example.com")
        .unwrap()
        .into_iter()
        .map(|approval| approval.head_sha)
        .collect()
}

#[test]
fn a_kept_approval_entry_returns_after_a_404_and_the_row_survives() {
    const V3: &str = "0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e";
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    kept_approval_with_an_entry(&sandbox);
    gitlab_access_lost(&sandbox);
    sandbox.gitlab.set(
        "GET /merge_requests+after",
        route(fixture("gitlab/merge-requests-reviews-for-me.empty.json")),
    );
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    assert_eq!(approval_heads(&sandbox), [V2]);
    // The access is back, on the same head, with the same approval: still to review.
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V3, "2026-10-24T09:00:00.000Z", "opened", "W/\"pushed\""),
    );
    gitlab_lists_it_in_full_only(&sandbox);
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    assert_eq!(approval_heads(&sandbox), [V2]);
    // A start dates my approval to the head that I reviewed.
    let id = start(&socket, GITLAB_KEY).task.summary.id.to_string();
    let state = json_file(
        &sandbox
            .store("company-a")
            .join("tasks")
            .join(&id)
            .join(FORGES[1].4)
            .join("sync.json"),
    );
    assert_eq!(state["verdict"]["sha"], V2, "{state}");
    daemon.stop();
}

#[test]
fn an_approval_row_stays_through_several_404_and_the_entry_comes_back_without_a_list() {
    const V3: &str = "0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e";
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    kept_approval_with_an_entry(&sandbox);
    gitlab_access_lost(&sandbox);
    sandbox.gitlab.set(
        "GET /merge_requests+after",
        route(fixture("gitlab/merge-requests-reviews-for-me.empty.json")),
    );
    for _ in 0..3 {
        sandbox.sync();
    }
    assert_eq!(approval_heads(&sandbox), [V2]);
    // No list shows the MR: its row keeps it read, so the entry returns with the access.
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V3, "2026-10-24T09:00:00.000Z", "opened", "W/\"pushed\""),
    );
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    daemon.stop();
}

#[test]
fn a_404_on_an_mr_with_only_a_row_does_not_ask_for_a_full_list() {
    let sandbox = Sandbox::new();
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    let daemon = sandbox.start();
    sandbox.sync();
    assert_eq!(approval_heads(&sandbox), [V2]);
    assert_eq!(queue(&sandbox.socket(), Some("company-a")).entries.len(), 0);
    gitlab_access_lost(&sandbox);
    sandbox.sync();
    // Polls with nothing to remove: each list stays limited, and the row stays.
    let seen = sandbox.gitlab.requests().len();
    sandbox.sync();
    sandbox.sync();
    let requests = sandbox.gitlab.requests();
    assert!(
        !requests[seen..].contains(&"GET /merge_requests".to_owned()),
        "{:?}",
        &requests[seen..]
    );
    assert_eq!(approval_heads(&sandbox), [V2]);
    daemon.stop();
}

#[test]
fn a_404_drops_an_approval_row_past_its_age() {
    let sandbox = Sandbox::new();
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    let daemon = sandbox.start_with_env("LOOM_TEST_APPROVAL_EXPIRY", "0s");
    sandbox.sync();
    assert_eq!(approval_heads(&sandbox), [V2]);
    gitlab_access_lost(&sandbox);
    sandbox.sync();
    assert_eq!(approval_heads(&sandbox), Vec::<String>::new());
    daemon.stop();
}

#[test]
fn only_a_404_drops_an_old_approval_row() {
    let sandbox = Sandbox::new();
    // Each row is past its age, and the MR keeps answering.
    let daemon = sandbox.start_with_env("LOOM_TEST_APPROVAL_EXPIRY", "0s");
    kept_approval_with_an_entry(&sandbox);
    sandbox.sync();
    sandbox.sync();
    assert_eq!(approval_heads(&sandbox), [V2]);
    daemon.stop();
}

#[test]
fn the_approval_row_of_an_mr_of_another_context_goes() {
    let sandbox = sandbox_for_a_move("");
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
    sandbox
        .gitlab
        .set(&format!("{MR_88}/approvals"), approvals(true));
    let daemon = sandbox.start();
    sandbox.sync();
    assert_eq!(approval_heads(&sandbox), [V2]);
    register_the_clone(&sandbox);
    sandbox.sync();
    assert_eq!(approval_heads(&sandbox), Vec::<String>::new());
    daemon.stop();
}

#[test]
fn a_merge_that_removes_an_entry_does_not_ask_for_a_full_list() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 1);
    sandbox.gitlab.set(
        MR_88,
        gitlab_mr(V2, "2026-10-24T11:00:00.000Z", "merged", "W/\"m\""),
    );
    sandbox.sync();
    assert_eq!(queue(&socket, Some("company-a")).entries.len(), 0);
    let seen = sandbox.gitlab.requests().len();
    sandbox.sync();
    let requests = sandbox.gitlab.requests();
    assert!(
        !requests[seen..].contains(&"GET /merge_requests".to_owned()),
        "{:?}",
        &requests[seen..]
    );
    daemon.stop();
}

// The reasons of the GitLab queue (`docs/design/reviews.md`, "Waiting on me").

const KEY_88: &str = "company-a/checkout!88";
const NOTE_T1: &str = "2026-10-21T16:10:00.000Z";

/// The MR of the fixtures, without my request: `edit` then sets what the test is about. A new
/// `etag` is a new body.
fn set_mr(sandbox: &Sandbox, etag: &str, edit: impl FnOnce(&mut Value)) {
    let mut mr = fixture_json("gitlab/merge-request-88.v2.json");
    mr["reviewers"] = json!([]);
    mr["assignees"] = json!([]);
    edit(&mut mr);
    sandbox.gitlab.set(MR_88, with_etag(mr.to_string(), etag));
}

/// Not in the list of MRs to review.
fn no_review_list(sandbox: &Sandbox) {
    sandbox.gitlab.set("GET /merge_requests", route(EMPTY_LIST));
}

fn carol() -> Value {
    fixture_json("gitlab/merge-request-88.own-failed.json")["author"].clone()
}

fn set_list(sandbox: &Sandbox, list: &str, listed: bool) {
    let body = if listed {
        fixture("gitlab/merge-requests-reviews-for-me.json")
    } else {
        EMPTY_LIST.to_owned()
    };
    sandbox.gitlab.set(list, route(body));
}

fn set_mentioned(sandbox: &Sandbox, list: &str, pending: bool) {
    let file = if pending {
        "gitlab/todos-mentioned.json"
    } else {
        "gitlab/todos.empty.json"
    };
    sandbox.gitlab.set(list, route(fixture(file)));
}

/// My own MR: listed, with no comment from others, no pipeline, not mergeable, no approval.
fn set_own(sandbox: &Sandbox, etag: &str, edit: impl FnOnce(&mut Value)) {
    no_review_list(sandbox);
    set_list(sandbox, OWN, true);
    set_notes(sandbox, &[]);
    set_approved_by(sandbox, &[]);
    set_mr(sandbox, etag, |mr| {
        mr["author"] = carol();
        mr["detailed_merge_status"] = json!("not_approved");
        edit(mr);
    });
}

fn set_approved_by(sandbox: &Sandbox, users: &[&str]) {
    let mut approvals = fixture_json("gitlab/approve.response.json");
    approvals["user_has_approved"] = json!(false);
    approvals["approved_by"] = users
        .iter()
        .map(|user| json!({"user": {"id": 1, "username": user}}))
        .collect();
    sandbox.gitlab.set(
        "GET /projects/77/merge_requests/88/approvals",
        route(approvals.to_string()),
    );
}

/// The author, the time, and whether it is a system note.
type Note<'a> = (&'a str, &'a str, bool);

/// The discussions: one per note.
fn set_notes(sandbox: &Sandbox, notes: &[Note]) {
    let discussions: Vec<Value> = notes
        .iter()
        .enumerate()
        .map(|(index, (author, at, system))| {
            json!({
                "id": format!("d{index}"),
                "notes": [{
                    "id": index,
                    "body": "text",
                    "author": {"id": 1, "username": author},
                    "created_at": at,
                    "system": system,
                }],
            })
        })
        .collect();
    sandbox.gitlab.set(
        "GET /projects/77/merge_requests/88/discussions",
        route(Value::Array(discussions).to_string()),
    );
}

fn pipeline(status: &str, sha: &str) -> Value {
    json!({"id": 9001, "sha": sha, "status": status})
}

/// The reasons of the MR in the queue, by name, or `None` when it is not in the queue.
fn reasons_of(sandbox: &Sandbox) -> Option<Vec<QueueReason>> {
    queue(&sandbox.socket(), Some("company-a"))
        .entries
        .into_iter()
        .find(|entry| entry.key == KEY_88)
        .map(|entry| entry.reasons)
}

fn gitlab_failed_lists(result: &ReviewSyncResult) -> Vec<ReviewListFailure> {
    match &poll(result, "company-a", "gitlab.example.com").outcome {
        ReviewSyncOutcome::Synced { failed_lists, .. } => failed_lists.clone(),
        outcome => panic!("not synced: {outcome:?}"),
    }
}

fn seen(socket: &Path) -> common::HttpResponse {
    request(
        socket,
        "POST",
        &format!("/v1/reviews/queue/{GITLAB_KEY}/seen"),
        Some(&json!({})),
    )
}

/// The entry file of the MR, as JSON.
fn entry_file(sandbox: &Sandbox) -> Option<Value> {
    let path = sandbox
        .store("company-a")
        .join("queue/mrs/gitlab-company-a%2Fcheckout%2188.json");
    fs::read_to_string(path)
        .ok()
        .map(|text| serde_json::from_str(&text).unwrap())
}

#[test]
fn an_assigned_mr_has_the_reason_until_i_am_unassigned() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    no_review_list(&sandbox);
    set_list(&sandbox, ASSIGNED, true);
    set_mr(&sandbox, "W/\"a\"", |mr| {
        mr["assignees"] = json!([carol()]);
    });
    let result = sandbox.sync();
    assert_eq!(reasons_of(&sandbox), Some(vec![QueueReason::Assigned]));
    assert_eq!(gitlab_failed_lists(&result), []);
    // A list with no change writes nothing.
    let before = snapshot(&sandbox.store("company-a"));
    sandbox.sync();
    assert_eq!(snapshot(&sandbox.store("company-a")), before);

    // Unassigned: the read tells, with the MR no longer in the list.
    set_list(&sandbox, ASSIGNED, false);
    set_mr(&sandbox, "W/\"b\"", |_| {});
    let result = sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);
    assert_left_the_queue(&sandbox, "company-a", "gitlab.example.com", &result);
    daemon.stop();
}

#[test]
fn an_mr_that_i_wrote_and_is_assigned_to_me_has_no_assigned_reason() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |mr| mr["assignees"] = json!([carol()]));
    set_list(&sandbox, ASSIGNED, true);
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);
    assert_eq!(entry_file(&sandbox), None);
    daemon.stop();
}

#[test]
fn a_mention_comes_with_its_todo_and_goes_when_the_todo_is_done() {
    for list in [MENTIONED, ADDRESSED] {
        let sandbox = Sandbox::new();
        let daemon = sandbox.start();
        no_review_list(&sandbox);
        set_mr(&sandbox, "W/\"a\"", |_| {});
        // First read without a to-do: the ETag of the MR is known from here on.
        set_list(&sandbox, ASSIGNED, true);
        sandbox.sync();
        set_list(&sandbox, ASSIGNED, false);
        assert_eq!(reasons_of(&sandbox), None);

        set_mentioned(&sandbox, list, true);
        // The MR did not change: a 304 alone would give nothing to write the entry from.
        sandbox.sync();
        assert_eq!(
            reasons_of(&sandbox),
            Some(vec![QueueReason::Mentioned]),
            "{list}"
        );

        // A to-do done is seen on a 304 too.
        set_mentioned(&sandbox, list, false);
        sandbox.sync();
        assert_eq!(reasons_of(&sandbox), None, "{list}");
        daemon.stop();
    }
}

#[test]
fn a_new_comment_on_my_mr_comes_and_goes_with_my_reply() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |_| {});
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);

    set_notes(&sandbox, &[("bob", NOTE_T1, false)]);
    set_mr(&sandbox, "W/\"b\"", |mr| mr["author"] = carol());
    sandbox.sync();
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::OwnNewComments])
    );

    set_notes(
        &sandbox,
        &[
            ("bob", NOTE_T1, false),
            ("carol", "2026-10-21T17:00:00.000Z", false),
        ],
    );
    set_mr(&sandbox, "W/\"c\"", |mr| mr["author"] = carol());
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);
    daemon.stop();
}

#[test]
fn a_note_of_mine_or_a_system_note_gives_no_comment_reason() {
    let table: [(&str, &[Note]); 3] = [
        ("my note", &[("carol", NOTE_T1, false)]),
        ("a system note", &[("bob", NOTE_T1, true)]),
        (
            "a note older than my latest note",
            &[
                ("bob", NOTE_T1, false),
                ("carol", "2026-10-21T17:00:00.000Z", false),
            ],
        ),
    ];
    for (name, notes) in table {
        let sandbox = Sandbox::new();
        let daemon = sandbox.start();
        set_own(&sandbox, "W/\"a\"", |_| {});
        set_notes(&sandbox, notes);
        sandbox.sync();
        assert_eq!(reasons_of(&sandbox), None, "{name}");
        daemon.stop();
    }
}

#[test]
fn discussions_are_read_only_when_my_mr_changed_and_never_for_an_mr_of_another() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let discussions = "GET /projects/77/merge_requests/88/discussions";
    let count = || {
        sandbox
            .gitlab
            .requests()
            .iter()
            .filter(|request| *request == discussions)
            .count()
    };
    set_own(&sandbox, "W/\"a\"", |_| {});
    sandbox.sync();
    let reads = count();
    assert!(reads > 0);
    sandbox.sync();
    assert_eq!(count(), reads);
    daemon.stop();

    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    no_review_list(&sandbox);
    set_list(&sandbox, ASSIGNED, true);
    set_mr(&sandbox, "W/\"a\"", |mr| {
        mr["assignees"] = json!([carol()]);
    });
    sandbox.sync();
    assert!(
        !sandbox
            .gitlab
            .requests()
            .iter()
            .any(|request| request == discussions)
    );
    daemon.stop();
}

#[test]
fn opening_an_entry_removes_the_comment_reason_and_survives_a_rebuild_of_the_index() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    set_own(&sandbox, "W/\"a\"", |_| {});
    set_notes(&sandbox, &[("bob", NOTE_T1, false)]);
    sandbox.sync();
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::OwnNewComments])
    );

    // No forge call, one commit, the file holds `seen_at`.
    let requests = sandbox.gitlab.requests().len();
    let store = sandbox.store("company-a");
    let commits_before = commits(&store);
    let answer: ReviewSeenResult = ok(&seen(&socket));
    assert_eq!(answer.key, KEY_88);
    assert_eq!(answer.context.as_str(), "company-a");
    assert_eq!(answer.reasons, []);
    assert_eq!(sandbox.gitlab.requests().len(), requests);
    assert_eq!(commits(&store), commits_before + 1);
    // No reason left: out of the queue, and the file stays for its `seen_at`.
    assert_eq!(reasons_of(&sandbox), None);
    let file = entry_file(&sandbox).unwrap();
    assert_eq!(file["reasons"], json!([]));
    assert!(file["seen_at"].is_string());

    // A restart and a rebuilt index read the MR in full, and the note does not come back.
    daemon.stop();
    for name in ["index.db", "index.db-wal", "index.db-shm"] {
        let _ = fs::remove_file(sandbox.loom_home().join(name));
    }
    let daemon = sandbox.start();
    let result = sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);
    assert_eq!(
        entry_file(&sandbox).unwrap()["seen_at"],
        file["seen_at"],
        "{result:?}"
    );

    // A newer note comes back with the same `seen_at`.
    set_notes(
        &sandbox,
        &[
            ("bob", NOTE_T1, false),
            ("bob", "2099-01-01T00:00:00.000Z", false),
        ],
    );
    set_mr(&sandbox, "W/\"b\"", |mr| {
        mr["author"] = carol();
        mr["detailed_merge_status"] = json!("not_approved");
    });
    sandbox.sync();
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::OwnNewComments])
    );
    assert_eq!(entry_file(&sandbox).unwrap()["seen_at"], file["seen_at"]);

    // Merged: the file goes, with no reason left to list.
    set_mr(&sandbox, "W/\"c\"", |mr| {
        mr["author"] = carol();
        mr["state"] = json!("merged");
    });
    set_list(&sandbox, OWN, false);
    sandbox.sync();
    assert_eq!(entry_file(&sandbox), None);
    daemon.stop();
}

#[test]
fn a_parked_file_goes_when_my_mr_is_merged() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |_| {});
    set_notes(&sandbox, &[("bob", NOTE_T1, false)]);
    sandbox.sync();
    let _: ReviewSeenResult = ok(&seen(&sandbox.socket()));
    sandbox.sync();
    assert!(entry_file(&sandbox).is_some());
    set_list(&sandbox, OWN, false);
    set_mr(&sandbox, "W/\"b\"", |mr| {
        mr["author"] = carol();
        mr["state"] = json!("merged");
    });
    sandbox.sync();
    assert_eq!(entry_file(&sandbox), None);
    daemon.stop();
}

#[test]
fn opening_an_entry_keeps_its_other_reasons_and_refuses_a_bad_request() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    no_review_list(&sandbox);
    set_list(&sandbox, ASSIGNED, true);
    set_mr(&sandbox, "W/\"a\"", |mr| {
        mr["assignees"] = json!([carol()]);
    });

    let missing = seen(&socket);
    assert_eq!(missing.status, 404, "{}", missing.body);
    let bad = request(
        &socket,
        "POST",
        "/v1/reviews/queue/not-a-key/seen",
        Some(&json!({})),
    );
    assert_eq!(bad.status, 400, "{}", bad.body);

    sandbox.sync();
    let unknown = request(
        &socket,
        "POST",
        &format!("/v1/reviews/queue/{GITLAB_KEY}/seen"),
        Some(&json!({"x": 1})),
    );
    assert_eq!(unknown.status, 400, "{}", unknown.body);
    let answer: ReviewSeenResult = ok(&seen(&socket));
    assert_eq!(answer.reasons, [QueueReason::Assigned]);
    let entries = queue(&socket, Some("company-a")).entries;
    assert_eq!(entries[0].reasons, [QueueReason::Assigned]);
    assert_eq!(entries[0].seen_at, Some(answer.seen_at));
    // A poll that follows keeps it.
    set_mr(&sandbox, "W/\"b\"", |mr| {
        mr["assignees"] = json!([carol()]);
        mr["title"] = json!("Another title");
    });
    sandbox.sync();
    let entries = queue(&socket, Some("company-a")).entries;
    assert_eq!(entries[0].title, "Another title");
    assert_eq!(entries[0].seen_at, Some(answer.seen_at));
    daemon.stop();
}

#[test]
fn a_failed_pipeline_counts_on_the_head_with_the_same_updated_at() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |mr| {
        mr["head_pipeline"] = pipeline("running", V2);
    });
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);

    // The pipeline result: a new body, the same `updated_at`, and the list has no `updated_after`.
    let updated_at = "2026-10-23T11:06:00.000Z";
    set_mr(&sandbox, "W/\"b\"", |mr| {
        mr["author"] = carol();
        mr["updated_at"] = json!(updated_at);
        mr["head_pipeline"] = pipeline("failed", V2);
    });
    sandbox.sync();
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::OwnPipelineFailed])
    );
    let requests = sandbox.gitlab.requests();
    assert!(requests.iter().filter(|request| *request == OWN).count() >= 2);
    assert!(
        !requests
            .iter()
            .any(|request| request.starts_with(OWN) && request.contains("+after")),
        "{requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.starts_with(ASSIGNED) && request.contains("+after")),
        "{requests:?}"
    );

    // A new pipeline on the head that is not failed.
    set_mr(&sandbox, "W/\"c\"", |mr| {
        mr["author"] = carol();
        mr["updated_at"] = json!(updated_at);
        mr["head_pipeline"] = pipeline("running", V2);
    });
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);
    daemon.stop();
}

#[test]
fn a_failed_pipeline_on_an_older_head_gives_no_reason() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |mr| {
        mr["head_pipeline"] = pipeline("failed", V1);
    });
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);
    daemon.stop();
}

#[test]
fn ready_to_merge_needs_mergeable_an_approval_and_no_draft() {
    let table = [
        ("mergeable, no approval", "mergeable", false, &[][..], None),
        ("a draft", "mergeable", true, &["bob"][..], None),
        ("not mergeable", "ci_must_pass", false, &["bob"][..], None),
        (
            "mergeable and approved",
            "mergeable",
            false,
            &["bob"][..],
            Some(vec![QueueReason::OwnReadyToMerge]),
        ),
    ];
    for (name, status, draft, approved_by, expected) in table {
        let sandbox = Sandbox::new();
        let daemon = sandbox.start();
        set_own(&sandbox, "W/\"a\"", |mr| {
            mr["detailed_merge_status"] = json!(status);
            mr["draft"] = json!(draft);
        });
        set_approved_by(&sandbox, approved_by);
        sandbox.sync();
        assert_eq!(reasons_of(&sandbox), expected, "{name}");
        daemon.stop();
    }
}

#[test]
fn an_approval_that_leaves_the_body_as_it_was_is_seen_and_not_mergeable_ends_the_reason() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let mergeable = |mr: &mut Value| {
        mr["author"] = carol();
        mr["detailed_merge_status"] = json!("mergeable");
    };
    set_own(&sandbox, "W/\"a\"", mergeable);
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);

    // Same ETag: only the approvals tell.
    set_approved_by(&sandbox, &["bob"]);
    sandbox.sync();
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::OwnReadyToMerge])
    );

    // GitLab computes the status again: no change, so no flapping.
    set_mr(&sandbox, "W/\"b\"", |mr| {
        mr["author"] = carol();
        mr["detailed_merge_status"] = json!("checking");
    });
    sandbox.sync();
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::OwnReadyToMerge])
    );

    set_mr(&sandbox, "W/\"c\"", |mr| {
        mr["author"] = carol();
        mr["detailed_merge_status"] = json!("conflict");
    });
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);
    daemon.stop();
}

#[test]
fn removal_of_one_reason_keeps_the_entry_with_another() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    // Requested and assigned.
    set_list(&sandbox, ASSIGNED, true);
    let both = |mr: &mut Value| {
        mr["assignees"] = json!([carol()]);
        mr["reviewers"] = json!([carol()]);
    };
    set_mr(&sandbox, "W/\"a\"", both);
    sandbox.sync();
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::ReviewRequested, QueueReason::Assigned])
    );

    // My verdict on the head: the request reason goes and stays gone, on a read and on a 304.
    sandbox.gitlab.set(
        "GET /projects/77/merge_requests/88/approvals",
        approvals(true),
    );
    set_mr(&sandbox, "W/\"b\"", both);
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), Some(vec![QueueReason::Assigned]));
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), Some(vec![QueueReason::Assigned]));

    // Not requested any more, still assigned: the entry stays. Then unassigned: it goes.
    no_review_list(&sandbox);
    set_mr(&sandbox, "W/\"c\"", |mr| {
        mr["assignees"] = json!([carol()]);
    });
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), Some(vec![QueueReason::Assigned]));
    set_list(&sandbox, ASSIGNED, false);
    set_mr(&sandbox, "W/\"d\"", |_| {});
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);
    daemon.stop();
}

#[test]
fn a_start_on_an_assigned_mr_creates_a_task_that_the_next_poll_does_not_cancel() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    no_review_list(&sandbox);
    set_list(&sandbox, ASSIGNED, true);
    let assigned = |mr: &mut Value| mr["assignees"] = json!([carol()]);
    set_mr(&sandbox, "W/\"a\"", assigned);
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), Some(vec![QueueReason::Assigned]));

    let started = start(&socket, GITLAB_KEY);
    assert!(started.created);
    let id = started.task.summary.id.to_string();
    assert_eq!(status(&socket, &id), Status::Active);
    assert_eq!(reasons_of(&sandbox), None);
    let store = sandbox.store("company-a");
    let sync: Value = serde_json::from_str(
        &fs::read_to_string(
            store
                .join("tasks")
                .join(&id)
                .join("mrs/gitlab-company-a%2Fcheckout%2188/sync.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(sync["requested"], json!(false));

    // The trap: a `requested: true` here would read as a removed request, and cancel the task.
    sandbox.sync();
    assert_eq!(status(&socket, &id), Status::Active);
    // Unassigned: the task follows its own rules, and no entry comes back.
    set_list(&sandbox, ASSIGNED, false);
    set_mr(&sandbox, "W/\"b\"", |_| {});
    sandbox.sync();
    assert_eq!(status(&socket, &id), Status::Active);
    assert_eq!(reasons_of(&sandbox), None);
    daemon.stop();
}

#[test]
fn a_start_on_a_mentioned_mr_creates_a_task() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    no_review_list(&sandbox);
    set_mentioned(&sandbox, MENTIONED, true);
    set_mr(&sandbox, "W/\"a\"", |_| {});
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), Some(vec![QueueReason::Mentioned]));
    let id = start(&socket, GITLAB_KEY).task.summary.id.to_string();
    sandbox.sync();
    assert_eq!(status(&socket, &id), Status::Active);
    assert_eq!(reasons_of(&sandbox), None);
    daemon.stop();
}

#[test]
fn a_start_with_my_request_gone_but_still_assigned_creates_the_task() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    set_list(&sandbox, ASSIGNED, true);
    set_mr(&sandbox, "W/\"a\"", |mr| {
        mr["assignees"] = json!([carol()]);
        mr["reviewers"] = json!([carol()]);
    });
    sandbox.sync();
    set_mr(&sandbox, "W/\"b\"", |mr| {
        mr["assignees"] = json!([carol()]);
    });
    assert!(start(&socket, GITLAB_KEY).created);
    daemon.stop();
}

#[test]
fn a_start_with_no_reason_left_is_refused_and_removes_the_entry() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    no_review_list(&sandbox);
    set_list(&sandbox, ASSIGNED, true);
    set_mr(&sandbox, "W/\"a\"", |mr| {
        mr["assignees"] = json!([carol()]);
    });
    sandbox.sync();
    set_mr(&sandbox, "W/\"b\"", |_| {});
    let refused = start_response(&socket, GITLAB_KEY);
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert_eq!(reasons_of(&sandbox), None);
    assert_eq!(tasks(&socket, "company-a").tasks.len(), 0);
    daemon.stop();
}

#[test]
fn a_start_on_my_own_mr_is_refused_and_the_entry_stays() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    set_own(&sandbox, "W/\"a\"", |mr| {
        mr["head_pipeline"] = pipeline("failed", V2);
    });
    sandbox.sync();
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::OwnPipelineFailed])
    );
    let refused = start_response(&socket, GITLAB_KEY);
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::OwnPipelineFailed])
    );
    assert_eq!(tasks(&socket, "company-a").tasks.len(), 0);
    daemon.stop();
}

#[test]
fn an_mr_with_a_review_task_gets_no_entry_for_another_reason() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync_and_start();
    set_list(&sandbox, ASSIGNED, true);
    set_mentioned(&sandbox, MENTIONED, true);
    set_mr(&sandbox, "W/\"a\"", |mr| {
        mr["assignees"] = json!([carol()]);
        mr["reviewers"] = json!([carol()]);
    });
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);
    daemon.stop();
}

#[test]
fn a_list_that_fails_keeps_the_entries_and_is_reported() {
    let refused = Route {
        status: 403,
        ..route(r#"{"message":"403 Forbidden"}"#)
    };
    let broken = Route {
        status: 500,
        ..route(r#"{"message":"500 Internal Server Error"}"#)
    };
    let table = [
        (
            MENTIONED,
            ReviewList::Mentioned,
            &refused,
            ReviewListFailureKind::Forbidden,
        ),
        (
            ADDRESSED,
            ReviewList::Mentioned,
            &broken,
            ReviewListFailureKind::Other,
        ),
        (
            ASSIGNED,
            ReviewList::Assigned,
            &broken,
            ReviewListFailureKind::Other,
        ),
        (
            OWN,
            ReviewList::Own,
            &refused,
            ReviewListFailureKind::Forbidden,
        ),
    ];
    for (list, name, answer, kind) in table {
        let sandbox = Sandbox::new();
        let daemon = sandbox.start();
        // An entry with a reason from each list.
        no_review_list(&sandbox);
        set_list(&sandbox, ASSIGNED, true);
        set_mentioned(&sandbox, MENTIONED, true);
        set_mr(&sandbox, "W/\"a\"", |mr| {
            mr["assignees"] = json!([carol()]);
        });
        sandbox.sync();
        let held = vec![QueueReason::Assigned, QueueReason::Mentioned];
        assert_eq!(reasons_of(&sandbox), Some(held.clone()), "{list}");
        let before = snapshot(&sandbox.store("company-a"));

        sandbox.gitlab.set(list, answer.clone());
        let result = sandbox.sync();
        let failed = gitlab_failed_lists(&result);
        assert_eq!(failed.len(), 1, "{list}: {failed:?}");
        assert_eq!((failed[0].list, failed[0].kind), (name, kind), "{list}");
        assert_eq!(reasons_of(&sandbox), Some(held.clone()), "{list}");
        assert_eq!(snapshot(&sandbox.store("company-a")), before, "{list}");

        // The next poll reads the list again: it comes back with no failure.
        let empty = if list == MENTIONED || list == ADDRESSED {
            fixture("gitlab/todos.empty.json")
        } else {
            EMPTY_LIST.to_owned()
        };
        sandbox.gitlab.set(list, route(empty));
        let result = sandbox.sync();
        assert_eq!(gitlab_failed_lists(&result), [], "{list}");
        daemon.stop();
    }
}

#[test]
fn a_list_failure_of_the_host_stops_the_poll_and_changes_nothing() {
    for answer in [
        Route {
            status: 429,
            headers: vec![("retry-after", "3600".to_owned())],
            ..route(r#"{"message":"429 Too Many Requests"}"#)
        },
        Route {
            status: 401,
            ..route(r#"{"message":"401 Unauthorized"}"#)
        },
    ] {
        let status = answer.status;
        let sandbox = Sandbox::new();
        let daemon = sandbox.start();
        no_review_list(&sandbox);
        set_mentioned(&sandbox, MENTIONED, true);
        set_mr(&sandbox, "W/\"a\"", |_| {});
        sandbox.sync();
        let before = snapshot(&sandbox.store("company-a"));
        sandbox.gitlab.set(MENTIONED, answer);
        let result = sandbox.sync();
        let outcome = &poll(&result, "company-a", "gitlab.example.com").outcome;
        match status {
            429 => assert!(
                matches!(outcome, ReviewSyncOutcome::RateLimited { .. }),
                "{outcome:?}"
            ),
            _ => assert!(
                matches!(outcome, ReviewSyncOutcome::Failed { .. }),
                "{outcome:?}"
            ),
        }
        assert_eq!(snapshot(&sandbox.store("company-a")), before);
        assert_eq!(reasons_of(&sandbox), Some(vec![QueueReason::Mentioned]));
        daemon.stop();
    }
}

#[test]
fn an_entry_follows_its_read_when_its_list_fails() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |mr| {
        mr["head_pipeline"] = pipeline("failed", V2);
    });
    sandbox.sync();
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::OwnPipelineFailed])
    );
    sandbox.gitlab.set(
        OWN,
        Route {
            status: 500,
            ..route(r#"{"message":"500"}"#)
        },
    );
    set_mr(&sandbox, "W/\"b\"", |mr| {
        mr["author"] = carol();
        mr["head_pipeline"] = pipeline("success", V2);
    });
    let result = sandbox.sync();
    assert_eq!(gitlab_failed_lists(&result).len(), 1);
    assert_eq!(reasons_of(&sandbox), None);
    daemon.stop();
}

#[test]
fn a_404_on_an_entry_for_another_reason_means_gone_unless_a_list_shows_the_mr() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    no_review_list(&sandbox);
    set_list(&sandbox, ASSIGNED, true);
    set_mr(&sandbox, "W/\"a\"", |mr| {
        mr["assignees"] = json!([carol()]);
    });
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), Some(vec![QueueReason::Assigned]));
    sandbox.gitlab.set(
        MR_88,
        Route {
            status: 404,
            ..route(r#"{"message":"404 Not found"}"#)
        },
    );
    // A list shows it: a failure, and the entry stays.
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")).5,
        1
    );
    assert_eq!(reasons_of(&sandbox), Some(vec![QueueReason::Assigned]));
    // No list shows it: gone.
    set_list(&sandbox, ASSIGNED, false);
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);
    assert_eq!(entry_file(&sandbox), None);
    daemon.stop();
}

#[test]
fn a_poll_with_every_list_costs_five_list_requests_and_one_etag_read_and_commits_nothing() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |mr| {
        mr["head_pipeline"] = pipeline("failed", V2);
    });
    sandbox.sync();
    let before = snapshot(&sandbox.store("company-a"));
    let requests = sandbox.gitlab.requests().len();
    let result = sandbox.sync();
    assert_eq!(snapshot(&sandbox.store("company-a")), before);
    let all = sandbox.gitlab.requests();
    // Two contexts poll this host in the sandbox.
    let lists = all[requests..]
        .iter()
        .filter(|request| {
            request.starts_with("GET /merge_requests") || request.starts_with("GET /todos")
        })
        .count();
    assert_eq!(lists, 10, "{:?}", &all[requests..]);
    let reads: Vec<&String> = all[requests..]
        .iter()
        .filter(|request| request.contains("/merge_requests/88"))
        .collect();
    assert_eq!(reads, [MR_88], "{result:?}");
    daemon.stop();
}

const APPROVALS_88: &str = "GET /projects/77/merge_requests/88/approvals";
const VERSIONS_88: &str = "GET /projects/77/merge_requests/88/versions";

fn count_of(sandbox: &Sandbox, what: &str) -> usize {
    sandbox
        .gitlab
        .requests()
        .iter()
        .filter(|request| *request == what)
        .count()
}

fn not_found() -> Route {
    Route {
        status: 404,
        ..route(r#"{"message":"404 Not found"}"#)
    }
}

/// My own open MR that is mergeable, with a new comment, so that it has an entry and its
/// approvals are read at each poll.
fn own_mergeable_with_an_entry(sandbox: &Sandbox) {
    set_own(sandbox, "W/\"a\"", |mr| {
        mr["detailed_merge_status"] = json!("mergeable");
    });
    set_notes(sandbox, &[("bob", NOTE_T1, false)]);
    sandbox.sync();
    assert_eq!(reasons_of(sandbox), Some(vec![QueueReason::OwnNewComments]));
}

#[test]
fn waiting_for_approval_and_a_404_removes_the_entry() {
    // The MR answers 404, with or without its approvals.
    for approvals_too in [true, false] {
        let sandbox = Sandbox::new();
        let daemon = sandbox.start();
        own_mergeable_with_an_entry(&sandbox);
        sandbox.gitlab.set(MR_88, not_found());
        if approvals_too {
            sandbox.gitlab.set(APPROVALS_88, not_found());
        }
        set_list(&sandbox, OWN, false);
        let result = sandbox.sync();
        assert_eq!(reasons_of(&sandbox), None, "{result:?}");
        assert_eq!(entry_file(&sandbox), None);
        assert_eq!(
            counts(poll(&result, "company-a", "gitlab.example.com")).5,
            0,
            "{result:?}"
        );
        daemon.stop();
    }
}

#[test]
fn a_404_on_the_approvals_alone_reads_the_mr_and_stays_a_failure() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    own_mergeable_with_an_entry(&sandbox);
    sandbox.gitlab.set(APPROVALS_88, not_found());
    let reads = count_of(&sandbox, MR_88);
    let result = sandbox.sync();
    // A 304 proves nothing here: the MR itself is read in full, and it is there.
    assert_eq!(count_of(&sandbox, MR_88), reads + 2);
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")).5,
        1
    );
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::OwnNewComments])
    );
    daemon.stop();
}

#[test]
fn an_approval_revoked_on_the_same_etag_ends_the_reason() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |mr| {
        mr["detailed_merge_status"] = json!("mergeable");
    });
    set_approved_by(&sandbox, &["bob"]);
    sandbox.sync();
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::OwnReadyToMerge])
    );
    // No change: the approvals are read, the MR stays a 304.
    let before = snapshot(&sandbox.store("company-a"));
    sandbox.sync();
    assert_eq!(snapshot(&sandbox.store("company-a")), before);
    set_approved_by(&sandbox, &[]);
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);
    daemon.stop();
}

#[test]
fn a_status_that_gitlab_is_computing_does_not_end_ready_to_merge() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |mr| {
        mr["detailed_merge_status"] = json!("mergeable");
    });
    set_approved_by(&sandbox, &["bob"]);
    sandbox.sync();
    for (etag, status) in [
        ("W/\"b\"", "preparing"),
        ("W/\"c\"", "approvals_syncing"),
        ("W/\"d\"", "unchecked"),
    ] {
        set_mr(&sandbox, etag, |mr| {
            mr["author"] = carol();
            mr["detailed_merge_status"] = json!(status);
        });
        sandbox.sync();
        assert_eq!(
            reasons_of(&sandbox),
            Some(vec![QueueReason::OwnReadyToMerge]),
            "{status}"
        );
    }
    daemon.stop();
}

#[test]
fn a_failed_merged_results_or_train_pipeline_counts_and_a_branch_of_that_name_does_not() {
    const MERGE_COMMIT: &str = "1604b0c46c395822e4e9478777f8e54ac99fe5b9";
    let fixture_body = fixture_json("gitlab/merge-request-88.merged-results.json");
    let table = [
        (
            "merged results",
            "refs/merge-requests/88/merge",
            MERGE_COMMIT,
            true,
        ),
        (
            "merge train",
            "refs/merge-requests/88/train",
            MERGE_COMMIT,
            true,
        ),
        // A pipeline for the MR on its own head compares its SHA.
        (
            "detached, older head",
            "refs/merge-requests/88/head",
            V1,
            false,
        ),
        (
            "detached, on the head",
            "refs/merge-requests/88/head",
            V2,
            true,
        ),
        // A source branch can end in `/merge`: only the MR ref has no comparable SHA.
        ("a branch named so", "feature/merge", V1, false),
    ];
    for (name, reference, sha, expected) in table {
        let sandbox = Sandbox::new();
        let daemon = sandbox.start();
        set_own(&sandbox, "W/\"a\"", |mr| {
            mr["head_pipeline"] = json!({
                "sha": sha,
                "ref": reference,
                "status": "failed",
            });
        });
        sandbox.sync();
        let reasons = expected.then(|| vec![QueueReason::OwnPipelineFailed]);
        assert_eq!(reasons_of(&sandbox), reasons, "{name}");
        daemon.stop();
    }
    // The composed fixture too.
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |_| {});
    sandbox
        .gitlab
        .set(MR_88, with_etag(fixture_body.to_string(), "W/\"f\""));
    sandbox.sync();
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::OwnPipelineFailed])
    );
    daemon.stop();
}

#[test]
fn a_host_failure_on_the_first_list_stops_the_second() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.gitlab.set(
        ASSIGNED,
        Route {
            status: 429,
            headers: vec![("retry-after", "3600".to_owned())],
            ..route(r#"{"message":"429"}"#)
        },
    );
    let result = sandbox.sync();
    assert!(
        matches!(
            poll(&result, "company-a", "gitlab.example.com").outcome,
            ReviewSyncOutcome::RateLimited { .. }
        ),
        "{result:?}"
    );
    for later in [OWN, MENTIONED, ADDRESSED] {
        assert_eq!(count_of(&sandbox, later), 0, "{later}");
    }
    daemon.stop();
}

#[test]
fn a_list_failure_that_echoes_the_token_is_redacted() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.gitlab.set(
        MENTIONED,
        Route {
            status: 403,
            ..route(format!(r#"{{"message":"refused for {GITLAB_TOKEN}"}}"#))
        },
    );
    let result = sandbox.sync();
    let failed = gitlab_failed_lists(&result);
    assert_eq!(failed.len(), 1, "{result:?}");
    assert!(!failed[0].error.contains(GITLAB_TOKEN), "{failed:?}");
    assert!(failed[0].error.contains("refused"), "{failed:?}");
    daemon.stop();
}

#[test]
fn approvals_are_read_at_each_poll_only_for_my_own_mergeable_mr() {
    // Mine, mergeable: one read per poll, on a 304.
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |mr| {
        mr["detailed_merge_status"] = json!("mergeable");
    });
    sandbox.sync();
    let reads = count_of(&sandbox, APPROVALS_88);
    sandbox.sync();
    assert_eq!(count_of(&sandbox, APPROVALS_88), reads + 1);
    daemon.stop();

    // Mine, not mergeable, or a draft; and the MR of another, mergeable: none on a 304.
    let table: [(&str, bool, &str, bool); 3] = [
        ("not mergeable", true, "conflict", false),
        ("a draft", true, "mergeable", true),
        ("of another", false, "mergeable", false),
    ];
    for (name, mine, status, draft) in table {
        let sandbox = Sandbox::new();
        let daemon = sandbox.start();
        set_list(&sandbox, ASSIGNED, true);
        set_own(&sandbox, "W/\"a\"", |mr| {
            if !mine {
                mr["author"] = fixture_json("gitlab/merge-request-88.v2.json")["author"].clone();
            }
            mr["assignees"] = json!([carol()]);
            mr["detailed_merge_status"] = json!(status);
            mr["draft"] = json!(draft);
        });
        sandbox.sync();
        let reads = count_of(&sandbox, APPROVALS_88);
        sandbox.sync();
        assert_eq!(count_of(&sandbox, APPROVALS_88), reads, "{name}");
        daemon.stop();
    }
}

#[test]
fn a_mention_with_an_entry_or_a_task_keeps_the_etag_of_the_mr() {
    // An entry: the 304 holds, so no approvals are read.
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    no_review_list(&sandbox);
    set_mentioned(&sandbox, MENTIONED, true);
    set_mr(&sandbox, "W/\"a\"", |_| {});
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), Some(vec![QueueReason::Mentioned]));
    let reads = count_of(&sandbox, APPROVALS_88);
    sandbox.sync();
    assert_eq!(count_of(&sandbox, APPROVALS_88), reads);
    daemon.stop();

    // A task: no versions are read again.
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    sandbox.sync_and_start();
    sandbox.sync();
    set_mentioned(&sandbox, MENTIONED, true);
    let reads = count_of(&sandbox, VERSIONS_88);
    sandbox.sync();
    sandbox.sync();
    assert_eq!(count_of(&sandbox, VERSIONS_88), reads);
    assert_eq!(reasons_of(&sandbox), None);
    daemon.stop();
}

#[test]
fn a_mention_that_gives_no_entry_is_read_without_its_etag_once() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    sandbox.sync();
    let id = start(&socket, GITLAB_KEY).task.summary.id.to_string();
    let done = request(
        &socket,
        "POST",
        &format!("/v1/tasks/{id}/actions/done"),
        None,
    );
    assert_eq!(done.status, 200, "{}", done.body);
    daemon.stop();
    let store = sandbox.store("company-a");
    fs::create_dir_all(store.join("archive/2026")).unwrap();
    fs::rename(
        store.join("tasks").join(&id),
        store.join("archive/2026").join(&id),
    )
    .unwrap();

    // Archived, same head: a to-do alone brings no entry back.
    let daemon = sandbox.start();
    set_mentioned(&sandbox, MENTIONED, true);
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), None);
    let reads = count_of(&sandbox, APPROVALS_88);
    sandbox.sync();
    sandbox.sync();
    assert_eq!(count_of(&sandbox, APPROVALS_88), reads);
    assert_eq!(reasons_of(&sandbox), None);
    daemon.stop();
}

#[test]
fn opening_an_entry_is_refused_on_a_machine_that_does_not_poll_it_and_can_be_repeated() {
    let sandbox = Sandbox::new();
    let config = CONFIG.replace(
        "[contexts.company-a]\n",
        "[contexts.company-a]\nmachines = [\"work\", \"laptop\"]\n",
    );
    fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
    let daemon = sandbox.start();
    let dir = sandbox.store("company-a").join("queue/mrs");
    fs::create_dir_all(&dir).unwrap();
    let mut mr = fixture_json("gitlab/merge-request-88.v2.json");
    mr["author"] = carol();
    let file = "gitlab-company-a%2Fcheckout%2188.json";
    fs::write(
        dir.join(file),
        json!({
            "v": 1,
            "key": KEY_88,
            "forge_ids": {"forge": "gitlab", "id": mr["id"], "project_id": 77},
            "title": mr["title"],
            "author": "carol",
            "web_url": mr["web_url"],
            "head_sha": V2,
            "updated_at": "2026-10-23T11:06:00Z",
            "reasons": ["own_new_comments"],
        })
        .to_string(),
    )
    .unwrap();
    let before = fs::read(dir.join(file)).unwrap();
    let refused = seen(&sandbox.socket());
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert_eq!(fs::read(dir.join(file)).unwrap(), before);
    daemon.stop();

    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |_| {});
    set_notes(&sandbox, &[("bob", NOTE_T1, false)]);
    sandbox.sync();
    let first: ReviewSeenResult = ok(&seen(&sandbox.socket()));
    assert_eq!(first.reasons, []);
    let again: ReviewSeenResult = ok(&seen(&sandbox.socket()));
    assert_eq!(again.reasons, []);
    daemon.stop();
}

#[test]
fn a_file_with_no_reason_is_not_there_for_a_start() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    set_own(&sandbox, "W/\"a\"", |_| {});
    set_notes(&sandbox, &[("bob", NOTE_T1, false)]);
    sandbox.sync();
    let _: ReviewSeenResult = ok(&seen(&socket));
    let requests = sandbox.gitlab.requests().len();
    let refused = start_response(&socket, GITLAB_KEY);
    assert_eq!(refused.status, 404, "{}", refused.body);
    assert_eq!(sandbox.gitlab.requests().len(), requests);
    assert!(entry_file(&sandbox).is_some());
    daemon.stop();
}

#[test]
fn a_file_with_no_reason_of_another_context_is_not_reported_at_each_poll() {
    let sandbox = sandbox_for_a_move(
        "\n[contexts.personal.forges.\"gitlab.example.com\"]\ntoken_env = \"LOOM_TEST_CLIENT_TOKEN\"\n",
    );
    let mut other = fixture_json("gitlab/user.json");
    other["username"] = json!("carol-client");
    sandbox.gitlab.set(
        &format!("GET /user@{CLIENT_TOKEN}"),
        route(other.to_string()),
    );
    for list in ["GET /merge_requests", OWN, ASSIGNED] {
        sandbox
            .gitlab
            .set(&format!("{list}@{CLIENT_TOKEN}"), route(EMPTY_LIST));
    }
    let daemon = sandbox.start();
    set_own(&sandbox, "W/\"a\"", |_| {});
    set_notes(&sandbox, &[("bob", NOTE_T1, false)]);
    sandbox.sync();
    sandbox.sync();
    let _: ReviewSeenResult = ok(&seen(&sandbox.socket()));
    register_the_clone(&sandbox);
    let result = sandbox.sync();
    assert_eq!(
        counts(poll(&result, "company-a", "gitlab.example.com")).5,
        0,
        "{result:?}"
    );
    assert!(entry_file(&sandbox).is_some());
    daemon.stop();
}

#[test]
fn a_new_mention_on_an_entry_keeps_the_etag_and_adds_the_reason() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    no_review_list(&sandbox);
    set_list(&sandbox, ASSIGNED, true);
    set_mr(&sandbox, "W/\"a\"", |mr| {
        mr["assignees"] = json!([carol()]);
    });
    sandbox.sync();
    assert_eq!(reasons_of(&sandbox), Some(vec![QueueReason::Assigned]));
    set_mentioned(&sandbox, MENTIONED, true);
    let reads = count_of(&sandbox, APPROVALS_88);
    sandbox.sync();
    assert_eq!(count_of(&sandbox, APPROVALS_88), reads);
    assert_eq!(
        reasons_of(&sandbox),
        Some(vec![QueueReason::Assigned, QueueReason::Mentioned])
    );
    daemon.stop();
}

#[test]
fn a_file_with_no_reason_in_another_context_makes_no_key_ambiguous() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let socket = sandbox.socket();
    no_review_list(&sandbox);
    set_list(&sandbox, ASSIGNED, true);
    set_mr(&sandbox, "W/\"a\"", |mr| {
        mr["assignees"] = json!([carol()]);
    });
    sandbox.sync();
    let mut parked = entry_file(&sandbox).unwrap();
    parked["reasons"] = json!([]);
    let dir = sandbox.store("personal").join("queue/mrs");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("gitlab-company-a%2Fcheckout%2188.json"),
        parked.to_string(),
    )
    .unwrap();

    let answer: ReviewSeenResult = ok(&seen(&socket));
    assert_eq!(answer.context.as_str(), "company-a");
    let started = start(&socket, GITLAB_KEY);
    assert!(started.created);
    daemon.stop();
}
