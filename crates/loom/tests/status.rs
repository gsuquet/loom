//! `loom status` end to end, with a temporary `LOOM_HOME` and the `loomd` built next to `loom`.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs::{self, File};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use loom_api::{ApiError, ApiVersion, ErrorCode, StatusResponse, Timestamp, VersionInfo};
use serde_json::Value;
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);

fn loom_bin() -> PathBuf {
    PathBuf::from(assert_cmd::cargo::cargo_bin!("loom"))
}

/// A loom home under `/tmp`: the macOS default `$TMPDIR` is too long for a socket path.
fn short_home() -> TempDir {
    let loomd = loom_bin().with_file_name("loomd");
    assert!(
        loomd.is_file(),
        "{} is missing: build the workspace (`cargo test --workspace`)",
        loomd.display()
    );
    tempfile::Builder::new()
        .prefix("loom-")
        .tempdir_in("/tmp")
        .unwrap()
}

fn socket(home: &Path) -> PathBuf {
    home.join("run/loomd.sock")
}

fn lock(home: &Path) -> PathBuf {
    home.join("run/loomd.lock")
}

fn log(home: &Path) -> PathBuf {
    home.join("log/loomd.log")
}

/// Runs `loom --color=never <args>` with `LOOM_HOME` set to `home`.
fn loom(home: &Path, args: &[&str]) -> Output {
    Command::new(loom_bin())
        .env("LOOM_HOME", home)
        .env_remove("NO_COLOR")
        .arg("--color=never")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// Stops, when dropped, the daemon a test started in `home`.
struct DaemonGuard<'a> {
    home: &'a Path,
}

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        // A lock nobody holds: no daemon to stop.
        let Ok(file) = File::open(lock(self.home)) else {
            return;
        };
        if file.try_lock().is_ok() {
            return;
        }
        let Some(pid) = fs::read_to_string(lock(self.home))
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        else {
            return;
        };
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
        // Waits for the lock to be released, so the temporary home is removed after loomd.
        let deadline = Instant::now() + TIMEOUT;
        while file.try_lock().is_err() && Instant::now() < deadline {
            thread::sleep(POLL);
        }
    }
}

/// PID written in the lock by the running daemon.
fn lock_pid(home: &Path) -> u32 {
    fs::read_to_string(lock(home))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// A fake loomd on the socket of `home`: answers one request with `status` and `body`.
fn fake_loomd(home: &Path, status: u16, body: String) -> JoinHandle<()> {
    fs::create_dir_all(home.join("run")).unwrap();
    let listener = UnixListener::bind(socket(home)).unwrap();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        let mut reader = BufReader::new(&stream);
        // Request head, up to the empty line; `GET` has no body.
        let mut line = String::new();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        write!(
            &stream,
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    })
}

