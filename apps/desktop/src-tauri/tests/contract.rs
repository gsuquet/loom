//! The bridge client against a real loomd on a real socket: fake data, fake home.
//! `LOOMD_BIN` names the binary; the test fails without it, it never looks for a real install.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use loom_desktop::client::{ClientError, SseEvent, request, stream_events};

#[tokio::test(flavor = "current_thread")]
async fn request_and_events_on_a_real_loomd() {
    let bin = std::env::var("LOOMD_BIN").expect("LOOMD_BIN");
    // Under /tmp: the macOS default temporary directory is too long for a socket path.
    let root = tempfile::Builder::new().prefix("lsc-").tempdir_in("/tmp").unwrap();
    let home = root.path().join("h");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "[update]\npolicy = \"off\"\n\n[repos]\nroots = []\n\n[contexts.demo]\n",
    )
    .unwrap();
    let mut child = Command::new(bin)
        .args(["--color", "never"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("LOOM_HOME", &home)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let socket = home.join("run/loomd.sock");
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let status = request(&socket, "GET", "/v1/status", None).await.unwrap();
    assert!(status["api_version"].is_number() || status["api_version"].is_string());

    let (tx, rx) = mpsc::channel::<SseEvent>();
    let stream_socket = socket.clone();
    let stream = tokio::spawn(async move {
        stream_events(&stream_socket, None, |e| tx.send(e).is_ok()).await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let body = br#"{"title":"Fake contract","kind":"chore","context":"demo","status":"inbox"}"#.to_vec();
    let created = request(&socket, "POST", "/v1/tasks", Some(body)).await.unwrap();
    let id = created["id"].as_str().expect("id").to_owned();

    let mut seen = None;
    for _ in 0..100 {
        if let Ok(event) = rx.try_recv() {
            seen = Some(event);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let event = seen.expect("no event in 5 s");
    assert_eq!(event.event, "task.changed");
    assert!(event.data.contains(&id));
    assert!(event.id.is_some());

    let missing = request(&socket, "GET", "/v1/tasks/zzzzzzzz", None).await;
    assert!(matches!(missing, Err(ClientError::Api { status: 404, .. })));

    stream.abort();
    child.kill().unwrap();
    child.wait().unwrap();

    let gone = request(&socket, "GET", "/v1/status", None).await;
    assert!(matches!(gone, Err(ClientError::Unreachable { .. })));
}
