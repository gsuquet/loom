//! `loomd` end to end: started in a temporary `LOOM_HOME`, queried over its unix socket.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fmt::Write as _;
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

/// A loom home whose context `work` has a backup remote, and a `git` first in `PATH` that makes
/// every `git fetch` last a minute: `loomd` is then inside a fetch from its first sync on.
/// The pids of the shim and of its `sleep` are in `shim/`; whatever survives is killed on drop.
struct SlowFetch {
    home: TempDir,
}

impl SlowFetch {
    /// `ignore_sigterm`: the shim and its `sleep` ignore `SIGTERM`, like a hung `ssh`.
    fn new(ignore_sigterm: bool) -> Self {
        let home = short_home();
        let shim = home.path().join("shim");
        fs::create_dir_all(&shim).unwrap();
        fs::write(
            home.path().join("config.toml"),
            format!(
                "[repos]\nroots = []\n\n[contexts.work]\nstore.backup.remote = \"{}\"\n",
                home.path().join("remote.git").display()
            ),
        )
        .unwrap();
        fs::write(
            home.path().join(".gitconfig"),
            "[user]\n\tname = Loom Test\n\temail = loom@example.invalid\n",
        )
        .unwrap();
        let trap = if ignore_sigterm { "trap '' TERM\n" } else { "" };
        let script = format!(
            "#!/bin/sh\n\
             case \" $* \" in\n\
             *\" fetch \"*)\n\
             {trap}echo $$ > '{dir}/fetch.pid'\n\
             sleep 60 &\n\
             echo $! > '{dir}/sleep.pid'\n\
             wait\n\
             ;;\n\
             *) exec '{git}' \"$@\" ;;\n\
             esac\n",
            dir = shim.display(),
            git = real_git().display(),
        );
        let path = shim.join("git");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        Self { home }
    }

