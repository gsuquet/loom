//! Integration tests for the TUI:
//! - Non-TTY invocation prints help without launching the TUI.
//! - TUI driven against a live `loomd` with a temporary home, verifying queue rendering,
//!   page stack navigation, context colors, search, and SSE updates.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::str::FromStr as _;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use loom::client::Client;
use loom::search::search_api_path;
use loom::task::list_api_path;
use loom::tui::color::Theme;
use loom::tui::render::render;
use loom::tui::{AppEffect, AppState, ObjectPageContent, QueueTab};
use loom_api::{ContextName, SearchResults, TaskDetail, TaskList};
use loom_config::{ContextColor, ContextConfig};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tempfile::TempDir;

mod isolation;

const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
const TODAY: &str = "2026-10-14";

const CONFIG: &str = r#"
default_context = "personal"

[contexts.personal]
color = "cyan"

[contexts.work]
color = "yellow"
"#;

fn loom_bin() -> PathBuf {
    PathBuf::from(assert_cmd::cargo::cargo_bin!("loom"))
}

struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let loomd = loom_bin().with_file_name("loomd");
        assert!(loomd.is_file(), "{} is missing", loomd.display());
        let dir = tempfile::Builder::new()
            .prefix("lt-tui")
            .tempdir_in("/tmp")
            .unwrap();
        let sandbox = Self { dir };
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

    fn socket(&self) -> PathBuf {
        self.loom_home().join("run/loomd.sock")
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(loom_bin());
        command
            .env("LOOM_HOME", self.loom_home())
            .envs(isolation::isolated_env(&self.home()))
            .env("PWD", self.home())
            .env("LOOM_TEST_TODAY", TODAY)
            .env_remove("LOOM_CONTEXT")
            .env_remove("VISUAL")
            .env_remove("EDITOR")
            .env_remove("NO_COLOR")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
            .current_dir(self.home())
            .arg("--color=never")
            .args(args)
            .stdin(Stdio::null());
        command.output().unwrap()
    }

    fn add(&self, args: &[&str]) -> TaskDetail {
        let mut full = vec!["add", "--json"];
        full.extend_from_slice(args);
        let output = self.run(&full);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let lock = self.loom_home().join("run/loomd.lock");
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
            let deadline = Instant::now() + TIMEOUT;
            while file.try_lock().is_err() && Instant::now() < deadline {
                thread::sleep(POLL);
            }
        }
    }
}