fn other_daemon_status(home: &Path) -> StatusResponse {
    StatusResponse {
        api_version: ApiVersion::V1,
        daemon: VersionInfo {
            version: "0.0.1-other".to_owned(),
            ..VersionInfo::current()
        },
        pid: 4242,
        started_at: Timestamp::from_unix_secs(1_791_792_000).unwrap(),
        uptime_secs: 90,
        loom_home: home.to_path_buf(),
        socket_path: socket(home),
        config_path: home.join("config.toml"),
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn starts_loomd_when_absent_then_reuses_it() {
    let home = short_home();
    let _guard = DaemonGuard { home: home.path() };

    let first = loom(home.path(), &["status"]);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert!(stderr(&first).contains("starting it"), "{}", stderr(&first));
    let text = stdout(&first);
    let version = &VersionInfo::current().version;
    assert!(
        text.starts_with(&format!("loomd {version} running\n")),
        "{text}"
    );
    let pid = lock_pid(home.path());
    assert!(text.contains(&format!("\npid:         {pid}\n")), "{text}");
    assert!(
        text.contains(&format!("\nloom_home:   {}\n", home.path().display())),
        "{text}"
    );

    let second = loom(home.path(), &["status", "--json"]);
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert!(
        !stderr(&second).contains("starting it"),
        "{}",
        stderr(&second)
    );
    let status: StatusResponse = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(status.pid, pid);

    assert_eq!(mode(&home.path().join("log")), 0o700);
    assert_eq!(mode(&log(home.path())), 0o600);
}

#[test]
fn json_output_validates_against_the_schema() {
    let home = short_home();
    let _guard = DaemonGuard { home: home.path() };

    let output = loom(home.path(), &["status", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.ends_with("}\n"), "{text}");
    let instance: Value = serde_json::from_str(&text).unwrap();
    let schema = serde_json::to_value(schemars::schema_for!(StatusResponse)).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "schema violations: {errors:?}");

    let status: StatusResponse = serde_json::from_value(instance).unwrap();
    assert_eq!(status.daemon, VersionInfo::current());
    assert_eq!(status.loom_home, home.path());
}

#[test]
fn no_start_without_daemon_fails() {
    let home = short_home();
    let output = loom(home.path(), &["status", "--no-start"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("loomd is not running"), "{text}");
    assert!(text.contains("--no-start"), "{text}");
    assert!(
        text.contains(&*socket(home.path()).to_string_lossy()),
        "{text}"
    );
    assert_eq!(stdout(&output), "");
    // Nothing was started.
    assert!(!home.path().join("run").exists());
    assert!(!home.path().join("log").exists());
}

#[test]
fn start_failure_names_the_log_and_shows_its_end() {
    let home = short_home();
    let _guard = DaemonGuard { home: home.path() };
    let config = home.path().join("config.toml");
    fs::write(&config, "# loom\n\nunknown_key = 1\n").unwrap();

    let started = Instant::now();
    let output = loom(home.path(), &["status"]);
    let elapsed = started.elapsed();
    assert_eq!(output.status.code(), Some(1));
    // Nobody holds the lock: no wait for the 5 s start window.
    assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    let text = stderr(&output);
    assert!(text.contains("cannot start loomd"), "{text}");
    assert!(
        text.contains(&*log(home.path()).to_string_lossy()),
        "{text}"
    );
    assert!(
        text.contains(&format!("invalid configuration {}:3:1", config.display())),
        "{text}"
    );
    assert!(text.contains("unknown_key"), "{text}");
    assert_eq!(stdout(&output), "");
}

#[test]
fn version_mismatch_is_a_warning_on_stderr() {
    let home = short_home();
    let body = serde_json::to_string(&other_daemon_status(home.path())).unwrap();
    let server = fake_loomd(home.path(), 200, body);

    let output = loom(home.path(), &["status"]);
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let warning = stderr(&output);
    let version = &VersionInfo::current().version;
    assert!(warning.contains("WARN"), "{warning}");
    assert!(warning.contains(&format!("loom {version} ")), "{warning}");
    assert!(warning.contains("loomd 0.0.1-other "), "{warning}");
    assert!(warning.contains("restart loomd (PID 4242)"), "{warning}");
    let text = stdout(&output);
    assert!(text.starts_with("loomd 0.0.1-other running\n"), "{text}");
    assert!(!text.contains("restart"), "{text}");
}

#[test]
fn quiet_suppresses_the_version_mismatch_warning() {
    let home = short_home();
    let body = serde_json::to_string(&other_daemon_status(home.path())).unwrap();
    let server = fake_loomd(home.path(), 200, body);

    let output = loom(home.path(), &["-q", "status", "--json"]);
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stderr(&output), "");
    let status: StatusResponse = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status.daemon.version, "0.0.1-other");
}

#[test]
fn api_error_fails_with_its_code_and_message() {
    let home = short_home();
    let body =
        serde_json::to_string(&ApiError::new(ErrorCode::Unavailable, "shutting down")).unwrap();
    let server = fake_loomd(home.path(), 503, body);

    let output = loom(home.path(), &["status"]);
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("HTTP 503, unavailable"), "{text}");
    assert!(text.contains("shutting down"), "{text}");
    assert_eq!(stdout(&output), "");
}

#[test]
fn invalid_body_is_a_protocol_error() {
    let home = short_home();
    let server = fake_loomd(home.path(), 200, "not json".to_owned());

    let output = loom(home.path(), &["status"]);
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("unexpected answer from loomd"), "{text}");
    assert!(
        text.contains(&*socket(home.path()).to_string_lossy()),
        "{text}"
    );
    assert_eq!(stdout(&output), "");
}
