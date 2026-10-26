//! Entrypoint and runtime loop of the interactive TUI (`loom` with no subcommand).

use std::env;
use std::io::{self, IsTerminal as _};
use std::path::PathBuf;
use std::time::Duration;

use clap::CommandFactory as _;
use crossterm::event::{Event, KeyEventKind};
use futures_util::StreamExt as _;
use loom::cli::{Cli, GlobalArgs};
use loom::client::Client;
use loom::conventions::{Exit, use_color};
use loom::search::search_api_path;
use loom::task::list_api_path;
use loom::tui::actions::{ActionRequest, ActionResponse, execute_action};
use loom::tui::color::Theme;
use loom::tui::render::render;
use loom::tui::state::{AppEffect, AppState, ObjectPageContent, ObjectTarget, SseState};
use loom::tui::terminal::TerminalGuard;
use loom_api::{
    ContextName, DEFAULT_SEARCH_LIMIT, EventCursor, SearchResults, TaskDetail, TaskId, TaskList,
};
use miette::{IntoDiagnostic as _, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, watch};

use crate::task_cmd::{cd_file_from_env, local_today};
use crate::{load_config, print, repo_cmd};

/// Timeout for task/search requests made by the TUI.
const TUI_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Initial delay before reconnecting a dropped SSE stream.
const SSE_INITIAL_RECONNECT_DELAY: Duration = Duration::from_millis(500);
/// Maximum backoff delay before reconnecting a dropped SSE stream.
const SSE_MAX_RECONNECT_DELAY: Duration = Duration::from_secs(5);
/// Inactivity pause before sending a debounced live search query.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);

struct RefreshRequest {
    context: Option<ContextName>,
    open_id: Option<TaskId>,
}

struct RefreshedData {
    today: Result<TaskList, String>,
    inbox: Result<TaskList, String>,
    waiting: Result<TaskList, String>,
    open: Option<(TaskId, Result<TaskDetail, String>)>,
}

enum FetchResponse {
    TaskDetail {
        id: TaskId,
        result: Box<Result<TaskDetail, String>>,
    },
    SearchResults {
        query: String,
        results: Result<SearchResults, String>,
    },
}

enum SseMessage {
    Frame(loom::sse::SseFrame),
    Connected,
    Disconnected,
}

