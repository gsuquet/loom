//! `loom review` end to end: the `loomd` built next to `loom`, and the fake forge in place of the
//! GitHub API (`LOOM_TEST_FORGE_API`, debug builds).

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{
    ConnectorHealth, ConnectorKind, ConnectorList, ReviewQueue, ReviewStartResult, Status,
};
use tempfile::TempDir;

#[path = "../../loom-forge/tests/common/fake_forge.rs"]
mod fake_forge;
mod isolation;

use fake_forge::{FakeForge, Route};

const TOKEN_ENV: &str = "LOOM_REVIEW_TEST_TOKEN";
const SEARCH: &str =
    "/search/issues?q=is:pr+is:open+archived:false+review-requested:@me&per_page=100";
const PULL: &str = "/repos/acme/shop/pulls/412";
const EMPTY: &str = "The queue is empty. Run `loom review sync` to poll the forges now.\n";
const CONFIG: &str = r#"
[repos]
roots = []

[contexts.personal]
match.hosts = ["github.com/acme"]

[forges."github.com"]
kind = "github"
token_env = "LOOM_REVIEW_TEST_TOKEN"
"#;

struct Sandbox {
    dir: TempDir,
    forge: FakeForge,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("lrv")
            .tempdir_in("/tmp")
            .unwrap();
        let forge = FakeForge::start();
        forge
            .route(Route::get("/user").fixture("github/user.json"))
            .route(Route::get(SEARCH).fixture("github/search-review-requested.json"))
            .route(Route::get(PULL).fixture("github/pull-412.v1.json"))
            .route(Route::get(&format!("{PULL}/reviews?per_page=100")).body("[]"))
            .route(
                Route::get("/repos/acme/shop/issues/412/comments?per_page=100")
                    .fixture("github/issue-412-comments.json"),
            )
            .route(Route::post("/graphql").fixture("github/graphql-review-threads.json"));
        let sandbox = Self { dir, forge };
        fs::create_dir_all(sandbox.loom_home()).unwrap();
        fs::create_dir_all(sandbox.home()).unwrap();
        fs::write(sandbox.loom_home().join("config.toml"), CONFIG).unwrap();
        sandbox
    }

    fn loom_home(&self) -> PathBuf {
        self.dir.path().join("l")
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("u")
    }

    fn loom(&self, args: &[&str]) -> Output {
        Command::new(assert_cmd::cargo::cargo_bin!("loom"))
            .env("LOOM_HOME", self.loom_home())
            .envs(isolation::isolated_env(&self.home()))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LOOM_TEST_FORGE_API", self.forge.base())
            .env("LOOM_TEST_REVIEW_START_DELAY", "1h")
            .env(TOKEN_ENV, "fake-token-0123456789")
            .current_dir(self.home())
            .arg("--color=never")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.loom(args);
        assert!(
            output.status.success(),
            "loom {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

/// Stops the loomd of the sandbox, if one runs, before the directory is removed.
impl Drop for Sandbox {
    fn drop(&mut self) {
        stop_loomd(&self.loom_home());
    }
}

fn stop_loomd(home: &Path) {
    let lock = home.join("run/loomd.lock");
    let Ok(file) = File::open(&lock) else {
        return;
    };
    if file.try_lock().is_ok() {
        return;
    }
    if let Some(pid) = fs::read_to_string(&lock)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
    {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while file.try_lock().is_err() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn queue_then_start_creates_one_active_task_and_empties_the_queue() {
    let sandbox = Sandbox::new();
    assert_eq!(sandbox.ok(&["review", "queue"]), EMPTY);

    let synced = sandbox.ok(&["review", "sync"]);
    assert!(synced.contains("1 added to the queue"), "{synced}");

    let text = sandbox.ok(&["review", "queue"]);
    assert!(
        text.starts_with("personal acme/shop#412 [review_requested] Auth refactor"),
        "{text}"
    );
    let queue: ReviewQueue =
        serde_json::from_str(&sandbox.ok(&["review", "queue", "--json"])).unwrap();
    assert_eq!(queue.entries.len(), 1);
    let other: ReviewQueue =
        serde_json::from_str(&sandbox.ok(&["-c", "personal", "review", "queue", "--json"]))
            .unwrap();
    assert_eq!(other, queue);

    // The key holds `/` and `#`.
    let started: ReviewStartResult =
        serde_json::from_str(&sandbox.ok(&["review", "start", "acme/shop#412", "--json"])).unwrap();
    assert!(started.created);
    let task = started.task.summary.task.as_ref().unwrap();
    assert_eq!(task.status, Status::Active);
    assert_eq!(sandbox.ok(&["review", "queue"]), EMPTY);

    // A second start names the same task.
    let id = started.task.summary.id.to_string();
    assert_eq!(
        sandbox.ok(&["review", "start", "acme/shop#412"]),
        format!("Review task {id} already exists (active): Review acme/shop#412: Auth refactor\n")
    );

    let missing = sandbox.loom(&["review", "start", "acme/shop#9"]);
    assert!(!missing.status.success());
    let error = String::from_utf8_lossy(&missing.stderr);
    assert!(error.contains("no queue entry"), "{error}");
}

#[test]
fn connector_status_tells_the_forge_before_and_after_a_poll() {
    let sandbox = Sandbox::new();
    let before = sandbox.ok(&["connector", "status"]);
    assert!(
        before.starts_with("personal github.com: not polled yet, stale, 0 polled, next poll "),
        "{before}"
    );

    sandbox.ok(&["review", "sync"]);
    let after = sandbox.ok(&["connector", "status"]);
    assert!(
        after.starts_with("personal github.com: ok, 1 polled, next poll "),
        "{after}"
    );
    assert!(after.contains(", last success "), "{after}");

    let list: ConnectorList =
        serde_json::from_str(&sandbox.ok(&["connector", "status", "--json"])).unwrap();
    assert_eq!(list.connectors.len(), 1);
    assert_eq!(list.connectors[0].health, ConnectorHealth::Ok);
    assert_eq!(list.connectors[0].kind, ConnectorKind::Github);
    let same: ConnectorList =
        serde_json::from_str(&sandbox.ok(&["-c", "personal", "connector", "status", "--json"]))
            .unwrap();
    assert_eq!(same.connectors.len(), 1);
}

#[test]
fn connector_status_with_no_credential_tells_what_to_do() {
    let sandbox = Sandbox::new();
    fs::write(
        sandbox.loom_home().join("config.toml"),
        "[repos]\nroots = []\n\n[contexts.personal]\n",
    )
    .unwrap();
    let text = sandbox.ok(&["connector", "status"]);
    assert!(text.starts_with("No connector: "), "{text}");
}

#[test]
fn connector_status_of_an_unknown_context_says_so() {
    let sandbox = Sandbox::new();
    let text = sandbox.ok(&["-c", "typo", "connector", "status"]);
    assert!(
        text.starts_with("No connector in the context `typo`: "),
        "{text}"
    );
}
