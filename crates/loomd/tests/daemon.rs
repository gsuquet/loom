//! `loomd` end to end: started in a temporary `LOOM_HOME`, queried over its unix socket.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{ApiError, ApiVersion, ErrorCode, StatusResponse, VersionInfo};
use schemars::schema_for;
use serde_json::Value;
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(20);

/// A loom home under `/tmp`: the macOS default `$TMPDIR` is too long for a socket path.
fn short_home() -> TempDir {
    tempfile::Builder::new()
        .prefix("loomd-")
        .tempdir_in("/tmp")
        .unwrap()
}

fn socket(home: &Path) -> PathBuf {
    home.join("run/loomd.sock")
}

fn lock(home: &Path) -> PathBuf {
    home.join("run/loomd.lock")
}

/// `loomd` with `LOOM_HOME` set to `home`, without color and with a clean environment.
/// `HOME` is `home` too, so that the repository scan never walks the real home directory.
fn loomd(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
    command
        .env("LOOM_HOME", home)
        .env("HOME", home)
        .env_remove("NO_COLOR")
        .arg("--color=never")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

/// A running daemon, killed when dropped.
struct Daemon {
    child: Child,
}

impl Daemon {
    /// Spawns `command` and waits until its socket accepts connections.
    fn start(command: &mut Command, socket: &Path) -> Self {
        let mut daemon = Self {
            child: command.spawn().unwrap(),
        };
        let deadline = Instant::now() + TIMEOUT;
        while UnixStream::connect(socket).is_err() {
            if let Some(status) = daemon.child.try_wait().unwrap() {
                panic!("loomd exited with {status}: {}", daemon.stderr());
            }
            assert!(
                Instant::now() < deadline,
                "socket {} never appeared",
                socket.display()
            );
            thread::sleep(POLL);
        }
        daemon
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .args([format!("-{name}"), self.pid().to_string()])
            .status()
            .unwrap();
        assert!(status.success(), "kill -{name} failed");
    }

    fn wait(&mut self) -> ExitStatus {
        wait(&mut self.child)
    }

    fn stderr(&mut self) -> String {
        let mut text = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            stderr.read_to_string(&mut text).unwrap();
        }
        text
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Already exited when the test waited for it; errors are irrelevant then.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Waits for `child` to exit, killing it after the timeout.
fn wait(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("loomd did not exit in time");
        }
        thread::sleep(POLL);
    }
}

/// Runs `command` to completion: exit code and stderr.
fn run(command: &mut Command) -> (Option<i32>, String) {
    let mut child = command.spawn().unwrap();
    let status = wait(&mut child);
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    (status.code(), stderr)
}

struct HttpResponse {
    status: u16,
    content_type: Option<String>,
    body: String,
}

/// Minimal HTTP/1.1 client: one request, `Connection: close`, body read to the end.
fn request(socket: &Path, method: &str, path: &str) -> HttpResponse {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: loomd\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();

    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let mut lines = head.lines();
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let content_type = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-type")
            .then(|| value.trim().to_owned())
    });
    HttpResponse {
        status,
        content_type,
        body: body.to_owned(),
    }
}

fn assert_api_error(response: &HttpResponse, status: u16, code: ErrorCode) {
    assert_eq!(response.status, status, "{}", response.body);
    assert_eq!(response.content_type.as_deref(), Some("application/json"));
    let value: Value = serde_json::from_str(&response.body).unwrap();
    let schema = serde_json::to_value(schema_for!(ApiError)).unwrap();
    assert!(jsonschema::is_valid(&schema, &value), "{value}");
    let error: ApiError = serde_json::from_value(value).unwrap();
    assert_eq!(error.code, code);
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn status_returns_the_daemon_state() {
    let home = short_home();
    let daemon = Daemon::start(&mut loomd(home.path()), &socket(home.path()));

    let response = request(&socket(home.path()), "GET", "/v1/status");
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(response.content_type.as_deref(), Some("application/json"));
    let value: Value = serde_json::from_str(&response.body).unwrap();
    let schema = serde_json::to_value(schema_for!(StatusResponse)).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&value), "{value}");

    let status: StatusResponse = serde_json::from_value(value).unwrap();
    assert_eq!(status.api_version, ApiVersion::V1);
    assert_eq!(status.daemon, VersionInfo::current());
    assert_eq!(status.pid, daemon.pid());
    assert!(
        status.uptime_secs < TIMEOUT.as_secs(),
        "{}",
        status.uptime_secs
    );
    assert_eq!(status.loom_home, home.path());
    assert_eq!(status.socket_path, socket(home.path()));
    assert_eq!(status.config_path, home.path().join("config.toml"));
}

/// Guard: a test-started loomd scans under the test's home, never under the real one.
#[test]
fn scan_roots_are_under_the_test_home() {
    let home = short_home();
    let mut daemon = Daemon::start(&mut loomd(home.path()), &socket(home.path()));
    daemon.signal("TERM");
    daemon.wait();
    let stderr = daemon.stderr();
    let line = stderr
        .lines()
        .find(|line| line.contains("repository index started"))
        .unwrap_or_else(|| panic!("{stderr}"));
    let (_, rest) = line.split_once("roots=[").unwrap();
    let (list, _) = rest.split_once(']').unwrap();
    let roots: Vec<PathBuf> = list
        .split(", ")
        .map(|item| PathBuf::from(item.trim_matches('"')))
        .collect();
    assert!(!roots.is_empty(), "{line}");
    let real_home = std::env::var_os("HOME").map(PathBuf::from);
    for root in &roots {
        assert!(root.starts_with(home.path()), "{}", root.display());
        if let Some(real_home) = &real_home {
            assert!(
                !root.starts_with(real_home) || home.path().starts_with(real_home),
                "scan root {} is under the real home",
                root.display()
            );
        }
    }
}

