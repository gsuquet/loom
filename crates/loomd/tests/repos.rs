//! The repository index of `loomd` end to end: fixture repositories under a temporary `HOME`,
//! the daemon queried over its unix socket.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fmt::Write as _;
use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{
    ApiError, ErrorCode, MaybeKnown, PlannedAction, RepoHead, RepoList, RepoSummary,
    RepoUpdatePolicy, ScanResult, SkipReason, SyncOutcome, SyncResult, Untracked, UpdateOutcome,
};
use schemars::schema_for;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(20);

const CONFIG: &str = r#"
[repos]
roots = ["~/perso"]

[contexts.personal]
match.paths = ["~/perso"]

[contexts.work]
match.hosts = ["gitlab.com/company"]
"#;

/// A loom home and a user home under `/tmp`: the macOS default `$TMPDIR` is too long for a
/// socket path.
struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("loomd-repos-")
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

    fn index(&self) -> PathBuf {
        self.loom_home().join("index.db")
    }

    fn start(&self) -> Daemon {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
        command
            .env("LOOM_HOME", self.loom_home())
            .env("HOME", self.user_home())
            .env("XDG_CONFIG_HOME", self.user_home().join(".config"))
            // The git commands of the automatic updates: no global or system configuration.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_SSH_COMMAND")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("NO_COLOR")
            .arg("--color=never")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Daemon::start(&mut command, &self.socket())
    }

    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> HttpResponse {
        request(&self.socket(), method, path, body)
    }

    /// `GET path`, expecting `200` and a body of type `T` valid against its schema.
    fn get<T: DeserializeOwned + schemars::JsonSchema>(&self, path: &str) -> T {
        ok(&self.request("GET", path, None))
    }

    fn slugs(&self, path: &str) -> Vec<String> {
        self.get::<RepoList>(path)
            .repos
            .into_iter()
            .map(|repo| repo.identity.slug)
            .collect()
    }

    fn scan(&self) -> ScanResult {
        ok(&self.request("POST", "/v1/repos/scan", None))
    }
}

/// A repository at `dir` with a hand-written `.git` directory and `remotes` (name, URL).
fn fake_repo(dir: &Path, remotes: &[(&str, &str)]) {
    let git_dir = dir.join(".git");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let mut config = String::new();
    for (name, url) in remotes {
        writeln!(config, "[remote \"{name}\"]\n\turl = {url}").unwrap();
    }
    fs::write(git_dir.join("config"), config).unwrap();
}

/// A running daemon, killed when dropped.
struct Daemon {
    child: Child,
}

impl Daemon {
    fn start(command: &mut Command, socket: &Path) -> Self {
        let mut daemon = Self {
            child: command.spawn().unwrap(),
        };
        let deadline = Instant::now() + TIMEOUT;
        while UnixStream::connect(socket).is_err() {
            if let Some(status) = daemon.child.try_wait().unwrap() {
                panic!("loomd exited with {status}");
            }
            assert!(Instant::now() < deadline, "socket never appeared");
            thread::sleep(POLL);
        }
        daemon
    }

    /// Stops the daemon with `SIGTERM` and waits for it.
    fn stop(mut self) {
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + TIMEOUT;
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "loomd did not stop");
            thread::sleep(POLL);
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct HttpResponse {
    status: u16,
    body: String,
}

/// Minimal HTTP/1.1 client: one request, `Connection: close`, body read to the end.
fn request(socket: &Path, method: &str, path: &str, body: Option<&Value>) -> HttpResponse {
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    let content_type = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/json\r\n"
    };
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: loomd\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status = head
        .lines()
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    HttpResponse {
        status,
        body: body.to_owned(),
    }
}

/// The body of a `200` response, checked against the schema of `T`.
fn ok<T: DeserializeOwned + schemars::JsonSchema>(response: &HttpResponse) -> T {
    assert_eq!(response.status, 200, "{}", response.body);
    let value: Value = serde_json::from_str(&response.body).unwrap();
    let schema = serde_json::to_value(schema_for!(T)).unwrap();
    assert!(jsonschema::is_valid(&schema, &value), "{value}");
    serde_json::from_value(value).unwrap()
}

