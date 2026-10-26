//! Traces of `loom` and `loomd` end to end: a fake OTLP/HTTP collector on a local TCP port,
//! a temporary `LOOM_HOME` and the `loomd` built next to `loom`.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs::{self, File};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use opentelemetry_proto::tonic::common::v1::{AnyValue, any_value};
use opentelemetry_proto::tonic::trace::v1::Span;
use tempfile::TempDir;

mod common;
mod isolation;

use common::{Collector, string_attribute};

const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
/// `SpanKind` values of the OTLP protocol.
const KIND_INTERNAL: i32 = 1;
const KIND_SERVER: i32 = 2;
const KIND_CLIENT: i32 = 3;
/// OpenTelemetry variables of the developer's shell, removed so they cannot change a test.
const OTEL_VARIABLES: [&str; 8] = [
    "OTEL_SDK_DISABLED",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
    "OTEL_EXPORTER_OTLP_PROTOCOL",
    "OTEL_SERVICE_NAME",
    "OTEL_RESOURCE_ATTRIBUTES",
];

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

fn lock(home: &Path) -> PathBuf {
    home.join("run/loomd.lock")
}

/// Runs `loom --color=never <args>` with `LOOM_HOME` set to `home` and `env` added. A started
/// `loomd` inherits the same environment.
fn loom(home: &Path, args: &[&str], env: &[(&str, String)]) -> Output {
    let mut command = Command::new(loom_bin());
    for name in OTEL_VARIABLES {
        command.env_remove(name);
    }
    command
        // Outside any repository: no registration on use adds its own request.
        .current_dir(home)
        .env("LOOM_HOME", home)
        .envs(isolation::isolated_env(home))
        .env_remove("NO_COLOR")
        .envs(env.iter().map(|(name, value)| (name, value)))
        .arg("--color=never")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// Stops the daemon running in `home`, if any, and waits until it has exited.
fn stop_daemon(home: &Path) {
    let Ok(file) = File::open(lock(home)) else {
        return;
    };
    if file.try_lock().is_ok() {
        return;
    }
    let Some(pid) = fs::read_to_string(lock(home))
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
    else {
        return;
    };
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
    let deadline = Instant::now() + TIMEOUT;
    while file.try_lock().is_err() && Instant::now() < deadline {
        thread::sleep(POLL);
    }
}

/// Stops, when dropped, the daemon a test started in `home`.
struct DaemonGuard<'a> {
    home: &'a Path,
}

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        stop_daemon(self.home);
    }
}

/// Every connection attempt to a listener that is never accepted stays in its backlog:
/// `true` when one is waiting.
fn was_contacted(listener: &TcpListener) -> bool {
    listener.set_nonblocking(true).unwrap();
    match listener.accept() {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
        Err(error) => panic!("accept failed: {error}"),
    }
}

/// Runs `loom status`, which starts loomd, then `loom version`, then stops loomd, with every
/// OTLP endpoint variable pointing at a local listener; asserts that nothing connected to it.
fn assert_no_connection(config: Option<&str>, env: &[(&str, String)]) {
    let home = short_home();
    let _guard = DaemonGuard { home: home.path() };
    if let Some(config) = config {
        fs::write(home.path().join("config.toml"), config).unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mut env = env.to_vec();
    env.push(("OTEL_EXPORTER_OTLP_ENDPOINT", url.clone()));
    env.push((
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        format!("{url}/v1/traces"),
    ));

    let status = loom(home.path(), &["status"], &env);
    assert_eq!(status.status.code(), Some(0), "{}", stderr(&status));
    let version = loom(home.path(), &["version"], &env);
    assert_eq!(version.status.code(), Some(0), "{}", stderr(&version));
    // loomd would export its last spans when it stops.
    stop_daemon(home.path());

    assert!(!was_contacted(&listener), "a process connected to {url}");
}

#[test]
fn telemetry_disabled_by_default_opens_no_connection() {
    assert_no_connection(None, &[]);
}

#[test]
fn telemetry_disabled_in_config_opens_no_connection() {
    assert_no_connection(Some("[telemetry]\nenabled = false\n"), &[]);
}

#[test]
fn otel_sdk_disabled_wins_over_the_config() {
    assert_no_connection(
        Some("[telemetry]\nenabled = true\n"),
        &[("OTEL_SDK_DISABLED", "true".to_owned())],
    );
}

/// The listener itself reports any connection, so the tests above can fail.
#[test]
fn a_connection_to_the_listener_is_detected() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    // The handshake completes in the kernel; give it a moment on a loaded machine.
    let deadline = Instant::now() + TIMEOUT;
    let mut contacted = false;
    while !contacted && Instant::now() < deadline {
        contacted = was_contacted(&listener);
        thread::sleep(POLL);
    }
    assert!(contacted);
}