fn buffer_text_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn non_tty_invocation_prints_help_and_exits_zero() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&[]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: loom [OPTIONS] [COMMAND]"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("Personal work engine: CLI and TUI"),
        "stdout: {stdout}"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn tui_driven_against_real_loomd_and_renders_snapshot() {
    let sandbox = Sandbox::new();
    // Add two tasks in different contexts; this starts loomd.
    let t1 = sandbox.add(&["Fix navigation bar", "--context", "personal"]);
    let t2 = sandbox.add(&["Review design spec", "--context", "work"]);

    let client = Client::new(sandbox.socket());

    // 1. Subscribe to SSE events before initial fetch (matching TUI behavior)
    let mut sse_stream = client.subscribe_events(None, None).await.unwrap();

    // 2. Build Theme and AppState matching sandbox configuration
    let mut contexts_config = BTreeMap::new();
    contexts_config.insert(
        ContextName::from_str("personal").unwrap(),
        ContextConfig {
            color: Some(ContextColor::Cyan),
            ..Default::default()
        },
    );
    contexts_config.insert(
        ContextName::from_str("work").unwrap(),
        ContextConfig {
            color: Some(ContextColor::Yellow),
            ..Default::default()
        },
    );
    let theme = Theme::from_config(&contexts_config, false);
    let mut state = AppState::new(
        theme,
        None,
        vec![
            ContextName::from_str("personal").unwrap(),
            ContextName::from_str("work").unwrap(),
        ],
    );

    // 3. Fetch queues from real loomd
    let today_path = list_api_path(None, None, false);
    let inbox_path = list_api_path(None, Some("inbox"), false);
    let waiting_path = list_api_path(None, Some("waiting"), false);

    let today: TaskList = client.get(&today_path).await.unwrap();
    let inbox: TaskList = client.get(&inbox_path).await.unwrap();
    let waiting: TaskList = client.get(&waiting_path).await.unwrap();

    state.today = today.tasks;
    state.inbox = inbox.tasks;
    state.waiting = waiting.tasks;

    // 4. Render to TestBackend and verify Today is empty and Inbox has 2 tasks
    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| render(f, &state)).unwrap();

    let lines = buffer_text_lines(&terminal);
    assert!(lines[0].contains("1 Today 0"), "line 0: {}", lines[0]);
    assert!(lines[0].contains("2 Inbox 2"), "line 0: {}", lines[0]);
    assert!(
        lines[2].contains("No tasks in this queue."),
        "line 2: {}",
        lines[2]
    );

    // 5. Switch to Inbox tab (2) where newly added tasks land
    state.handle_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
    assert_eq!(state.active_tab, QueueTab::Inbox);

    terminal.draw(|f| render(f, &state)).unwrap();
    let lines = buffer_text_lines(&terminal);
    assert!(lines[1].contains("Inbox · [all]"), "line 1: {}", lines[1]);

    let (first_task, second_task) = if state.inbox.first().map(|t| &t.id) == Some(&t1.summary.id) {
        (&t1, &t2)
    } else {
        (&t2, &t1)
    };

    assert!(
        lines[2].contains(&first_task.summary.task.as_ref().unwrap().title),
        "line 2: {}",
        lines[2]
    );
    assert!(
        lines[2].contains(&format!("[{}]", first_task.summary.context)),
        "line 2: {}",
        lines[2]
    );
    assert!(
        lines[3].contains(&second_task.summary.task.as_ref().unwrap().title),
        "line 3: {}",
        lines[3]
    );
    assert!(
        lines[3].contains(&format!("[{}]", second_task.summary.context)),
        "line 3: {}",
        lines[3]
    );

    // 6. Navigate: press 'j' to select second task
    state.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    assert_eq!(state.current_selected(), 1);
    assert_eq!(
        state.selected_task().map(|t| &t.id),
        Some(&second_task.summary.id)
    );

    // 7. Press Enter to open task detail page
    let effect = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(effect, AppEffect::FetchTask(second_task.summary.id.clone()));

    // Fetch the task detail from loomd and populate the open page
    let task_path = format!("/v1/tasks/{}", second_task.summary.id);
    let detail: TaskDetail = client.get(&task_path).await.unwrap();
    if let Some(page) = state.page_stack.last_mut() {
        page.content = ObjectPageContent::Task(Box::new(detail));
    }

    // Render task page
    terminal.draw(|f| render(f, &state)).unwrap();
    let lines = buffer_text_lines(&terminal);
    assert!(lines[1].contains("Task"), "line 1: {}", lines[1]);
    assert!(
        lines[2].contains(&second_task.summary.id.to_string()),
        "line 2: {}",
        lines[2]
    );
    assert!(
        lines[2].contains(&format!("[{}]", second_task.summary.context)),
        "line 2: {}",
        lines[2]
    );
    assert!(
        lines[4].contains(&second_task.summary.task.as_ref().unwrap().title),
        "line 4: {}",
        lines[4]
    );

    // 8. Press Esc to go back to Inbox queue view
    state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(state.page_stack.len(), 0);

    // 9. Press '/' to open Search modal and search real loomd
    state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    state.handle_key(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE));
    state.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    state.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(state.search_query, "Fix");

    let search_path = search_api_path(&["Fix".to_string()], None, None, 10);
    let search_res: SearchResults = client.get(&search_path).await.unwrap();
    state.search_results = search_res.results;

    terminal.draw(|f| render(f, &state)).unwrap();
    let lines = buffer_text_lines(&terminal);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Query") || l.contains("Live search")),
        "lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("Fix navigation bar")),
        "lines: {lines:?}"
    );

    // 10. Close search and test SSE stream with new task
    state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    // Add a new task through CLI, which triggers loomd to emit an SSE event
    sandbox.add(&["Third task via SSE", "--context", "personal"]);

    // Read SSE frame from the stream
    let frame = tokio::time::timeout(Duration::from_secs(5), sse_stream.next())
        .await
        .expect("SSE event timeout")
        .expect("SSE error")
        .expect("SSE stream ended");
    let effect = state.apply_change_event(&frame.event);
    assert_eq!(effect, AppEffect::Refresh);

    // Refetch Inbox queue and verify update
    let updated_inbox: TaskList = client.get(&inbox_path).await.unwrap();
    state.inbox = updated_inbox.tasks;
    assert_eq!(state.current_queue_len(), 3);

    // 11. Press 'q' to quit
    state.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(state.should_quit);
}