fn assert_api_error(response: &HttpResponse, status: u16, code: ErrorCode) {
    assert_eq!(response.status, status, "{}", response.body);
    let error: ApiError = serde_json::from_str(&response.body).unwrap();
    assert_eq!(error.code, code, "{}", response.body);
}

/// Polls `GET /v1/repos` until it lists `count` repositories.
fn wait_for_repos(sandbox: &Sandbox, count: usize) -> Vec<String> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let slugs = sandbox.slugs("/v1/repos");
        if slugs.len() == count {
            return slugs;
        }
        assert!(Instant::now() < deadline, "{slugs:?}");
        thread::sleep(POLL);
    }
}

#[test]
fn scan_on_start_indexes_the_fixture_repositories() {
    let sandbox = Sandbox::new();
    let perso = sandbox.user_home().join("perso");
    fake_repo(&perso.join("a"), &[]);
    fake_repo(
        &perso.join("clients/b"),
        &[("origin", "git@gitlab.com:company/b.git")],
    );
    fake_repo(&perso.join("x/y/z/w/too-deep"), &[]);
    let daemon = sandbox.start();

    assert_eq!(wait_for_repos(&sandbox, 2), ["personal/a", "work/b"]);
    assert_eq!(
        fs::metadata(sandbox.index()).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let a: RepoSummary = sandbox.get("/v1/repos/personal/a");
    assert_eq!(a.identity.path, perso.join("a"));
    assert!(!a.registered);
    assert_eq!(a.last_used, None);

    assert_eq!(sandbox.slugs("/v1/repos?context=work"), ["work/b"]);
    assert_eq!(
        sandbox.slugs("/v1/repos?context=unassigned"),
        Vec::<String>::new()
    );
    assert_eq!(sandbox.slugs("/v1/repos?query=COMPANY"), ["work/b"]);
    assert_eq!(
        sandbox.slugs("/v1/repos?context=&query="),
        ["personal/a", "work/b"]
    );

    for path in [
        "/v1/repos/personal/nope",
        "/v1/repos/personal",
        "/v1/repos/a/b/c",
    ] {
        assert_api_error(
            &sandbox.request("GET", path, None),
            404,
            ErrorCode::NotFound,
        );
    }
    for path in ["/v1/repos?context=Not/A/Context", "/v1/repos?unknown=1"] {
        assert_api_error(
            &sandbox.request("GET", path, None),
            400,
            ErrorCode::InvalidRequest,
        );
    }
    assert_api_error(
        &sandbox.request("GET", "/v1/repos/scan", None),
        405,
        ErrorCode::InvalidRequest,
    );

    // A scan with nothing changed changes nothing.
    let result = sandbox.scan();
    assert_eq!((result.added, result.updated, result.removed), (0, 0, 0));
    assert_eq!(result.errors, []);

    // Deleted while loomd is stopped, the index is rebuilt by the scan on start.
    daemon.stop();
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", sandbox.index().display()));
    }
    let _daemon = sandbox.start();
    assert_eq!(wait_for_repos(&sandbox, 2), ["personal/a", "work/b"]);
}

