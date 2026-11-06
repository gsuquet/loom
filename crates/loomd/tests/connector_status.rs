//! Connector status end to end: a real `loomd` polling a fake GitLab, read through
//! `GET /v1/connectors`, `state/connectors.json` and `GET /v1/events`.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use loom_api::{
    ConnectorFailure, ConnectorHealth, ConnectorKind, ConnectorList, ConnectorStatus, PolledBy,
    ReviewSyncResult, Secret,
};
use serde_json::{Value, json};
use tempfile::TempDir;

mod common;
#[path = "../../loom-forge/tests/common/fake_forge.rs"]
mod fake_forge;

use common::{Daemon, TIMEOUT, ok, request};
use fake_forge::{FakeForge, Route};

const TOKEN: &str = "fake-status-token-0123";
const HOST: &str = "gitlab.example.com";
const REVIEWS: &str = "/merge_requests?scope=reviews_for_me&state=opened&per_page=100";
const ASSIGNED: &str = "/merge_requests?scope=assigned_to_me&state=opened&per_page=100";
const OWN: &str = "/merge_requests?scope=created_by_me&state=opened&per_page=100";
const MENTIONED: &str = "/todos?type=MergeRequest&state=pending&action=mentioned&per_page=100";
const ADDRESSED: &str =
    "/todos?type=MergeRequest&state=pending&action=directly_addressed&per_page=100";
const EMPTY_MRS: &str = "gitlab/merge-requests-reviews-for-me.empty.json";
const EMPTY_TODOS: &str = "gitlab/todos.empty.json";

fn config(machines: &str) -> String {
    format!(
        r#"
[repos]
roots = []

[contexts.work]
{machines}
match.hosts = ["{HOST}/company-a"]

[contexts.home]

[forges."{HOST}"]
kind = "gitlab"
token_env = "LOOM_STATUS_TEST_TOKEN"
"#
    )
}

struct Sandbox {
    dir: TempDir,
    forge: FakeForge,
    /// `LOOM_TEST_REVIEW_START_DELAY`.
    start_delay: &'static str,
    token: bool,
}

impl Sandbox {
    fn with_config(config: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("lcs")
            .tempdir_in("/tmp")
            .unwrap();
        let sandbox = Self {
            dir,
            forge: FakeForge::start(),
            start_delay: "1h",
            token: true,
        };
        fs::create_dir_all(sandbox.loom_home()).unwrap();
        fs::create_dir_all(sandbox.dir.path().join("u")).unwrap();
        fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
        fs::write(
            sandbox.loom_home().join("machine.toml"),
            "id = \"laptop\"\n",
        )
        .unwrap();
        sandbox
    }

    fn new() -> Self {
        Self::with_config(&config(""))
    }

    /// The routes of a poll, after `first`: the first route that matches answers.
    fn routes(&self, first: Vec<Route>) {
        for route in first {
            self.forge.route(route);
        }
        self.forge
            .route(Route::get("/user").fixture("gitlab/user.json"))
            .route(Route::get(REVIEWS).fixture(EMPTY_MRS))
            .route(Route::get(ASSIGNED).fixture(EMPTY_MRS))
            .route(Route::get(OWN).fixture(EMPTY_MRS))
            .route(Route::get(MENTIONED).fixture(EMPTY_TODOS))
            .route(Route::get(ADDRESSED).fixture(EMPTY_TODOS));
    }

    fn loom_home(&self) -> PathBuf {
        self.dir.path().join("l")
    }

    fn state(&self) -> PathBuf {
        self.loom_home().join("state/connectors.json")
    }

    fn socket(&self) -> PathBuf {
        self.loom_home().join("run/loomd.sock")
    }

    fn start(&self) -> Daemon {
        let home = self.dir.path().join("u");
        let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
        command
            .env("LOOM_HOME", self.loom_home())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("LOOM_TEST_FORGE_API", self.forge.base())
            .env("LOOM_TEST_REVIEW_START_DELAY", self.start_delay)
            .arg("--color=never")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if self.token {
            command.env("LOOM_STATUS_TEST_TOKEN", TOKEN);
        }
        Daemon::start(&mut command, &self.socket())
    }

    fn list(&self, query: &str) -> ConnectorList {
        ok(&request(
            &self.socket(),
            "GET",
            &format!("/v1/connectors{query}"),
            None,
        ))
    }

    /// The status of the forge in the context `work`.
    fn work(&self) -> ConnectorStatus {
        self.list("?context=work").connectors.remove(0)
    }

    fn sync(&self) -> ReviewSyncResult {
        ok(&request(
            &self.socket(),
            "POST",
            "/v1/reviews/sync",
            Some(&json!({ "context": "work" })),
        ))
    }
}