#[test]
fn loom_home_flag_overrides_the_environment() {
    let home = short_home();
    let other = short_home();
    let _daemon = Daemon::start(
        loomd(other.path()).arg("--loom-home").arg(home.path()),
        &socket(home.path()),
    );
    let response = request(&socket(home.path()), "GET", "/v1/status");
    let status: StatusResponse = serde_json::from_str(&response.body).unwrap();
    assert_eq!(status.loom_home, home.path());
    assert!(!other.path().join("run").exists());
}

#[test]
fn unknown_path_is_not_found() {
    let home = short_home();
    let _daemon = Daemon::start(&mut loomd(home.path()), &socket(home.path()));
    for path in ["/", "/v1/nope", "/v2/status"] {
        let response = request(&socket(home.path()), "GET", path);
        assert_api_error(&response, 404, ErrorCode::NotFound);
    }
}

#[test]
fn wrong_method_is_an_invalid_request() {
    let home = short_home();
    let _daemon = Daemon::start(&mut loomd(home.path()), &socket(home.path()));
    for method in ["POST", "PUT", "DELETE"] {
        let response = request(&socket(home.path()), method, "/v1/status");
        assert_api_error(&response, 405, ErrorCode::InvalidRequest);
    }
}

#[test]
fn run_files_are_private() {
    let home = short_home();
    let _daemon = Daemon::start(&mut loomd(home.path()), &socket(home.path()));
    assert_eq!(mode(&home.path().join("run")), 0o700);
    assert_eq!(mode(&socket(home.path())), 0o600);
    assert_eq!(mode(&lock(home.path())), 0o600);
}

#[test]
fn second_instance_exits_with_an_error_while_the_first_runs() {
    let home = short_home();
    let first = Daemon::start(&mut loomd(home.path()), &socket(home.path()));

    let (code, stderr) = run(&mut loomd(home.path()));
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("another loomd is running"), "{stderr}");
    assert!(stderr.contains(&format!("PID {}", first.pid())), "{stderr}");
    assert!(
        stderr.contains(&*lock(home.path()).to_string_lossy()),
        "{stderr}"
    );

    // The first one still serves.
    assert_eq!(
        request(&socket(home.path()), "GET", "/v1/status").status,
        200
    );
}

#[test]
fn stale_socket_from_a_killed_daemon_does_not_prevent_start() {
    let home = short_home();
    // SIGKILL: no cleanup, the socket file stays; the kernel releases the lock.
    let mut killed = Daemon::start(&mut loomd(home.path()), &socket(home.path()));
    killed.signal("KILL");
    killed.wait();
    assert!(socket(home.path()).exists());

    let daemon = Daemon::start(&mut loomd(home.path()), &socket(home.path()));
    let response = request(&socket(home.path()), "GET", "/v1/status");
    let status: StatusResponse = serde_json::from_str(&response.body).unwrap();
    assert_eq!(status.pid, daemon.pid());
}

#[test]
fn plain_file_at_the_socket_path_does_not_prevent_start() {
    let home = short_home();
    fs::create_dir(home.path().join("run")).unwrap();
    File::create(socket(home.path())).unwrap();

    let _daemon = Daemon::start(&mut loomd(home.path()), &socket(home.path()));
    assert_eq!(
        request(&socket(home.path()), "GET", "/v1/status").status,
        200
    );
}

#[test]
fn unknown_config_key_reports_file_line_and_key() {
    let home = short_home();
    let config = home.path().join("config.toml");
    fs::write(&config, "# loom\n\nunknown_key = 1\n").unwrap();

    let (code, stderr) = run(&mut loomd(home.path()));
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!("{}:3:1", config.display())),
        "{stderr}"
    );
    assert!(stderr.contains("unknown_key"), "{stderr}");
    // Nothing was created.
    assert!(!home.path().join("run").exists());
}

#[test]
fn config_syntax_error_reports_file_and_line() {
    let home = short_home();
    let config = home.path().join("config.toml");
    fs::write(&config, "\n\nbroken = \n").unwrap();

    let (code, stderr) = run(&mut loomd(home.path()));
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!("{}:3:", config.display())),
        "{stderr}"
    );
}

#[test]
fn too_long_socket_path_is_reported() {
    let home = short_home();
    let deep = home.path().join("d".repeat(120));
    let (code, stderr) = run(&mut loomd(&deep));
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains(&*socket(&deep).to_string_lossy()),
        "{stderr}"
    );
}

/// Sends `signal`, then checks the exit code, that the socket is gone and the lock released.
fn assert_graceful_shutdown(signal: &str, expected_code: i32) {
    let home = short_home();
    let mut daemon = Daemon::start(&mut loomd(home.path()), &socket(home.path()));

    daemon.signal(signal);
    let status = daemon.wait();
    let stderr = daemon.stderr();
    assert_eq!(status.code(), Some(expected_code), "{stderr}");
    assert!(stderr.contains("loomd stopped"), "{stderr}");
    assert!(!socket(home.path()).exists());
    File::open(lock(home.path())).unwrap().try_lock().unwrap();
}

#[test]
fn sigterm_shuts_down_gracefully_with_code_0() {
    assert_graceful_shutdown("TERM", 0);
}

#[test]
fn sigint_shuts_down_gracefully_with_code_130() {
    assert_graceful_shutdown("INT", 130);
}