/// Runs `loom` when no subcommand is given: prints help on a non-TTY, or opens the TUI.
pub fn run(global: &GlobalArgs) -> Result<Exit> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        let help = format!("{}\n", Cli::command().render_help());
        print(&help)?;
        return Ok(Exit::Success);
    }

    let layout = crate::layout()?;
    let config = load_config(&layout.config)?;
    let color_enabled = use_color(
        global.color,
        env::var_os("NO_COLOR").as_deref(),
        io::stdout().is_terminal(),
    );
    let theme = Theme::from_config(&config.contexts, color_enabled);
    let contexts: Vec<ContextName> = config.contexts.keys().cloned().collect();
    let default_context = config.default_context.as_ref().map(|s| s.get_ref().clone());
    let mut state = AppState::new(theme, global.context.clone(), contexts, local_today())
        .with_default_context(default_context);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;

    runtime.block_on(async {
        // Starts loomd if not running, matching every other CLI command
        let client = repo_cmd::connect(&layout, true)
            .await?
            .with_timeout(TUI_REQUEST_TIMEOUT);
        run_tui_loop(&client, &mut state).await
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "event loop handles input, background workers, SSE and timers in one select!"
)]
async fn run_tui_loop(client: &Client, state: &mut AppState) -> Result<Exit> {
    let guard = TerminalGuard::enter().into_diagnostic()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).into_diagnostic()?;

    let mut sigterm = signal(SignalKind::terminate()).into_diagnostic()?;
    let mut sigint = signal(SignalKind::interrupt()).into_diagnostic()?;

    // 1. Spawn asynchronous refresh worker.
    let (refresh_tx, refresh_rx) = mpsc::channel::<RefreshRequest>(16);
    let (refresh_done_tx, mut refresh_done_rx) = mpsc::channel::<Box<RefreshedData>>(16);
    let refresh_client = client.clone();
    tokio::spawn(async move {
        run_refresh_worker(refresh_client, refresh_rx, refresh_done_tx).await;
    });

    // 2. Watch channel for single-task detail/preview fetching (j/k navigation).
    let (task_tx, mut task_rx) = watch::channel::<Option<TaskId>>(None);
    let task_client = client.clone();
    let (task_done_tx, mut task_done_rx) = mpsc::channel::<FetchResponse>(16);
    tokio::spawn(async move {
        while task_rx.changed().await.is_ok() {
            let maybe_id = task_rx.borrow().clone();
            if let Some(id) = maybe_id {
                let path = format!("/v1/tasks/{id}");
                let result = task_client
                    .get::<TaskDetail>(&path)
                    .await
                    .map_err(|e| e.to_string());
                let _ = task_done_tx
                    .send(FetchResponse::TaskDetail {
                        id,
                        result: Box::new(result),
                    })
                    .await;
            }
        }
    });

    // 3. Watch channel for live search queries (decoupled from queue refreshes).
    let (search_tx, mut search_rx) = watch::channel::<Option<(String, Option<ContextName>)>>(None);
    let search_client = client.clone();
    let (search_done_tx, mut search_done_rx) = mpsc::channel::<FetchResponse>(16);
    tokio::spawn(async move {
        while search_rx.changed().await.is_ok() {
            let maybe_req = search_rx.borrow().clone();
            if let Some((query, context)) = maybe_req {
                if query.trim().is_empty() {
                    let _ = search_done_tx
                        .send(FetchResponse::SearchResults {
                            query,
                            results: Ok(SearchResults {
                                results: Vec::new(),
                            }),
                        })
                        .await;
                    continue;
                }
                let words: Vec<String> = query.split_whitespace().map(ToOwned::to_owned).collect();
                let path = search_api_path(&words, context.as_ref(), None, DEFAULT_SEARCH_LIMIT);
                let results = search_client
                    .get::<SearchResults>(&path)
                    .await
                    .map_err(|e| e.to_string());
                let _ = search_done_tx
                    .send(FetchResponse::SearchResults { query, results })
                    .await;
            }
        }
    });

    // 4. Spawn background SSE worker so SSE connection/reconnection never blocks the UI loop.
    let (sse_tx, mut sse_rx) = mpsc::channel::<SseMessage>(32);
    let sse_client = client.clone();
    tokio::spawn(async move {
        run_sse_worker(sse_client, sse_tx).await;
    });

    // 5. Spawn action worker for task triage operations (accept, snooze, cancel, done, go, undo, create).
    let (action_tx, action_rx) = mpsc::channel::<ActionRequest>(16);
    let (action_done_tx, mut action_done_rx) = mpsc::channel::<ActionResponse>(16);
    let action_client = client.clone();
    let cd_file = cd_file_from_env();
    tokio::spawn(async move {
        run_action_worker(action_client, action_rx, action_done_tx, cd_file).await;
    });

    // 6. Initial draw and trigger initial refresh.
    terminal.draw(|f| render(f, state)).into_diagnostic()?;

    let mut refresh_queued = false;
    let _ = refresh_tx.try_send(RefreshRequest {
        context: state.active_context.clone(),
        open_id: None,
    });
    let mut fetch_in_flight = true;
    let mut action_in_flight = false;

    let mut event_stream = crossterm::event::EventStream::new();
    let mut pending_search: Option<String> = None;
    let mut search_timer = Box::pin(tokio::time::sleep(SEARCH_DEBOUNCE));
    let mut exit = Exit::Success;

    while !state.should_quit {
        let mut needs_redraw = false;

        tokio::select! {
            _ = sigterm.recv() => {
                break;
            }
            _ = sigint.recv() => {
                exit = Exit::Interrupted;
                break;
            }

            // Handle crossterm terminal events
            Some(event_res) = event_stream.next() => {
                if let Ok(event) = event_res {
                    match event {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            state.set_local_date(local_today());
                            let effect = state.handle_key(key);
                            handle_effect(
                                effect,
                                state,
                                &refresh_tx,
                                &task_tx,
                                &mut fetch_in_flight,
                                &mut refresh_queued,
                                &mut pending_search,
                                &mut search_timer,
                                &action_tx,
                                &mut action_in_flight,
                            );
                            needs_redraw = true;
                        }
                        Event::Resize(_, _) => {
                            needs_redraw = true;
                        }
                        _ => {}
                    }
                }
            }

            // Handle background SSE frames and connection updates
            Some(sse_msg) = sse_rx.recv() => {
                match sse_msg {
                    SseMessage::Connected => {
                        state.sse_state = SseState::Connected;
                        needs_redraw = true;
                    }
                    SseMessage::Disconnected => {
                        state.sse_state = SseState::Reconnecting;
                        needs_redraw = true;
                    }
                    SseMessage::Frame(frame) => {
                        let effect = state.apply_change_event(&frame.event);
                        handle_effect(
                            effect,
                            state,
                            &refresh_tx,
                            &task_tx,
                            &mut fetch_in_flight,
                            &mut refresh_queued,
                            &mut pending_search,
                            &mut search_timer,
                            &action_tx,
                            &mut action_in_flight,
                        );
                        needs_redraw = true;
                    }
                }
            }

            // Handle debounced search query trigger
            () = &mut search_timer, if pending_search.is_some() => {
                if let Some(query) = pending_search.take() {
                    let _ = search_tx.send(Some((query, state.active_context.clone())));
                }
            }

            // Handle search responses
            Some(search_resp) = search_done_rx.recv() => {
                if let FetchResponse::SearchResults { query, results } = search_resp
                    && state.search_query == query
                {
                    match results {
                        Ok(res) => state.search_results = res.results,
                        Err(err) => state.set_error(err),
                    }
                    needs_redraw = true;
                }
            }

            // Handle single-task responses from preview/detail fetches
            Some(task_resp) = task_done_rx.recv() => {
                if let FetchResponse::TaskDetail { id, result } = task_resp {
                    match *result {
                        Ok(detail) => {
                            if let Some(page) = state.page_stack.last_mut() {
                                let ObjectTarget::Task(target_id) = &page.target;
                                if target_id == &id {
                                    page.content = ObjectPageContent::Task(Box::new(detail.clone()));
                                }
                            }
                            if state.selected_task().map(|t| &t.id) == Some(&id) {
                                state.preview_detail = Some(detail);
                            }
                        }
                        Err(err) => {
                            state.set_error(err);
                        }
                    }
                    needs_redraw = true;
                }
            }

            // Handle refresh worker responses
            Some(data) = refresh_done_rx.recv() => {
                fetch_in_flight = false;
                let all_ok = data.today.is_ok()
                    && data.inbox.is_ok()
                    && data.waiting.is_ok()
                    && data.open.as_ref().is_none_or(|(_, r)| r.is_ok());

                match data.today {
                    Ok(list) => {
                        state.today = list.tasks;
                        if state.group_by_parent() {
                            state.sort_today_by_parent();
                        }
                    }
                    Err(err) => state.set_error(err),
                }
                match data.inbox {
                    Ok(list) => state.inbox = list.tasks,
                    Err(err) => state.set_error(err),
                }
                match data.waiting {
                    Ok(list) => state.waiting = list.tasks,
                    Err(err) => state.set_error(err),
                }
                if let Some((open_id, open_res)) = data.open
                    && let Some(page) = state.page_stack.last_mut()
                {
                    let ObjectTarget::Task(target_id) = &page.target;
                    if target_id == &open_id {
                        match open_res {
                            Ok(detail) => {
                                page.content = ObjectPageContent::Task(Box::new(detail));
                            }
                            Err(err) => state.set_error(err),
                        }
                    }
                }

                if all_ok {
                    state.clear_error();
                }

                // Also request preview task for currently selected row
                if let Some(task) = state.selected_task() {
                    let _ = task_tx.send(Some(task.id.clone()));
                } else {
                    state.preview_detail = None;
                }

                // Trigger next refresh if one was queued while this one was running
                if refresh_queued {
                    refresh_queued = false;
                    let open_id = state.page_stack.last().map(|p| {
                        let ObjectTarget::Task(id) = &p.target;
                        id.clone()
                    });
                    let _ = refresh_tx.try_send(RefreshRequest {
                        context: state.active_context.clone(),
                        open_id,
                    });
                    fetch_in_flight = true;
                }
                needs_redraw = true;
            }

            // Handle action worker responses
            Some(action_resp) = action_done_rx.recv() => {
                action_in_flight = false;
                match action_resp {
                    Ok(outcome) => {
                        if let Some(task_id) = outcome.task_id {
                            state.record_action_success(task_id);
                        }
                        if outcome.undone {
                            state.set_last_action(None);
                        }
                        if let Some(msg) = outcome.message {
                            state.status_message = Some(msg);
                        }
                        if outcome.should_quit {
                            state.should_quit = true;
                        }
                        if outcome.should_refresh {
                            handle_effect(
                                AppEffect::Refresh,
                                state,
                                &refresh_tx,
                                &task_tx,
                                &mut fetch_in_flight,
                                &mut refresh_queued,
                                &mut pending_search,
                                &mut search_timer,
                                &action_tx,
                                &mut action_in_flight,
                            );
                        }
                    }
                    Err(err) => {
                        state.set_error(err);
                    }
                }
                if let Some(next) = state.queued_action.take() {
                    submit_action(next, state, &action_tx, &mut action_in_flight);
                }
                needs_redraw = true;
            }
        }

        if needs_redraw {
            terminal.draw(|f| render(f, state)).into_diagnostic()?;
        }
    }

    drop(guard);
    Ok(exit)
}