#[test]
fn register_outside_the_roots_then_forget() {
    let sandbox = Sandbox::new();
    fake_repo(&sandbox.user_home().join("perso/r"), &[]);
    let elsewhere = sandbox.user_home().join("elsewhere/r");
    fake_repo(
        &elsewhere,
        &[("origin", "https://gitlab.com/company/r.git")],
    );
    fs::create_dir_all(elsewhere.join("src/deep")).unwrap();
    let daemon = sandbox.start();
    // Waits for the scan on start.
    sandbox.scan();

    let body = json!({ "path": elsewhere.join("src/deep/") });
    let registered: RepoSummary = ok(&sandbox.request("POST", "/v1/repos/register", Some(&body)));
    assert_eq!(registered.identity.path, elsewhere);
    assert_eq!(registered.identity.slug, "work/r");
    assert!(registered.registered);

    // A scan keeps it; it survives a restart.
    let result = sandbox.scan();
    assert_eq!((result.added, result.updated, result.removed), (0, 0, 0));
    daemon.stop();
    let _daemon = sandbox.start();
    sandbox.scan();
    assert_eq!(sandbox.slugs("/v1/repos"), ["personal/r", "work/r"]);
    let again: RepoSummary = sandbox.get("/v1/repos/work/r");
    assert!(again.registered);

    // Invalid registrations.
    for body in [
        json!({ "path": "relative/path" }),
        json!({ "path": sandbox.user_home().join("elsewhere") }),
        json!({ "path": "/a/../b" }),
        json!({ "path": elsewhere, "extra": true }),
        json!({}),
    ] {
        assert_api_error(
            &sandbox.request("POST", "/v1/repos/register", Some(&body)),
            400,
            ErrorCode::InvalidRequest,
        );
    }

    let forgotten: RepoSummary = ok(&sandbox.request("DELETE", "/v1/repos/work/r", None));
    assert_eq!(forgotten.identity.path, elsewhere);
    assert_api_error(
        &sandbox.request("GET", "/v1/repos/work/r", None),
        404,
        ErrorCode::NotFound,
    );
    assert_api_error(
        &sandbox.request("DELETE", "/v1/repos/work/r", None),
        404,
        ErrorCode::NotFound,
    );
    sandbox.scan();
    assert_eq!(sandbox.slugs("/v1/repos"), ["personal/r"]);

    // A registered repository gone from disk is removed by the next scan.
    let body = json!({ "path": elsewhere });
    ok::<RepoSummary>(&sandbox.request("POST", "/v1/repos/register", Some(&body)));
    fs::remove_dir_all(&elsewhere).unwrap();
    let result = sandbox.scan();
    assert_eq!(result.removed, 1);
    assert_eq!(sandbox.slugs("/v1/repos"), ["personal/r"]);
}

#[test]
fn reads_during_a_scan_see_the_state_before_or_after_it() {
    const BEFORE: usize = 200;
    const AFTER: usize = 600;
    let sandbox = Sandbox::new();
    let perso = sandbox.user_home().join("perso");
    for n in 0..BEFORE {
        fake_repo(&perso.join(format!("g{}/r{n}", n % 10)), &[]);
    }
    let _daemon = sandbox.start();
    sandbox.scan();
    assert_eq!(sandbox.get::<RepoList>("/v1/repos").repos.len(), BEFORE);

    for n in BEFORE..AFTER {
        fake_repo(&perso.join(format!("g{}/r{n}", n % 10)), &[]);
    }
    let socket = sandbox.socket();
    let scans: Vec<_> = (0..2)
        .map(|_| {
            let socket = socket.clone();
            thread::spawn(move || request(&socket, "POST", "/v1/repos/scan", None))
        })
        .collect();
    let mut seen = Vec::new();
    loop {
        seen.push(sandbox.get::<RepoList>("/v1/repos").repos.len());
        if scans.iter().all(thread::JoinHandle::is_finished) {
            break;
        }
    }
    let added: Vec<u64> = scans
        .into_iter()
        .map(|scan| ok::<ScanResult>(&scan.join().unwrap()).added)
        .collect();
    assert!(
        seen.iter().all(|count| *count == BEFORE || *count == AFTER),
        "{seen:?}"
    );
    // Both requests were served by one scan, or the second found nothing new.
    assert!(
        added == [400, 400] || added.iter().sum::<u64>() == 400,
        "{added:?}"
    );
    assert_eq!(sandbox.get::<RepoList>("/v1/repos").repos.len(), AFTER);
}

#[test]
fn index_written_by_a_newer_loom_stops_the_daemon() {
    let sandbox = Sandbox::new();
    drop(loom_store::Store::open(&sandbox.index()).unwrap());
    // `PRAGMA user_version`: 4 bytes, big-endian, at offset 60 of the database header.
    let mut bytes = fs::read(sandbox.index()).unwrap();
    bytes[60..64].copy_from_slice(&99_u32.to_be_bytes());
    fs::write(sandbox.index(), bytes).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loomd"))
        .env("LOOM_HOME", sandbox.loom_home())
        .env("HOME", sandbox.user_home())
        .arg("--color=never")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("schema version 99"), "{stderr}");
    assert!(!sandbox.socket().exists());
}

