//! Entrypoint and runtime loop of the interactive TUI (`loom` with no subcommand).

use std::collections::BTreeMap;
use std::env;
use std::io::{self, IsTerminal as _};
use std::path::PathBuf;
use std::time::Duration;

use clap::CommandFactory as _;
use crossterm::event::{Event, KeyEventKind};
use futures_util::{FutureExt as _, StreamExt as _};
use loom::cli::{Cli, GlobalArgs};
use loom::client::Client;
use loom::conventions::{Exit, use_color};
use loom::edit;
use loom::search::search_api_path;
use loom::task::list_api_path;
use loom::tui::actions::{
    ActionRequest, ActionResponse, Next, Submission, execute_action, submit, take_next,
};
use loom::tui::color::Theme;
use loom::tui::events::EventsLoad;
use loom::tui::relations::{RelationsData, RelationsLoad};
use loom::tui::render::render;
use loom::tui::state::{AppEffect, AppState, ObjectPageContent, ObjectTarget, PageTab, SseState};
use loom::tui::terminal::{SuspendError, TerminalGuard, repaint_all, while_suspended};
use loom_api::{
    ContextName, DEFAULT_SEARCH_LIMIT, EventCursor, SearchResults, TaskDetail, TaskEvents, TaskId,
    TaskList,
};
use miette::{IntoDiagnostic as _, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, watch};

use crate::task_cmd::{cd_file_from_env, local_today};
use crate::{load_config, print, repo_cmd};

