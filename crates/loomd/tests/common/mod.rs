//! Helpers shared by the integration tests of `loomd`: hermetic git commands, a running daemon
//! and a minimal HTTP client over its unix socket, and snapshots of a repository
//! ([`snapshot`]).

#![allow(
    dead_code,
    clippy::unwrap_used,
    reason = "each test crate uses part of the helpers; a failing helper must fail the test"
)]

pub mod snapshot;

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::Value;

/// Longest wait for the daemon to start, stop or reach a state.
pub const TIMEOUT: Duration = Duration::from_secs(20);
/// Delay between two polls.
pub const POLL: Duration = Duration::from_millis(20);
/// Longest wait for an answer: a sync answers after every update it runs.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// `git` in `dir`, hermetic: `HOME` is `home`, no global or system configuration, no signing,
/// no editor, no inherited repository or SSH command, and the `file://` transport allowed for
/// submodules.
pub fn git_command(home: &Path, dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_SSH_COMMAND")
        .env_remove("GIT_SSH")
        .args([
            "-c",
            "user.name=Loom Test",
            "-c",
            "user.email=loom@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "advice.detachedHead=false",
            "-c",
            "protocol.file.allow=always",
        ])
        .stdin(Stdio::null());
    command
}

/// `git <args>` in `dir` (see [`git_command`]), whatever its exit status.
pub fn git_run(home: &Path, dir: &Path, args: &[&str]) -> Output {
    git_command(home, dir).args(args).output().unwrap()
}

/// `git <args>` in `dir` (see [`git_command`]), which must succeed; its standard output.
pub fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
    let output = git_run(home, dir, args);
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// A running daemon, killed when dropped.
pub struct Daemon {
    child: Child,
}

impl Daemon {
    /// Spawns `command` and waits for `socket` to accept connections.
    pub fn start(command: &mut Command, socket: &Path) -> Self {
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
    pub fn stop(mut self) {
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

pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// Minimal HTTP/1.1 client: one request, `Connection: close`, body read to the end.
pub fn request(socket: &Path, method: &str, path: &str, body: Option<&Value>) -> HttpResponse {
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(REQUEST_TIMEOUT)).unwrap();
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
pub fn ok<T: DeserializeOwned + schemars::JsonSchema>(response: &HttpResponse) -> T {
    assert_eq!(response.status, 200, "{}", response.body);
    let value: Value = serde_json::from_str(&response.body).unwrap();
    let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    assert!(jsonschema::is_valid(&schema, &value), "{value}");
    serde_json::from_value(value).unwrap()
}