/// Sends an action to the worker. While another action runs, keeps the request for after it.
fn submit_action(
    request: ActionRequest,
    state: &mut AppState,
    action_tx: &mpsc::Sender<ActionRequest>,
    action_in_flight: &mut bool,
) {
    if *action_in_flight {
        if state.queued_action.is_some() {
            state.set_status("action in progress");
        } else {
            state.queued_action = Some(request);
        }
        return;
    }
    if action_tx.try_send(request).is_ok() {
        *action_in_flight = true;
    } else {
        state.set_error("cannot reach the action worker");
    }
}

async fn run_action_worker(
    client: Client,
    mut requests: mpsc::Receiver<ActionRequest>,
    responses: mpsc::Sender<ActionResponse>,
    cd_file: Option<PathBuf>,
) {
    while let Some(request) = requests.recv().await {
        let response = execute_action(&client, request, cd_file.as_deref()).await;
        if responses.send(response).await.is_err() {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_effect(
    effect: AppEffect,
    state: &mut AppState,
    refresh_tx: &mpsc::Sender<RefreshRequest>,
    task_tx: &watch::Sender<Option<TaskId>>,
    fetch_in_flight: &mut bool,
    refresh_queued: &mut bool,
    pending_search: &mut Option<String>,
    search_timer: &mut std::pin::Pin<Box<tokio::time::Sleep>>,
    action_tx: &mpsc::Sender<ActionRequest>,
    action_in_flight: &mut bool,
) {
    match effect {
        AppEffect::None => {}
        AppEffect::Refresh => {
            if *fetch_in_flight {
                *refresh_queued = true;
            } else {
                let open_id = state.page_stack.last().map(|p| {
                    let ObjectTarget::Task(id) = &p.target;
                    id.clone()
                });
                let _ = refresh_tx.try_send(RefreshRequest {
                    context: state.active_context.clone(),
                    open_id,
                });
                *fetch_in_flight = true;
            }
        }
        AppEffect::FetchTask(id) => {
            let _ = task_tx.send(Some(id));
        }
        AppEffect::Search(query) => {
            *pending_search = Some(query);
            *search_timer = Box::pin(tokio::time::sleep(SEARCH_DEBOUNCE));
        }
        effect => {
            if let Ok(request) = ActionRequest::from_effect(effect) {
                submit_action(request, state, action_tx, action_in_flight);
            }
        }
    }
}

async fn run_refresh_worker(
    client: Client,
    mut requests: mpsc::Receiver<RefreshRequest>,
    responses: mpsc::Sender<Box<RefreshedData>>,
) {
    while let Some(req) = requests.recv().await {
        let ctx_ref = req.context.as_ref();
        let today_path = list_api_path(ctx_ref, None, false);
        let inbox_path = list_api_path(ctx_ref, Some("inbox"), false);
        let waiting_path = list_api_path(ctx_ref, Some("waiting"), false);

        let today = client
            .get::<TaskList>(&today_path)
            .await
            .map_err(|e| e.to_string());
        let inbox = client
            .get::<TaskList>(&inbox_path)
            .await
            .map_err(|e| e.to_string());
        let waiting = client
            .get::<TaskList>(&waiting_path)
            .await
            .map_err(|e| e.to_string());

        let open = if let Some(id) = req.open_id {
            let path = format!("/v1/tasks/{id}");
            let res = client
                .get::<TaskDetail>(&path)
                .await
                .map_err(|e| e.to_string());
            Some((id, res))
        } else {
            None
        };

        let _ = responses
            .send(Box::new(RefreshedData {
                today,
                inbox,
                waiting,
                open,
            }))
            .await;
    }
}

async fn run_sse_worker(client: Client, sse_tx: mpsc::Sender<SseMessage>) {
    let mut cursor: Option<EventCursor> = None;
    let mut reconnect_delay = SSE_INITIAL_RECONNECT_DELAY;
    loop {
        match client.subscribe_events(None, cursor).await {
            Ok(mut stream) => {
                reconnect_delay = SSE_INITIAL_RECONNECT_DELAY;
                let _ = sse_tx.send(SseMessage::Connected).await;
                while let Ok(Some(frame)) = stream.next().await {
                    if let Some(id) = frame.id {
                        cursor = Some(id);
                    }
                    if sse_tx.send(SseMessage::Frame(frame)).await.is_err() {
                        return;
                    }
                }
                // Update cursor from stream before reconnecting
                if let Some(id) = stream.last_event_id() {
                    cursor = Some(id);
                }
                let _ = sse_tx.send(SseMessage::Disconnected).await;
            }
            Err(_) => {
                let _ = sse_tx.send(SseMessage::Disconnected).await;
            }
        }
        tokio::time::sleep(reconnect_delay).await;
        reconnect_delay = (reconnect_delay * 2).min(SSE_MAX_RECONNECT_DELAY);
    }
}