/// Longest wait for a running action when the TUI quits.
const ACTION_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for task/search requests made by the TUI.
const TUI_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Initial delay before reconnecting a dropped SSE stream.
const SSE_INITIAL_RECONNECT_DELAY: Duration = Duration::from_millis(500);
/// Maximum backoff delay before reconnecting a dropped SSE stream.
const SSE_MAX_RECONNECT_DELAY: Duration = Duration::from_secs(5);
/// Inactivity pause before sending a debounced live search query.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
/// How long a toast stays on the bottom bar (docs/design/ui-style.md, 4.8).
const TOAST_LIFETIME: Duration = Duration::from_secs(4);

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
    TaskEvents {
        id: TaskId,
        result: Box<Result<TaskEvents, String>>,
    },
    TaskRelations {
        id: TaskId,
        result: Box<Result<RelationsData, String>>,
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
    let stores = env::home_dir()
        .map(|home| config.store_paths(&layout.home, &home))
        .unwrap_or_default();
    let mut state = AppState::new(theme, global.context.clone(), contexts, local_today())
        .with_default_context(default_context)
        .with_time_zone(jiff::tz::TimeZone::system());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;

    runtime.block_on(async {
        // Starts loomd if not running, matching every other CLI command
        let client = repo_cmd::connect(&layout, true)
            .await?
            .with_timeout(TUI_REQUEST_TIMEOUT);
        run_tui_loop(&client, &mut state, &stores).await
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "event loop handles input, background workers, SSE and timers in one select!"
)]
async fn run_tui_loop(
    client: &Client,
    state: &mut AppState,
    stores: &BTreeMap<ContextName, PathBuf>,
) -> Result<Exit> {
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

    // 2b. Watch channel for the event log of the task whose Events tab is open.
    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<TaskId>();
    let events_client = client.clone();
    let (events_done_tx, mut events_done_rx) = mpsc::channel::<FetchResponse>(16);
    tokio::spawn(async move {
        while let Some(id) = events_rx.recv().await {
            let result = events_client
                .get::<TaskEvents>(&format!("/v1/tasks/{id}/events"))
                .await
                .map_err(|e| e.to_string());
            let _ = events_done_tx
                .send(FetchResponse::TaskEvents {
                    id,
                    result: Box::new(result),
                })
                .await;
        }
    });

    // 2c. Watch channel for the relations of the task whose Relations tab is open.
    let (relations_tx, mut relations_rx) = mpsc::unbounded_channel::<TaskId>();
    let relations_client = client.clone();
    let (relations_done_tx, mut relations_done_rx) = mpsc::channel::<FetchResponse>(16);
    tokio::spawn(async move {
        while let Some(id) = relations_rx.recv().await {
            let result = loom::tui::load::fetch_relations(&relations_client, &id).await;
            let _ = relations_done_tx
                .send(FetchResponse::TaskRelations {
                    id,
                    result: Box::new(result),
                })
                .await;
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

    // `None` while the editor runs: the stream must not read the keys of the editor.
    let mut event_stream = Some(crossterm::event::EventStream::new());
    let mut pending_search: Option<String> = None;
    let mut search_timer = Box::pin(tokio::time::sleep(SEARCH_DEBOUNCE));
    // The toast (by generation) that the timer expires; a new toast restarts the timer.
    let mut shown_toast: Option<u64> = None;
    let mut toast_timer = Box::pin(tokio::time::sleep(TOAST_LIFETIME));
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
            Some(event_res) = next_terminal_event(&mut event_stream) => {
                if let Ok(event) = event_res {
                    match event {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            state.set_local_date(local_today());
                            let mut effect = state.handle_key(key);
                            if let AppEffect::EditBody(id) = effect {
                                drop(event_stream.take());
                                edit_in_editor(&id, client, stores, &guard, &mut terminal, &mut sigint, state)
                                    .await?;
                                event_stream = Some(crossterm::event::EventStream::new());
                                effect = AppEffect::Refresh;
                            }
                            handle_effect(
                                effect,
                                state,
                                &refresh_tx,
                                &task_tx,
                                &events_tx,
                                &relations_tx,
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
                            &events_tx,
                            &relations_tx,
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

            // Expire the toast
            () = &mut toast_timer, if shown_toast.is_some() => {
                if state.shown_toast() == shown_toast {
                    state.clear_status();
                    needs_redraw = true;
                }
                shown_toast = None;
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

            // Handle event logs for the Events tab
            Some(events_resp) = events_done_rx.recv() => {
                if let FetchResponse::TaskEvents { id, result } = events_resp {
                    state.apply_events(&id, *result);
                    needs_redraw = true;
                }
            }

            // Handle relations for the Relations tab
            Some(relations_resp) = relations_done_rx.recv() => {
                if let FetchResponse::TaskRelations { id, result } = relations_resp {
                    state.apply_relations(&id, *result);
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
                let action_ok = action_resp.is_ok();
                match action_resp {
                    Ok(outcome) => {
                        let target = outcome.next_undo_target(state.last_action.as_ref());
                        state.set_last_action(target);
                        if let Some(msg) = outcome.message {
                            state.set_success(msg);
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
                                &events_tx,
                                &relations_tx,
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
                let succeeded = action_ok;
                match take_next(&mut state.queued_action, succeeded, state.last_action.as_ref()) {
                    Next::Nothing => {}
                    Next::Send(next) => {
                        submit_action(next, state, &action_tx, &mut action_in_flight);
                    }
                    Next::Dropped(reason) => state.set_status(reason),
                }
                needs_redraw = true;
            }
        }

        if state.shown_toast() != shown_toast {
            shown_toast = state.shown_toast();
            if shown_toast.is_some() {
                toast_timer
                    .as_mut()
                    .reset(tokio::time::Instant::now() + TOAST_LIFETIME);
            }
        }

        if needs_redraw {
            terminal.draw(|f| render(f, state)).into_diagnostic()?;
        }
    }

    // An action that runs ends first, so that quitting does not cut a change to a task.
    // The action that waits behind it does not run.
    state.queued_action = None;
    if action_in_flight {
        let _ = tokio::time::timeout(ACTION_DRAIN_TIMEOUT, action_done_rx.recv()).await;
    }

    drop(guard);
    Ok(exit)
}

/// The next event of the terminal; never ends while the stream is off.
async fn next_terminal_event(
    stream: &mut Option<crossterm::event::EventStream>,
) -> Option<io::Result<Event>> {
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

/// Takes a signal that is already waiting, without waiting for one. Returns whether there was
/// one.
fn drain_signal(signal: &mut tokio::signal::unix::Signal) -> bool {
    signal.recv().now_or_never().flatten().is_some()
}

/// `e` on the task page: gives the terminal to the editor on the task file, takes it back, and
/// tells the result in the status bar. The steps are those of `loom edit` (`loom::edit`).
///
/// `Ctrl-C` goes to the editor: it is a key of the editor, and the editor decides what it does
/// (a `SIGINT` to the whole process group also reaches loom). So the `SIGINT` that arrived
/// while the editor ran is dropped when the editor ends. `SIGTERM` is a request to stop and is
/// kept: the loop handles it as soon as the editor ends.
///
/// While the editor runs, change events wait in their channel, and the SSE worker waits behind
/// them. The caller reloads the queues and the page when this returns, so no change is lost.
///
/// # Errors
///
/// The terminal cannot be taken back: the TUI must stop.
async fn edit_in_editor(
    id: &TaskId,
    client: &Client,
    stores: &BTreeMap<ContextName, PathBuf>,
    guard: &TerminalGuard,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    sigint: &mut tokio::signal::unix::Signal,
    state: &mut AppState,
) -> Result<()> {
    let session = match edit::begin(client, &id.to_string(), stores).await {
        Ok(session) => session,
        Err(error) => {
            state.set_error(error.to_string());
            return Ok(());
        }
    };
    let editor_session = session.clone();
    let work = async move {
        tokio::task::spawn_blocking(move || {
            edit::run_editor(
                &editor_session,
                env::var("VISUAL").ok().as_deref(),
                env::var("EDITOR").ok().as_deref(),
            )
        })
        .await
    };
    let ran = match while_suspended(|| guard.suspend(), || guard.resume(), work).await {
        Ok(ran) => ran,
        Err(SuspendError::Suspend(error)) => {
            state.set_error(format!("cannot leave the terminal to the editor: {error}"));
            return Ok(());
        }
        Err(SuspendError::Resume(error)) => {
            return Err(miette::miette!("cannot take the terminal back: {error}"));
        }
    };
    // One turn of the scheduler first, so that a signal that is on its way is counted.
    tokio::task::yield_now().await;
    drain_signal(sigint);
    match ran {
        Ok(Ok(())) => match edit::finish(client, &session).await {
            // The save worked: a warning is information, not an error.
            Ok(outcome) if !outcome.warnings.is_empty() => {
                state.set_status(outcome.warnings.join("; "));
            }
            // Not recorded in time, or unchanged: information, with no check mark.
            Ok(outcome) if outcome.unchanged || !outcome.recorded => {
                state.set_status(outcome.text.lines().next().unwrap_or_default());
            }
            Ok(outcome) => state.set_success(outcome.text.lines().next().unwrap_or_default()),
            Err(error) => state.set_error(error.to_string()),
        },
        Ok(Err(error)) => state.set_error(error.to_string()),
        Err(error) => state.set_error(format!("the editor did not finish: {error}")),
    }
    // The editor drew on the screen: the next draw must write all of it again.
    if let Err(error) = repaint_all(terminal) {
        state.set_error(format!("cannot clear the screen: {error}"));
    }
    Ok(())
}

/// Sends an action to the worker. While another action runs, keeps the request for after it.
fn submit_action(
    request: ActionRequest,
    state: &mut AppState,
    action_tx: &mpsc::Sender<ActionRequest>,
    action_in_flight: &mut bool,
) {
    match submit(*action_in_flight, &mut state.queued_action, request) {
        Submission::Send(request) => {
            if action_tx.try_send(request).is_ok() {
                *action_in_flight = true;
            } else {
                state.set_error("cannot reach the action worker");
            }
        }
        Submission::Kept => {}
        Submission::Dropped => state.set_status("action in progress"),
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

/// Asks for the data of the tab of the top page, when it is the Events or the Relations tab.
fn fetch_tab_data(
    state: &mut AppState,
    events_tx: &mpsc::UnboundedSender<TaskId>,
    relations_tx: &mpsc::UnboundedSender<TaskId>,
) {
    let Some(page) = state.page_stack.last_mut() else {
        return;
    };
    let ObjectTarget::Task(id) = &page.target;
    match page.tab {
        PageTab::Events => {
            if !matches!(page.events.load, EventsLoad::Loaded(_)) {
                page.events.load = EventsLoad::Loading;
            }
            let _ = events_tx.send(id.clone());
        }
        PageTab::Relations => {
            if !matches!(page.relations.load, RelationsLoad::Loaded(_)) {
                page.relations.load = RelationsLoad::Loading;
            }
            let _ = relations_tx.send(id.clone());
        }
        PageTab::Overview => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_effect(
    effect: AppEffect,
    state: &mut AppState,
    refresh_tx: &mpsc::Sender<RefreshRequest>,
    task_tx: &watch::Sender<Option<TaskId>>,
    events_tx: &mpsc::UnboundedSender<TaskId>,
    relations_tx: &mpsc::UnboundedSender<TaskId>,
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
            // A change of the task adds events or moves relations: read them again while their
            // tab is open.
            fetch_tab_data(state, events_tx, relations_tx);
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
        AppEffect::FetchEvents(id) => {
            let _ = events_tx.send(id);
        }
        AppEffect::FetchRelations(id) => {
            let _ = relations_tx.send(id);
        }
        AppEffect::ReloadPage => {
            // A page that is the top page again shows what it showed before: read its task and
            // the data of its tab again. This also repairs a page whose first read was
            // replaced by a later one.
            if let Some(page) = state.page_stack.last() {
                let ObjectTarget::Task(id) = &page.target;
                let _ = task_tx.send(Some(id.clone()));
            }
            fetch_tab_data(state, events_tx, relations_tx);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_signal_that_arrived_while_the_editor_ran_is_dropped() {
        let mut signal = signal(SignalKind::user_defined1()).unwrap();
        assert!(!drain_signal(&mut signal), "nothing waits yet");
        let status = std::process::Command::new("kill")
            .args(["-USR1", &std::process::id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(drain_signal(&mut signal), "the signal waits");
        assert!(!drain_signal(&mut signal), "and is gone once taken");
    }
}