fn saved(sandbox: &Sandbox) -> Value {
    serde_json::from_slice(&fs::read(sandbox.state()).unwrap()).unwrap()
}

fn saved_work(sandbox: &Sandbox) -> Value {
    saved(sandbox)["connectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["context"] == "work")
        .unwrap()
        .clone()
}

#[test]
fn before_a_poll_every_credential_is_listed_with_its_kind_and_next_poll() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let list = sandbox.list("");
    let names: Vec<(&str, &str)> = list
        .connectors
        .iter()
        .map(|status| (status.context.as_str(), status.name.as_str()))
        .collect();
    assert_eq!(names, [("home", HOST), ("work", HOST)]);
    let status = &list.connectors[1];
    assert_eq!(status.kind, ConnectorKind::Gitlab);
    assert_eq!(status.host, HOST);
    assert_eq!(status.polled_by, PolledBy::EveryMachine);
    assert_eq!(status.health, ConnectorHealth::NotPolledYet);
    assert!(status.stale);
    assert!(status.next_poll.is_some());
    assert!(!status.capabilities.pool);
    assert!(!sandbox.state().exists());
    daemon.stop();
}

#[test]
fn a_query_filters_by_context_and_an_unknown_parameter_is_refused() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    assert_eq!(sandbox.list("?context=home").connectors.len(), 1);
    assert_eq!(sandbox.list("?context=none").connectors.len(), 0);
    let refused = request(&sandbox.socket(), "GET", "/v1/connectors?x=1", None);
    assert_eq!(refused.status, 400, "{}", refused.body);
    daemon.stop();
}

