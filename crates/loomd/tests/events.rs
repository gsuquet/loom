//! `GET /v1/events` end to end: a real loomd on a temporary loom home, its stream read over the
//! unix socket.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use loom_api::{ChangeEvent, TaskDetail};
use serde_json::{Value, json};
use tempfile::TempDir;

mod common;

use common::{Daemon, ok, request};

const CONFIG: &str = r"
[repos]
roots = []

[contexts.work]

[contexts.home]
";

/// How long a change may take to reach a client (the issue asks for 1 s).
const DELIVERY: Duration = Duration::from_secs(1);

struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("loomd-events-")
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

    fn start(&self) -> Daemon {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
        command
            .env("LOOM_HOME", self.loom_home())
            .env("HOME", self.user_home())
            .env("XDG_CONFIG_HOME", self.user_home().join(".config"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LOOM_TEST_EVENTS_HEARTBEAT", "1s")
            .arg("--color=never")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Daemon::start(&mut command, &self.socket())
    }

    fn create(&self, context: &str, title: &str) -> TaskDetail {
        ok(&request(
            &self.socket(),
            "POST",
            "/v1/tasks",
            Some(&json!({
                "title": title,
                "kind": "chore",
                "context": context,
                "status": "inbox",
            })),
        ))
    }
}

/// One Server-Sent Event, or a comment (`kind` `:`).
#[derive(Debug)]
struct Sse {
    id: Option<String>,
    kind: String,
    data: String,
}

/// An open `GET /v1/events`, its chunked body decoded as it arrives.
struct Stream {
    reader: BufReader<UnixStream>,
    /// Decoded body not yet split into events.
    text: String,
}

impl Stream {
    fn open(sandbox: &Sandbox, query: &str, last_event_id: Option<&str>) -> Self {
        let mut stream = UnixStream::connect(sandbox.socket()).unwrap();
        let last = last_event_id.map_or_else(String::new, |id| format!("last-event-id: {id}\r\n"));
        write!(
            stream,
            "GET /v1/events{query} HTTP/1.1\r\nhost: loomd\r\naccept: text/event-stream\r\n{last}\r\n"
        )
        .unwrap();
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        assert!(status.starts_with("HTTP/1.1 200"), "{status}");
        let mut chunked = false;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            let lower = line.to_ascii_lowercase();
            assert!(
                !lower.starts_with("content-type:") || lower.contains("text/event-stream"),
                "{line}"
            );
            chunked |= lower.starts_with("transfer-encoding:") && lower.contains("chunked");
        }
        assert!(chunked, "the stream is not chunked");
        Self {
            reader,
            text: String::new(),
        }
    }

    /// The next event or comment, within `within`; `None` when the stream ends or nothing
    /// comes in time.
    fn next(&mut self, within: Duration) -> Option<Sse> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(end) = self.text.find("\n\n") {
                let block: String = self.text.drain(..end + 2).collect();
                return Some(parse(&block));
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            // macOS refuses the option on a socket the peer closed: the stream ended.
            if self
                .reader
                .get_ref()
                .set_read_timeout(Some(left.max(Duration::from_millis(1))))
                .is_err()
            {
                return None;
            }
            let mut size = String::new();
            if !matches!(self.reader.read_line(&mut size), Ok(read) if read > 0) {
                return None;
            }
            let size = usize::from_str_radix(size.trim(), 16).unwrap();
            if size == 0 {
                return None;
            }
            let mut chunk = vec![0; size + 2];
            self.reader.read_exact(&mut chunk).unwrap();
            chunk.truncate(size);
            self.text.push_str(&String::from_utf8(chunk).unwrap());
        }
    }

    /// The next event, comments skipped.
    fn event(&mut self, within: Duration) -> Option<Sse> {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            let sse = self.next(left)?;
            if sse.kind != ":" {
                return Some(sse);
            }
        }
    }
}

fn parse(block: &str) -> Sse {
    let mut sse = Sse {
        id: None,
        kind: String::new(),
        data: String::new(),
    };
    for line in block.lines() {
        if let Some(comment) = line.strip_prefix(':') {
            ":".clone_into(&mut sse.kind);
            comment.trim().clone_into(&mut sse.data);
        } else if let Some(id) = line.strip_prefix("id:") {
            sse.id = Some(id.trim().to_owned());
        } else if let Some(kind) = line.strip_prefix("event:") {
            kind.trim().clone_into(&mut sse.kind);
        } else if let Some(data) = line.strip_prefix("data:") {
            data.trim().clone_into(&mut sse.data);
        }
    }
    sse
}

