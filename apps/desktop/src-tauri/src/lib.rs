//! Throwaway spike of the desktop app: the Rust side holds the socket.

pub mod allow;
pub mod client;

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use tauri::ipc::{Channel, Response};
use tauri_plugin_notification::NotificationExt as _;

use client::ClientError;

/// The sandbox home. Never `~/.loom`: the spike must not reach a real store.
fn loom_home() -> PathBuf {
    if let Some(home) = std::env::var_os("LOOM_SPIKE_HOME") {
        return PathBuf::from(home);
    }
    let base = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    base.join(".cache/loom-spike/home")
}

fn socket() -> PathBuf {
    loom_home().join("run/loomd.sock")
}

/// The one route to loomd for requests. `path` goes through the allow-list.
#[tauri::command]
async fn loom_api(
    method: String,
    path: String,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, ClientError> {
    if !allow::is_allowed(&method, &path) {
        return Err(ClientError::NotAllowed);
    }
    let body = body
        .map(|b| serde_json::to_vec(&b))
        .transpose()
        .map_err(|e| ClientError::Protocol {
            message: e.to_string(),
        })?;
    client::request(&socket(), &method, &path, body).await
}

/// What the page receives on the channel of `/v1/events`.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ChangeMessage {
    Connected,
    Disconnected {
        reason: String,
    },
    Event {
        id: Option<String>,
        event: String,
        data: String,
    },
}

/// Streams `/v1/events`, reconnecting with `Last-Event-ID`, until the channel closes.
#[tauri::command]
fn loom_events(channel: Channel<ChangeMessage>) {
    tauri::async_runtime::spawn(async move {
        let mut last_id: Option<String> = None;
        loop {
            if channel.send(ChangeMessage::Connected).is_err() {
                return;
            }
            let mut open = true;
            let resume = last_id.clone();
            let result = client::stream_events(&socket(), resume.as_deref(), |event| {
                if event.id.is_some() {
                    last_id.clone_from(&event.id);
                }
                open = channel
                    .send(ChangeMessage::Event {
                        id: event.id,
                        event: event.event,
                        data: event.data,
                    })
                    .is_ok();
                open
            })
            .await;
            if !open {
                return;
            }
            let reason = result.map_or_else(|e| e.to_string(), |()| "stream ended".to_owned());
            if channel
                .send(ChangeMessage::Disconnected { reason })
                .is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

/// The MR patch. loomd has no diff route yet; this reads the fixture of the sandbox as raw bytes.
#[tauri::command]
async fn spike_patch() -> Result<Response, String> {
    let path = loom_home().join("spike-fixtures/mr.patch");
    tokio::task::spawn_blocking(move || std::fs::read(&path))
        .await
        .map_err(|e| e.to_string())?
        .map(Response::new)
        .map_err(|e| format!("cannot read the patch fixture: {e}. Run `mise run spike-sandbox`."))
}

/// Sends an OS notification and reports what the plugin says. The display is for the owner to see.
#[tauri::command]
fn spike_notify(app: tauri::AppHandle, title: String, body: String) -> Result<String, String> {
    let notification = app.notification();
    let state = notification.permission_state().map_err(|e| e.to_string())?;
    notification
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())?;
    Ok(format!("sent, plugin permission state: {state:?}"))
}

/// Starts the app.
///
/// # Panics
///
/// Panics if the window cannot be created: nothing can run without it.
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            loom_api,
            loom_events,
            spike_patch,
            spike_notify
        ]);
    // WebKit stops requestAnimationFrame in an occluded window. The test keeps the window above
    // the others, without taking the keyboard focus from whoever uses the machine.
    #[cfg(feature = "e2e")]
    let builder = builder
        .plugin(tauri_plugin_wdio_webdriver::init())
        .setup(|app| {
            use tauri::Manager as _;
            if let Some(window) = app.get_webview_window("main") {
                window.set_always_on_top(true)?;
            }
            Ok(())
        });
    builder
        .run(tauri::generate_context!())
        .expect("cannot run the app");
}