#[test]
fn a_poll_that_succeeds_is_ok_and_writes_its_times_with_mode_0600() {
    let sandbox = Sandbox::new();
    sandbox.routes(vec![]);
    let daemon = sandbox.start();
    sandbox.sync();
    let status = sandbox.work();
    assert_eq!(status.health, ConnectorHealth::Ok);
    assert!(!status.stale);
    assert!(status.last_attempt.is_some());
    assert_eq!(status.last_attempt, status.last_success);
    assert!(status.next_poll.is_some());
    assert_eq!(
        saved_work(&sandbox),
        json!({
            "context": "work",
            "name": HOST,
            "last_attempt": status.last_attempt,
            "last_success": status.last_success,
        })
    );
    assert_eq!(
        fs::metadata(sandbox.state()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    daemon.stop();
}

/// The acceptance of C1: a scheduled poll logs `failed_lists` and used to drop it.
#[test]
fn a_scheduled_poll_keeps_a_refused_list_as_forbidden() {
    let mut sandbox = Sandbox::new();
    sandbox.start_delay = "1s";
    sandbox.routes(vec![
        Route::get(MENTIONED)
            .status(403)
            .body(r#"{"message":"403 Forbidden"}"#),
        Route::get(ASSIGNED).status(500).body("{}"),
    ]);
    let daemon = sandbox.start();
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        let status = sandbox.work();
        if status.health != ConnectorHealth::NotPolledYet {
            break status;
        }
        assert!(Instant::now() < deadline, "no poll ran");
        thread::sleep(Duration::from_millis(20));
    };
    // No `POST /v1/reviews/sync` ran: only a scheduled poll can tell this.
    let ConnectorHealth::Forbidden { error, failed } = &status.health else {
        panic!("{status:?}");
    };
    assert!(error.starts_with("list:mentioned: "), "{error}");
    let keys: Vec<&str> = failed
        .iter()
        .map(|f: &ConnectorFailure| f.key.as_str())
        .collect();
    assert_eq!(keys, ["list:assigned"]);
    assert_eq!(status.items.failed, 2);
    // The poll went on.
    assert!(status.last_success.is_some());
    daemon.stop();
}

#[test]
fn a_refused_token_is_credential_never_shows_the_token_and_heals() {
    let sandbox = Sandbox::new();
    sandbox.routes(vec![
        Route::get("/user")
            .status(401)
            .body(format!(r#"{{"message":"401 for {TOKEN}"}}"#))
            .times(1),
    ]);
    let daemon = sandbox.start();
    sandbox.sync();
    let status = sandbox.work();
    let ConnectorHealth::Credential { which, error } = &status.health else {
        panic!("{status:?}");
    };
    assert_eq!(*which, Secret::Token);
    assert!(!error.contains(TOKEN), "{error}");
    assert!(status.last_attempt.is_some());
    assert_eq!(status.last_success, None);
    assert!(status.stale);
    let body = request(&sandbox.socket(), "GET", "/v1/connectors", None).body;
    assert!(!body.contains(TOKEN), "{body}");
    assert!(!fs::read_to_string(sandbox.state()).unwrap().contains(TOKEN));

    // The next poll gets the token again, and the forge accepts it.
    sandbox.sync();
    let status = sandbox.work();
    assert_eq!(status.health, ConnectorHealth::Ok);
    assert!(status.last_success.is_some());
    daemon.stop();
}

#[test]
fn a_token_that_does_not_resolve_is_credential() {
    let mut sandbox = Sandbox::new();
    sandbox.token = false;
    let daemon = sandbox.start();
    sandbox.sync();
    let status = sandbox.work();
    assert!(
        matches!(
            status.health,
            ConnectorHealth::Credential {
                which: Secret::Token,
                ..
            }
        ),
        "{status:?}"
    );
    assert_eq!(sandbox.forge.requests(), []);
    // No call to the forge: this poll is no attempt, and writes nothing.
    assert_eq!(status.last_attempt, None);
    assert!(!sandbox.state().exists());
    daemon.stop();
}

#[test]
fn a_refused_request_that_stops_the_poll_is_forbidden() {
    let sandbox = Sandbox::new();
    sandbox.routes(vec![
        Route::get(REVIEWS)
            .status(403)
            .body(r#"{"message":"403 Forbidden"}"#),
    ]);
    let daemon = sandbox.start();
    sandbox.sync();
    let status = sandbox.work();
    assert!(
        matches!(status.health, ConnectorHealth::Forbidden { .. }),
        "{status:?}"
    );
    assert_eq!(status.last_success, None);
    daemon.stop();
}

#[test]
fn a_failing_host_counts_its_failures_and_keeps_the_counts_and_last_success() {
    let sandbox = Sandbox::new();
    sandbox
        .forge
        .route(Route::get("/user").fixture("gitlab/user.json"));
    sandbox
        .forge
        .route(Route::get(REVIEWS).fixture(EMPTY_MRS).times(1))
        .route(Route::get(ASSIGNED).fixture(EMPTY_MRS).times(1))
        .route(Route::get(OWN).fixture(EMPTY_MRS).times(1))
        .route(Route::get(MENTIONED).fixture(EMPTY_TODOS).times(1))
        .route(Route::get(ADDRESSED).fixture(EMPTY_TODOS).times(1));
    let daemon = sandbox.start();
    sandbox.sync();
    let good = sandbox.work();
    assert_eq!(good.health, ConnectorHealth::Ok);

    // Nothing answers the next `reviews_for_me` list with `updated_after`: 404 stops the poll.
    sandbox.sync();
    let status = sandbox.work();
    assert!(
        matches!(
            status.health,
            ConnectorHealth::Unreachable { failures: 1, .. }
        ),
        "{status:?}"
    );
    assert_eq!(status.last_success, good.last_success);
    assert_eq!(status.items, good.items);
    sandbox.sync();
    assert!(matches!(
        sandbox.work().health,
        ConnectorHealth::Unreachable { failures: 2, .. }
    ));
    daemon.stop();
}

#[test]
fn a_rate_limit_is_rate_limited_and_a_skipped_poll_is_no_attempt() {
    let sandbox = Sandbox::new();
    sandbox.routes(vec![
        Route::get("/user")
            .status(429)
            .header("retry-after", "3600")
            .body("{}"),
    ]);
    let daemon = sandbox.start();
    sandbox.sync();
    let status = sandbox.work();
    assert!(
        matches!(status.health, ConnectorHealth::RateLimited { .. }),
        "{status:?}"
    );
    let bytes = fs::read(sandbox.state()).unwrap();
    assert!(status.last_attempt.is_some());
    let calls = sandbox.forge.requests().len();

    thread::sleep(Duration::from_millis(1100));
    sandbox.sync();
    let after = sandbox.work();
    assert_eq!(sandbox.forge.requests().len(), calls, "no forge call");
    assert_eq!(after.last_attempt, status.last_attempt);
    assert!(matches!(after.health, ConnectorHealth::RateLimited { .. }));
    assert_eq!(fs::read(sandbox.state()).unwrap(), bytes);
    daemon.stop();
}

#[test]
fn a_restart_keeps_the_times_and_forgets_the_health() {
    let sandbox = Sandbox::new();
    sandbox.routes(vec![]);
    let daemon = sandbox.start();
    sandbox.sync();
    let before = sandbox.work();
    assert_eq!(before.health, ConnectorHealth::Ok);
    daemon.stop();

    let daemon = sandbox.start();
    let after = sandbox.work();
    assert_eq!(after.health, ConnectorHealth::NotPolledYet);
    assert_eq!(after.last_success, before.last_success);
    assert_eq!(after.last_attempt, before.last_attempt);
    assert!(!after.stale);
    daemon.stop();
}

#[test]
fn another_machine_polls_a_context_and_this_one_makes_no_claim() {
    let sandbox = Sandbox::with_config(&config(r#"machines = ["tower", "laptop"]"#));
    let daemon = sandbox.start();
    let status = sandbox.work();
    assert_eq!(
        status.polled_by,
        PolledBy::Other {
            machine: "tower".to_owned()
        }
    );
    assert_eq!(status.health, ConnectorHealth::NotPolledYet);
    assert!(!status.stale);
    assert_eq!(status.next_poll, None);
    assert_eq!(sandbox.sync().forges, []);
    daemon.stop();
}

#[test]
fn a_context_this_machine_does_not_hold_is_listed_too() {
    let sandbox = Sandbox::with_config(&config(r#"machines = ["tower"]"#));
    let daemon = sandbox.start();
    assert_eq!(
        sandbox.work().polled_by,
        PolledBy::Other {
            machine: "tower".to_owned()
        }
    );
    daemon.stop();
}

#[test]
fn the_first_of_machines_polls_here() {
    let sandbox = Sandbox::with_config(&config(r#"machines = ["laptop"]"#));
    sandbox.routes(vec![]);
    let daemon = sandbox.start();
    assert_eq!(sandbox.work().polled_by, PolledBy::ThisMachine);
    sandbox.sync();
    assert_eq!(sandbox.work().health, ConnectorHealth::Ok);
    daemon.stop();
}

#[test]
fn a_change_sends_the_event_to_a_client_of_the_context() {
    let sandbox = Sandbox::new();
    sandbox.routes(vec![]);
    let daemon = sandbox.start();
    let mut stream = UnixStream::connect(sandbox.socket()).unwrap();
    write!(
        stream,
        "GET /v1/events?context=work HTTP/1.1\r\nhost: loomd\r\naccept: text/event-stream\r\n\r\n"
    )
    .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    // The server sends its headers when the stream is open: then a change cannot be missed.
    let mut received = String::new();
    let mut buffer = [0_u8; 4096];
    let deadline = Instant::now() + TIMEOUT;
    while !received.contains("\r\n\r\n") {
        assert!(Instant::now() < deadline, "the stream did not open");
        if let Ok(read) = stream.read(&mut buffer) {
            received.push_str(&String::from_utf8_lossy(&buffer[..read]));
        }
    }

    sandbox.sync();
    while !received.contains("connector.changed") {
        assert!(Instant::now() < deadline, "no event: {received}");
        if let Ok(read) = stream.read(&mut buffer) {
            received.push_str(&String::from_utf8_lossy(&buffer[..read]));
        }
    }
    assert!(
        received
            .contains(r#""type":"connector.changed","context":"work","name":"gitlab.example.com""#),
        "{received}"
    );
    daemon.stop();
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A slow poll tells the time apart: its start is what counts, and the wait starts at its end.
#[test]
fn the_times_are_the_start_of_the_poll_and_the_next_poll_after_the_interval() {
    let sandbox = Sandbox::new();
    sandbox.routes(vec![
        Route::get("/user")
            .fixture("gitlab/user.json")
            .delay(Duration::from_millis(2100)),
    ]);
    let daemon = sandbox.start();
    let before = unix_now();
    sandbox.sync();
    let done = unix_now();
    assert!(done >= before + 2);
    let status = sandbox.work();
    assert_eq!(status.health, ConnectorHealth::Ok);
    let attempt = status.last_attempt.unwrap().unix_secs();
    assert!(attempt <= before + 1, "{attempt} after {before}");
    assert_eq!(status.last_success, status.last_attempt);
    // 5 minutes, ±10 %.
    let next = status.next_poll.unwrap().unix_secs();
    assert!(
        next >= done - 1 + 270 && next <= done + 330,
        "{next} after {done}"
    );
    daemon.stop();
}

/// A poll that the stop of loomd cuts says nothing of the host.
#[test]
fn a_poll_cut_by_the_stop_of_loomd_reports_nothing() {
    let mut sandbox = Sandbox::new();
    sandbox.start_delay = "1s";
    // No other route: when the slow call ends, the poll fails.
    sandbox.forge.route(
        Route::get("/user")
            .fixture("gitlab/user.json")
            .delay(Duration::from_millis(1500)),
    );
    let daemon = sandbox.start();
    let deadline = Instant::now() + TIMEOUT;
    while sandbox.forge.requests().is_empty() {
        assert!(Instant::now() < deadline, "no poll ran");
        thread::sleep(Duration::from_millis(5));
    }
    daemon.stop();
    assert!(!sandbox.state().exists());
}