    /// A repository `perso/app` with an SSH remote and a `ssh` that hangs, which `loomd` runs
    /// for a remote check. Updates of the repository are off: a repository update leaves its
    /// git command to finish by design, and this test is about the check.
    fn hanging_ssh() -> Self {
        let home = short_home();
        let shim = home.path().join("shim");
        fs::create_dir_all(&shim).unwrap();
        let repo = home.path().join("perso/app");
        fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["remote", "add", "origin", "git@hang.invalid:x/y.git"],
        ] {
            let status = Command::new(real_git())
                .current_dir(&repo)
                .env("HOME", home.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
        fs::write(
            home.path().join("config.toml"),
            format!(
                "[repos]\nroots = [\"{root}\"]\n\n[repos.\"{repo}\"]\nupdate = \"off\"\n",
                root = home.path().join("perso").display(),
                repo = repo.display()
            ),
        )
        .unwrap();
        let script = format!(
            "#!/bin/sh\necho $$ > '{dir}/fetch.pid'\nsleep 60 &\necho $! > '{dir}/sleep.pid'\nwait\n",
            dir = shim.display()
        );
        let path = shim.join("ssh");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        Self { home }
    }

    /// A second context with its own backup remote, so that a sync has two stores to go through.
    fn with_second_context(self) -> Self {
        let config = self.home.path().join("config.toml");
        let mut text = fs::read_to_string(&config).unwrap();
        writeln!(
            text,
            "\n[contexts.home]\nstore.backup.remote = \"{}\"",
            self.home.path().join("remote-2.git").display()
        )
        .unwrap();
        fs::write(&config, text).unwrap();
        self
    }

    fn command(&self) -> Command {
        let mut command = loomd(self.home.path());
        let path = std::env::var_os("PATH").unwrap_or_default();
        let paths =
            std::iter::once(self.home.path().join("shim")).chain(std::env::split_paths(&path));
        command
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
    }

    /// The pid in `shim/<name>.pid`, once the shim wrote it.
    fn pid(&self, name: &str) -> Option<u32> {
        fs::read_to_string(self.home.path().join(format!("shim/{name}.pid")))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Waits until `loomd` is inside the fetch: the shim and its `sleep` run.
    fn wait_for_fetch(&self) {
        let deadline = Instant::now() + TIMEOUT;
        while self.pid("fetch").is_none() || self.pid("sleep").is_none() {
            assert!(Instant::now() < deadline, "the fetch never started");
            thread::sleep(POLL);
        }
    }

    /// Whether the shim or its `sleep` still runs, after at most 2 s.
    fn orphans(&self) -> Vec<u32> {
        let pids: Vec<u32> = ["fetch", "sleep"]
            .iter()
            .filter_map(|name| self.pid(name))
            .collect();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let alive: Vec<u32> = pids
                .iter()
                .copied()
                .filter(|pid| process_alive(*pid))
                .collect();
            if alive.is_empty() || Instant::now() >= deadline {
                return alive;
            }
            thread::sleep(POLL);
        }
    }
}

impl Drop for SlowFetch {
    fn drop(&mut self) {
        for name in ["fetch", "sleep"] {
            if let Some(pid) = self.pid(name) {
                let _ = Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

/// The `git` of the test environment, as an absolute path.
fn real_git() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .unwrap()
}

fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// Opens `POST path` with `body`, which `loomd` answers only when what it asks for is over (a
/// sync, a remote check), and leaves it unanswered: an API request in flight.
fn request_in_flight(socket: &Path, path: &str, body: &str) -> UnixStream {
    let mut stream = UnixStream::connect(socket).unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: loomd\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    // Time for loomd to read it: a signal before would find no request in flight.
    thread::sleep(Duration::from_millis(500));
    stream
}

#[test]
fn a_sigterm_during_a_fetch_leaves_no_orphan_and_exits_in_bounded_time() {
    let slow = SlowFetch::new(false);
    let mut daemon = Daemon::start(&mut slow.command(), &socket(slow.home.path()));
    slow.wait_for_fetch();

    let started = Instant::now();
    daemon.signal("TERM");
    let status = daemon.wait();
    let took = started.elapsed();
    let stderr = daemon.stderr();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(took < Duration::from_secs(5), "{took:?}\n{stderr}");
    assert!(stderr.contains("loomd stopped"), "{stderr}");
    assert_eq!(slow.orphans(), Vec::<u32>::new(), "{stderr}");
}

#[test]
fn a_git_command_that_ignores_sigterm_is_killed_after_the_grace() {
    let slow = SlowFetch::new(true);
    let mut daemon = Daemon::start(&mut slow.command(), &socket(slow.home.path()));
    slow.wait_for_fetch();

    let started = Instant::now();
    daemon.signal("TERM");
    let status = daemon.wait();
    let took = started.elapsed();
    let stderr = daemon.stderr();
    assert_eq!(status.code(), Some(0), "{stderr}");
    // `SIGTERM` was ignored: the command lasted the kill grace (5 s), not its minute.
    assert!(took < Duration::from_secs(12), "{took:?}\n{stderr}");
    assert_eq!(slow.orphans(), Vec::<u32>::new(), "{stderr}");
}

#[test]
fn a_second_signal_ends_the_wait_for_requests_in_flight() {
    let slow = SlowFetch::new(false);
    let mut daemon = Daemon::start(&mut slow.command(), &socket(slow.home.path()));
    slow.wait_for_fetch();
    let _in_flight = request_in_flight(&socket(slow.home.path()), "/v1/tasks/sync", "{}");

    daemon.signal("INT");
    thread::sleep(Duration::from_millis(500));
    assert!(
        daemon.child.try_wait().unwrap().is_none(),
        "the request in flight is waited for after the first signal"
    );
    let started = Instant::now();
    daemon.signal("INT");
    let status = daemon.wait();
    let took = started.elapsed();
    let stderr = daemon.stderr();
    // The code of the first signal.
    assert_eq!(status.code(), Some(130), "{stderr}");
    assert!(took < Duration::from_secs(5), "{took:?}\n{stderr}");
    assert!(stderr.contains("loomd stopped"), "{stderr}");
    assert_eq!(slow.orphans(), Vec::<u32>::new(), "{stderr}");
}

#[test]
fn requests_in_flight_are_waited_for_only_during_the_grace() {
    let slow = SlowFetch::new(false);
    let mut command = slow.command();
    command.env(loomd::hooks::SHUTDOWN_GRACE_ENV, "1s");
    let mut daemon = Daemon::start(&mut command, &socket(slow.home.path()));
    slow.wait_for_fetch();
    let _in_flight = request_in_flight(&socket(slow.home.path()), "/v1/tasks/sync", "{}");

    let started = Instant::now();
    daemon.signal("TERM");
    let status = daemon.wait();
    let took = started.elapsed();
    let stderr = daemon.stderr();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(took >= Duration::from_millis(900), "{took:?}\n{stderr}");
    assert!(took < Duration::from_secs(8), "{took:?}\n{stderr}");
    assert_eq!(slow.orphans(), Vec::<u32>::new(), "{stderr}");
}

#[test]
fn a_remote_check_in_flight_does_not_delay_a_forced_shutdown() {
    let slow = SlowFetch::hanging_ssh();
    let mut daemon = Daemon::start(&mut slow.command(), &socket(slow.home.path()));
    let deadline = Instant::now() + TIMEOUT;
    while !request(&socket(slow.home.path()), "GET", "/v1/repos")
        .body
        .contains("perso/app")
    {
        assert!(
            Instant::now() < deadline,
            "the repository was never indexed"
        );
        thread::sleep(POLL);
    }
    let _in_flight = request_in_flight(
        &socket(slow.home.path()),
        "/v1/diagnostics/remote-check",
        "",
    );
    slow.wait_for_fetch();

    daemon.signal("INT");
    thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    daemon.signal("INT");
    let status = daemon.wait();
    let took = started.elapsed();
    let stderr = daemon.stderr();
    assert_eq!(status.code(), Some(130), "{stderr}");
    // Not the 20 s of the check, and no `ssh` left behind.
    assert!(took < Duration::from_secs(5), "{took:?}\n{stderr}");
    assert_eq!(slow.orphans(), Vec::<u32>::new(), "{stderr}");
}

#[test]
fn the_stores_after_the_one_syncing_are_not_touched_once_the_shutdown_began() {
    let slow = SlowFetch::new(false).with_second_context();
    let mut daemon = Daemon::start(&mut slow.command(), &socket(slow.home.path()));
    slow.wait_for_fetch();

    daemon.signal("TERM");
    let status = daemon.wait();
    let stderr = daemon.stderr();
    assert_eq!(status.code(), Some(0), "{stderr}");
    // The store whose fetch was stopped reports it; the other one is not even tried.
    assert_eq!(
        stderr.matches("context store not synced").count(),
        1,
        "{stderr}"
    );
}