/// `git <args>` in `dir`, hermetic: the sandbox `HOME`, no global or system configuration, no
/// signing. Its standard output; it must succeed.
fn git(sandbox: &Sandbox, dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .env("HOME", sandbox.user_home())
        .env("XDG_CONFIG_HOME", sandbox.user_home().join(".config"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args([
            "-c",
            "user.name=Loom Test",
            "-c",
            "user.email=loom@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// A repository at `dir` with `files` committed: `f<n>.txt` spread over directories of 100.
fn git_repo(sandbox: &Sandbox, dir: &Path, files: usize) {
    fs::create_dir_all(dir).unwrap();
    git(sandbox, dir, &["init", "-q"]);
    for n in 0..files {
        let sub = dir.join(format!("d{}", n / 100));
        if n % 100 == 0 {
            fs::create_dir_all(&sub).unwrap();
        }
        // One content: one blob to write, whatever the number of files.
        fs::write(sub.join(format!("f{n}.txt")), "file\n").unwrap();
    }
    git(sandbox, dir, &["add", "-A"]);
    git(
        sandbox,
        dir,
        &["commit", "-q", "--allow-empty", "-m", "files"],
    );
}

/// Polls `GET path` until the repository has a state.
fn wait_for_state(sandbox: &Sandbox, path: &str) -> RepoSummary {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let response = sandbox.request("GET", path, None);
        // Not found until the scan on start indexes it.
        if response.status != 404 {
            let repo: RepoSummary = ok(&response);
            if repo.state.is_some() {
                return repo;
            }
        }
        assert!(Instant::now() < deadline, "no state for {path}");
        thread::sleep(POLL);
    }
}

#[test]
fn states_are_read_after_the_scan_on_registration_and_on_demand() {
    let sandbox = Sandbox::new();
    let repo = sandbox.user_home().join("perso/a");
    git_repo(&sandbox, &repo, 3);
    let _daemon = sandbox.start();

    let summary = wait_for_state(&sandbox, "/v1/repos/personal/a");
    let state = summary.state.unwrap();
    assert_eq!(state.error, None, "{state:?}");
    assert_eq!(
        state.head,
        MaybeKnown::Known(RepoHead::Branch {
            name: "main".to_owned()
        })
    );
    assert_eq!(state.dirty, MaybeKnown::Known(0));
    assert_eq!(state.operation, MaybeKnown::Known(None));
    assert_eq!(state.upstream, MaybeKnown::Known(None));
    assert_eq!(
        state.default_branch,
        MaybeKnown::Known(Some("main".to_owned()))
    );
    assert!(matches!(state.last_commit, MaybeKnown::Known(Some(_))));
    // The list carries the states too.
    let list: RepoList = sandbox.get("/v1/repos");
    assert_eq!(list.repos[0].state.as_ref(), Some(&state));

    // A change is seen on demand; the stored state follows.
    fs::write(repo.join("d0/f0.txt"), "changed\n").unwrap();
    fs::write(repo.join("new.txt"), "new\n").unwrap();
    let refreshed: RepoSummary = sandbox.get("/v1/repos/personal/a?refresh=true");
    let refreshed = refreshed.state.unwrap();
    assert_eq!(refreshed.dirty, MaybeKnown::Known(1));
    assert_eq!(
        refreshed.untracked,
        MaybeKnown::Known(Untracked {
            count: 1,
            capped: false
        })
    );
    assert!(refreshed.refreshed_at >= state.refreshed_at);
    let stored: RepoSummary = sandbox.get("/v1/repos/personal/a?refresh=false");
    assert_eq!(stored.state, Some(refreshed));

    for path in [
        "/v1/repos/personal/a?refresh=maybe",
        "/v1/repos/personal/a?other=1",
    ] {
        assert_api_error(
            &sandbox.request("GET", path, None),
            400,
            ErrorCode::InvalidRequest,
        );
    }
    assert_api_error(
        &sandbox.request("GET", "/v1/repos/personal/nope?refresh=true", None),
        404,
        ErrorCode::NotFound,
    );

    // A registered repository gets its state.
    let elsewhere = sandbox.user_home().join("elsewhere/b");
    git_repo(&sandbox, &elsewhere, 1);
    git(&sandbox, &elsewhere, &["checkout", "-q", "-b", "topic"]);
    let body = json!({ "path": elsewhere });
    ok::<RepoSummary>(&sandbox.request("POST", "/v1/repos/register", Some(&body)));
    let registered = wait_for_state(&sandbox, "/v1/repos/unassigned/b");
    assert_eq!(
        registered.state.unwrap().head,
        MaybeKnown::Known(RepoHead::Branch {
            name: "topic".to_owned()
        })
    );

    // A repository that cannot be read has a state made of unknown fields and an error.
    let broken = sandbox.user_home().join("perso/broken");
    fake_repo(&broken, &[]);
    fs::write(broken.join(".git/HEAD"), "garbage\n").unwrap();
    sandbox.scan();
    let broken = sandbox.get::<RepoSummary>("/v1/repos/personal/broken?refresh=true");
    let broken = broken.state.unwrap();
    assert_eq!(broken.head, MaybeKnown::Unknown);
    assert!(broken.error.is_some());
}

/// The state of a repository of 20,000 files is read off the API: listing the index while it
/// is read takes no longer than without it.
#[test]
fn refreshing_a_large_repository_does_not_block_the_api() {
    const FILES: usize = 20_000;
    let sandbox = Sandbox::new();
    let large = sandbox.user_home().join("perso/large");
    let setup = Instant::now();
    git_repo(&sandbox, &large, FILES);
    // Untracked files beyond the cap: the count stops at 1,000.
    fs::create_dir_all(large.join("out")).unwrap();
    for n in 0..1_500 {
        fs::write(large.join(format!("out/u{n}")), "u").unwrap();
    }
    for n in 0..20 {
        fake_repo(&sandbox.user_home().join(format!("perso/small/s{n}")), &[]);
    }
    eprintln!("fixture: {FILES} files in {:?}", setup.elapsed());
    let _daemon = sandbox.start();
    wait_for_state(&sandbox, "/v1/repos/personal/large");

    let socket = sandbox.socket();
    let started = Instant::now();
    let refresh = thread::spawn(move || {
        let response = request(
            &socket,
            "GET",
            "/v1/repos/personal/large?refresh=true",
            None,
        );
        (response, started.elapsed())
    });
    let mut slowest = Duration::ZERO;
    let mut lists = 0;
    while !refresh.is_finished() {
        let listed = Instant::now();
        assert_eq!(sandbox.get::<RepoList>("/v1/repos").repos.len(), 21);
        slowest = slowest.max(listed.elapsed());
        lists += 1;
    }
    let (response, refresh_time) = refresh.join().unwrap();
    let large: RepoSummary = ok(&response);
    let state = large.state.unwrap();
    eprintln!(
        "refresh of {FILES} files: {refresh_time:?}; {lists} lists meanwhile, slowest {slowest:?}"
    );
    assert_eq!(state.dirty, MaybeKnown::Known(0), "{state:?}");
    assert_eq!(
        state.untracked,
        MaybeKnown::Known(Untracked {
            count: 1_000,
            capped: true
        })
    );
    assert!(lists > 0);
    assert!(
        slowest < Duration::from_secs(1),
        "a list took {slowest:?} while the state was read"
    );
}

/// Polls `GET path` until the repository's last update satisfies `done`.
fn wait_for_update(
    sandbox: &Sandbox,
    path: &str,
    done: impl Fn(&[UpdateOutcome]) -> bool,
) -> RepoSummary {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let response = sandbox.request("GET", path, None);
        if response.status != 404 {
            let repo: RepoSummary = ok(&response);
            if repo
                .last_update
                .as_ref()
                .is_some_and(|update| done(&update.outcomes))
            {
                return repo;
            }
        }
        assert!(Instant::now() < deadline, "no update for {path}");
        thread::sleep(POLL);
    }
}

/// A commit of `file` pushed to `main` of the remote `origin` from `seed`; its id.
fn push(sandbox: &Sandbox, seed: &Path, file: &str, content: &str) -> String {
    fs::write(seed.join(file), content).unwrap();
    git(sandbox, seed, &["add", "-A"]);
    git(sandbox, seed, &["commit", "-q", "-m", file]);
    git(sandbox, seed, &["push", "-q", "origin", "HEAD:main"]);
    git(sandbox, seed, &["rev-parse", "HEAD"]).trim().to_owned()
}

#[test]
fn repositories_are_updated_on_start_and_on_request() {
    let sandbox = Sandbox::new();
    let origin = sandbox.dir.path().join("origin.git");
    git(
        &sandbox,
        sandbox.dir.path(),
        &["init", "-q", "--bare", origin.to_str().unwrap()],
    );
    let url = format!("file://{}", origin.display());
    let seed = sandbox.dir.path().join("seed");
    git(
        &sandbox,
        sandbox.dir.path(),
        &["clone", "-q", &url, seed.to_str().unwrap()],
    );
    push(&sandbox, &seed, "f.txt", "one\n");
    let repo = sandbox.user_home().join("perso/app");
    git(
        &sandbox,
        sandbox.dir.path(),
        &["clone", "-q", &url, repo.to_str().unwrap()],
    );
    // Behind when loomd starts: fast-forwarded by the first scheduled update.
    let first = push(&sandbox, &seed, "f.txt", "two\n");
    // Without remote: skipped, no git command.
    git_repo(&sandbox, &sandbox.user_home().join("perso/local"), 1);
    let _daemon = sandbox.start();

    let app = wait_for_update(&sandbox, "/v1/repos/personal/app", |outcomes| {
        matches!(outcomes, [UpdateOutcome::FastForwarded { .. }])
    });
    let update = app.last_update.unwrap();
    assert_eq!(update.policy, RepoUpdatePolicy::Ff);
    assert!(matches!(
        &update.outcomes[0],
        UpdateOutcome::FastForwarded { branch, to, .. } if branch == "main" && *to == first
    ));
    assert_eq!(fs::read_to_string(repo.join("f.txt")).unwrap(), "two\n");
    // The state read after the update is stored with it.
    let state = app.state.unwrap();
    assert_eq!(
        state.upstream.known().flatten().unwrap().behind,
        MaybeKnown::Known(0)
    );
    let local = wait_for_update(&sandbox, "/v1/repos/personal/local", |_| true);
    assert_eq!(
        local.last_update.unwrap().outcomes,
        [UpdateOutcome::Skipped {
            reason: SkipReason::NoRemote
        }]
    );

    // On request: a dry run changes nothing.
    let second = push(&sandbox, &seed, "f.txt", "three\n");
    let body = json!({ "repos": ["personal/app"], "dry_run": true });
    let planned: SyncResult = ok(&sandbox.request("POST", "/v1/repos/sync", Some(&body)));
    assert_eq!(planned.repos.len(), 1);
    assert_eq!(planned.repos[0].slug, "personal/app");
    assert_eq!(
        planned.repos[0].result,
        SyncOutcome::Planned {
            policy: RepoUpdatePolicy::Ff,
            actions: vec![PlannedAction::Fetch, PlannedAction::DecideAfterFetch],
        }
    );
    assert_eq!(fs::read_to_string(repo.join("f.txt")).unwrap(), "two\n");

    // Every repository, sorted by slug; the result is stored before the answer.
    let synced: SyncResult = ok(&sandbox.request("POST", "/v1/repos/sync", Some(&json!({}))));
    let slugs: Vec<&str> = synced.repos.iter().map(|repo| repo.slug.as_str()).collect();
    assert_eq!(slugs, ["personal/app", "personal/local"]);
    let SyncOutcome::Updated { update } = &synced.repos[0].result else {
        panic!("{:?}", synced.repos[0]);
    };
    assert!(matches!(
        update.outcomes.as_slice(),
        [UpdateOutcome::FastForwarded { to, .. }] if *to == second
    ));
    assert_eq!(fs::read_to_string(repo.join("f.txt")).unwrap(), "three\n");
    let stored: RepoSummary = sandbox.get("/v1/repos/personal/app");
    assert_eq!(stored.last_update.as_ref(), Some(update));

    // Errors.
    assert_api_error(
        &sandbox.request(
            "POST",
            "/v1/repos/sync",
            Some(&json!({ "repos": ["personal/nope"] })),
        ),
        404,
        ErrorCode::NotFound,
    );
    assert_api_error(
        &sandbox.request("POST", "/v1/repos/sync", Some(&json!({ "dryrun": true }))),
        400,
        ErrorCode::InvalidRequest,
    );
}