fn int_attribute(span: &Span, key: &str) -> Option<i64> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_ref())
        .and_then(|value: &AnyValue| match value.value {
            Some(any_value::Value::IntValue(number)) => Some(number),
            _ => None,
        })
}

/// The acceptance check of telemetry: one `loom status` gives one trace in which the daemon's
/// request span is the child of the CLI's client span.
#[test]
fn status_trace_links_cli_and_daemon_spans() {
    let home = short_home();
    let _guard = DaemonGuard { home: home.path() };
    let collector = Collector::start();
    let endpoint = format!("{}/v1/traces", collector.url);
    fs::write(
        home.path().join("config.toml"),
        format!("[telemetry]\nenabled = true\notlp_endpoint = \"{endpoint}\"\n"),
    )
    .unwrap();
    // loomd exports its batch 100 ms after the request instead of 5 s.
    let env = [("OTEL_BSP_SCHEDULE_DELAY", "100".to_owned())];

    let output = loom(home.path(), &["-c", "personal", "status"], &env);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    let server_span = || {
        collector
            .spans()
            .into_iter()
            .find(|(service, span)| service == "loomd" && span.kind == KIND_SERVER)
    };
    let deadline = Instant::now() + TIMEOUT;
    while server_span().is_none() && Instant::now() < deadline {
        thread::sleep(POLL);
    }
    let (_, server) = server_span()
        .unwrap_or_else(|| panic!("no server span from loomd; spans: {:#?}", collector.spans()));
    assert!(collector.paths().iter().all(|path| path == "/v1/traces"));

    let spans = collector.spans();
    let (_, root) = spans
        .iter()
        .find(|(service, span)| service == "loom" && span.name == "loom status")
        .unwrap();
    assert_eq!(root.kind, KIND_INTERNAL);
    assert_eq!(root.parent_span_id, Vec::<u8>::new());
    assert_eq!(
        string_attribute(&root.attributes, "loom.context").as_deref(),
        Some("personal")
    );
    // The answered request; failed attempts may precede it while loomd starts.
    let (_, client) = spans
        .iter()
        .find(|(service, span)| {
            service == "loom"
                && span.kind == KIND_CLIENT
                && int_attribute(span, "http.response.status_code") == Some(200)
        })
        .unwrap();
    assert_eq!(client.name, "GET /v1/status");
    assert_eq!(client.parent_span_id, root.span_id);
    assert_eq!(client.trace_id, root.trace_id);

    assert_eq!(server.name, "GET /v1/status");
    assert_eq!(server.trace_id, root.trace_id);
    assert_eq!(server.parent_span_id, client.span_id);
    assert_eq!(
        int_attribute(&server, "http.response.status_code"),
        Some(200)
    );

    let version = loom_api::VersionInfo::current();
    for service in ["loom", "loomd"] {
        assert_eq!(
            collector.resource_attribute(service, "service.version"),
            Some(version.version.clone()),
            "{service}"
        );
        assert_eq!(
            collector.resource_attribute(service, "loom.channel"),
            Some(version.channel.as_str().to_owned()),
            "{service}"
        );
    }
}

/// `loom` reads `config.toml` for telemetry only: an invalid file does not break a command
/// that does not need it.
#[test]
fn invalid_config_warns_and_the_command_runs() {
    let home = short_home();
    fs::write(
        home.path().join("config.toml"),
        "[telemetry]\nenabled = true\notlp_endpoint = \"localhost:4318\"\n",
    )
    .unwrap();
    let output = loom(home.path(), &["version"], &[]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(
        text.contains("traces are not exported: invalid configuration "),
        "{text}"
    );
    assert!(text.contains("config.toml:3:"), "{text}");
    assert!(output.stdout.starts_with(b"loom "));
}

/// A dead collector delays a command by at most the 2 s shutdown timeout.
#[test]
fn unreachable_collector_does_not_fail_the_command() {
    let home = short_home();
    // A port nothing listens on: bound, then released.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    fs::write(
        home.path().join("config.toml"),
        format!(
            "[telemetry]\nenabled = true\notlp_endpoint = \"http://127.0.0.1:{port}/v1/traces\"\n"
        ),
    )
    .unwrap();
    let started = Instant::now();
    let output = loom(home.path(), &["version"], &[]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(output.stdout.starts_with(b"loom "));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
}