/// The event of `sse`, checked against the `ChangeEvent` schema.
fn change(sse: &Sse) -> ChangeEvent {
    let value: Value = serde_json::from_str(&sse.data).unwrap();
    let schema = serde_json::to_value(schemars::schema_for!(ChangeEvent)).unwrap();
    assert!(jsonschema::is_valid(&schema, &value), "{value}");
    let event: ChangeEvent = serde_json::from_value(value).unwrap();
    assert_eq!(event.kind(), sse.kind);
    event
}

fn task_changed(sse: &Sse) -> (String, String) {
    match change(sse) {
        ChangeEvent::TaskChanged { id, context } => (id.to_string(), context.to_string()),
        other => panic!("not task.changed: {other:?}"),
    }
}

#[test]
fn api_changes_and_hand_edits_reach_a_client_within_a_second() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let mut stream = Stream::open(&sandbox, "", None);

    let created = sandbox.create("work", "Review the API");
    let id = created.summary.id.to_string();
    let sse = stream.event(DELIVERY).expect("no event for the creation");
    assert_eq!(task_changed(&sse), (id.clone(), "work".to_owned()));
    let first = sse.id.unwrap();

    let _: TaskDetail = ok(&request(
        &sandbox.socket(),
        "PATCH",
        &format!("/v1/tasks/{id}"),
        Some(&json!({ "importance": "p0" })),
    ));
    let sse = stream.event(DELIVERY).expect("no event for the edit");
    assert_eq!(task_changed(&sse).0, id);
    assert_ne!(sse.id.unwrap(), first, "ids increase");

    // A hand edit, picked up by the watcher.
    let file = sandbox
        .loom_home()
        .join("contexts/work/tasks")
        .join(&id)
        .join("task.md");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(
        &file,
        text.replace("Review the API", "Review the tasks API"),
    )
    .unwrap();
    let sse = stream
        .event(Duration::from_secs(5))
        .expect("no event for the hand edit");
    assert_eq!(task_changed(&sse).0, id);
    daemon.stop();
}

#[test]
fn a_context_filter_keeps_the_events_of_its_context() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let mut home = Stream::open(&sandbox, "?context=home", None);
    sandbox.create("work", "Elsewhere");
    let mine = sandbox.create("home", "Here").summary.id.to_string();
    let sse = home.event(DELIVERY).unwrap();
    assert_eq!(task_changed(&sse), (mine, "home".to_owned()));

    let bad = request(
        &sandbox.socket(),
        "GET",
        "/v1/events?context=Not%20A%20Name",
        None,
    );
    assert_eq!(bad.status, 400, "{}", bad.body);
    let bad = request(&sandbox.socket(), "GET", "/v1/events?colour=red", None);
    assert_eq!(bad.status, 400, "{}", bad.body);
    daemon.stop();
}

#[test]
fn a_client_resumes_from_its_last_event_or_resyncs() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let mut stream = Stream::open(&sandbox, "", None);
    sandbox.create("work", "Seen");
    let seen = stream.event(DELIVERY).unwrap().id.unwrap();
    drop(stream);

    let missed = sandbox.create("work", "Missed").summary.id.to_string();
    let mut resumed = Stream::open(&sandbox, "", Some(&seen));
    let sse = resumed.event(DELIVERY).unwrap();
    assert_eq!(task_changed(&sse).0, missed);

    // A resume point of another run, or not an id at all: resync.
    for last in ["1-1", "garbage"] {
        let mut foreign = Stream::open(&sandbox, "", Some(last));
        let sse = foreign.event(DELIVERY).unwrap();
        assert_eq!(change(&sse), ChangeEvent::Resync);
    }
    daemon.stop();
}

#[test]
fn an_idle_stream_gets_heartbeats_and_ends_when_loomd_stops() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start();
    let mut stream = Stream::open(&sandbox, "", None);
    let beat = stream.next(Duration::from_secs(3)).expect("no heartbeat");
    assert_eq!(beat.kind, ":");
    assert_eq!(beat.data, "heartbeat");

    // An open stream does not hold the graceful shutdown.
    let started = Instant::now();
    daemon.stop();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    while stream.next(Duration::from_secs(2)).is_some() {}
}
