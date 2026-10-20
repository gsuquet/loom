//! Entrypoint and runtime loop of the interactive TUI (`loom` with no subcommand).

use std::env;
use std::io::{self, IsTerminal as _};
use std::time::Duration;

use clap::CommandFactory as _;
use crossterm::event::{Event, KeyEventKind};
use futures_util::StreamExt as _;
use loom::cli::{Cli, GlobalArgs};
use loom::conventions::use_color;
use loom::search::search_api_path;
use loom::task::list_api_path;
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
use tokio::sync::mpsc;

use crate::client::{Client, EventStream};
use crate::{load_config, print};

/// Timeout for task/search requests made by the TUI.
const TUI_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Delay before reconnecting a dropped SSE stream.
const SSE_RECONNECT_DELAY: Duration = Duration::from_millis(500);
/// Inactivity pause before sending a debounced live search query.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);

enum FetchRequest {
    Refresh {
        context: Option<ContextName>,
        preview_id: Option<TaskId>,
        open_id: Option<TaskId>,
    },
    Search {
        query: String,
        context: Option<ContextName>,
    },
}

struct RefreshedData {
    today: Result<TaskList, String>,
    inbox: Result<TaskList, String>,
    waiting: Result<TaskList, String>,
    preview: Option<Result<TaskDetail, String>>,
    open: Option<(TaskId, Result<TaskDetail, String>)>,
}

enum FetchResponse {
    Refreshed(Box<RefreshedData>),
    SearchResults {
        query: String,
        results: Result<SearchResults, String>,
    },
}

/// Runs `loom` when no subcommand is given: prints help on a non-TTY, or opens the TUI.
pub fn run(global: &GlobalArgs) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        let help = format!("{}\n", Cli::command().render_help());
        return print(&help);
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
    let mut state = AppState::new(theme, global.context.clone(), contexts);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;

    runtime.block_on(async {
        let socket = layout.socket;
        let client = Client::new(socket).with_timeout(TUI_REQUEST_TIMEOUT);
        run_tui_loop(&client, &mut state).await
    })
}

#[allow(clippy::too_many_lines)]
async fn run_tui_loop(client: &Client, state: &mut AppState) -> Result<()> {
    let guard = TerminalGuard::enter().into_diagnostic()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).into_diagnostic()?;

    // 1. Subscribe to SSE events before the initial fetch so no updates are missed.
    let mut sse_stream: Option<EventStream> = client.subscribe_events(None, None).await.ok();
    state.sse_state = if sse_stream.is_some() {
        SseState::Connected
    } else {
        SseState::Reconnecting
    };
    let mut last_event_id: Option<EventCursor> = None;
    let mut reconnect_tick = tokio::time::interval(SSE_RECONNECT_DELAY);

    // 2. Spawn the asynchronous background worker to decouple HTTP fetches from the UI loop.
    let (worker_tx, worker_rx) = mpsc::channel::<FetchRequest>(32);
    let (response_tx, mut response_rx) = mpsc::channel::<FetchResponse>(32);
    let worker_client = client.clone();
    tokio::spawn(async move {
        run_fetch_worker(worker_client, worker_rx, response_tx).await;
    });

    // 3. Queue the initial refresh.
    let mut fetch_in_flight = false;
    let mut refresh_queued = false;

    let trigger_refresh = |worker_tx: &mpsc::Sender<FetchRequest>,
                           state: &AppState,
                           fetch_in_flight: &mut bool,
                           refresh_queued: &mut bool| {
        if *fetch_in_flight {
            *refresh_queued = true;
            return;
        }
        *fetch_in_flight = true;
        let preview_id = if state.preview_enabled {
            state.selected_task().map(|t| t.id.clone())
        } else {
            None
        };
        let open_id = state.page_stack.last().map(|p| match &p.target {
            ObjectTarget::Task(id) => id.clone(),
        });
        let req = FetchRequest::Refresh {
            context: state.active_context.clone(),
            preview_id,
            open_id,
        };
        let _ = worker_tx.try_send(req);
    };

    trigger_refresh(&worker_tx, state, &mut fetch_in_flight, &mut refresh_queued);

    // 4. Setup event-driven crossterm input and signals.
    let mut event_stream = crossterm::event::EventStream::new();
    let mut needs_redraw = true;

    let mut pending_search: Option<String> = None;
    let mut debounce_deadline: Option<tokio::time::Instant> = None;

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .into_diagnostic()?;

    let loop_result: Result<()> = async {
        loop {
            if needs_redraw {
                terminal
                    .draw(|frame| render(frame, state))
                    .into_diagnostic()?;
                needs_redraw = false;
            }

            if state.should_quit {
                break;
            }

            tokio::select! {
                // Immediate termination signals
                _ = tokio::signal::ctrl_c() => {
                    break;
                }
                () = wait_sigterm(&mut sigterm) => {
                    break;
                }

                // Terminal user input events (event-driven, no busy 16 ms poll)
                Some(crossterm_event) = event_stream.next() => {
                    let event = crossterm_event.into_diagnostic()?;
                    match event {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            let effect = state.handle_key(key);
                            needs_redraw = true;
                            match effect {
                                AppEffect::None => {}
                                AppEffect::Refresh => {
                                    trigger_refresh(&worker_tx, state, &mut fetch_in_flight, &mut refresh_queued);
                                }
                                AppEffect::FetchTask(id) => {
                                    let req = FetchRequest::Refresh {
                                        context: state.active_context.clone(),
                                        preview_id: Some(id.clone()),
                                        open_id: Some(id),
                                    };
                                    let _ = worker_tx.try_send(req);
                                }
                                AppEffect::Search(query) => {
                                    pending_search = Some(query);
                                    debounce_deadline = Some(tokio::time::Instant::now() + SEARCH_DEBOUNCE);
                                }
                            }
                        }
                        Event::Resize(_, _) => {
                            needs_redraw = true;
                        }
                        _ => {}
                    }
                }

                // Debounced live search typing
                () = async {
                    if let Some(deadline) = debounce_deadline {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if debounce_deadline.is_some() => {
                    debounce_deadline = None;
                    if let Some(query) = pending_search.take() {
                        let _ = worker_tx.try_send(FetchRequest::Search {
                            query,
                            context: state.active_context.clone(),
                        });
                    }
                }

                // Fetch responses from background worker
                Some(resp) = response_rx.recv() => {
                    match resp {
                        FetchResponse::Refreshed(data) => {
                            fetch_in_flight = false;
                            match data.today {
                                Ok(list) => {
                                    state.today = list.tasks;
                                    state.clear_error();
                                }
                                Err(err) => state.set_error(err),
                            }
                            if let Ok(list) = data.inbox {
                                state.inbox = list.tasks;
                            }
                            if let Ok(list) = data.waiting {
                                state.waiting = list.tasks;
                            }
                            if let Some(prev) = data.preview {
                                state.preview_detail = prev.ok();
                            }
                            if let Some((id, Ok(detail))) = data.open
                                && let Some(page) = state
                                    .page_stack
                                    .last_mut()
                                    .filter(|p| p.target == ObjectTarget::Task(id))
                            {
                                page.content = ObjectPageContent::Task(Box::new(detail));
                            }
                            if refresh_queued {
                                refresh_queued = false;
                                trigger_refresh(&worker_tx, state, &mut fetch_in_flight, &mut refresh_queued);
                            }
                            needs_redraw = true;
                        }
                        FetchResponse::SearchResults { query, results } => {
                            if query == state.search_query {
                                match results {
                                    Ok(res) => state.search_results = res.results,
                                    Err(err) => state.set_error(err),
                                }
                                needs_redraw = true;
                            }
                        }
                    }
                }

                // SSE change events from loomd
                frame_opt = next_sse_frame(&mut sse_stream), if sse_stream.is_some() => {
                    if let Some(Ok(Some(frame))) = frame_opt {
                        if let Some(id) = frame.id {
                            last_event_id = Some(id);
                        }
                        let effect = state.apply_change_event(&frame.event);
                        needs_redraw = true;
                        if effect == AppEffect::Refresh {
                            trigger_refresh(&worker_tx, state, &mut fetch_in_flight, &mut refresh_queued);
                        }
                    } else {
                        sse_stream = None;
                        state.sse_state = SseState::Reconnecting;
                        needs_redraw = true;
                    }
                }

                // SSE reconnection backoff
                _ = reconnect_tick.tick(), if sse_stream.is_none() => {
                    if let Ok(stream) = client.subscribe_events(None, last_event_id).await {
                        sse_stream = Some(stream);
                        state.sse_state = SseState::Connected;
                        trigger_refresh(&worker_tx, state, &mut fetch_in_flight, &mut refresh_queued);
                        needs_redraw = true;
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    drop(guard);
    loop_result
}

async fn run_fetch_worker(
    client: Client,
    mut requests: mpsc::Receiver<FetchRequest>,
    responses: mpsc::Sender<FetchResponse>,
) {
    while let Some(req) = requests.recv().await {
        match req {
            FetchRequest::Refresh {
                context,
                preview_id,
                open_id,
            } => {
                let today_path = list_api_path(context.as_ref(), None, false);
                let inbox_path = list_api_path(context.as_ref(), Some("inbox"), false);
                let waiting_path = list_api_path(context.as_ref(), Some("waiting"), false);

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

                let preview = if let Some(id) = preview_id {
                    let path = format!("/v1/tasks/{id}");
                    Some(
                        client
                            .get::<TaskDetail>(&path)
                            .await
                            .map_err(|e| e.to_string()),
                    )
                } else {
                    None
                };

                let open = if let Some(id) = open_id {
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
                    .send(FetchResponse::Refreshed(Box::new(RefreshedData {
                        today,
                        inbox,
                        waiting,
                        preview,
                        open,
                    })))
                    .await;
            }
            FetchRequest::Search { query, context } => {
                let path = search_api_path(
                    std::slice::from_ref(&query),
                    context.as_ref(),
                    None,
                    DEFAULT_SEARCH_LIMIT,
                );
                let results = client
                    .get::<SearchResults>(&path)
                    .await
                    .map_err(|e| e.to_string());
                let _ = responses
                    .send(FetchResponse::SearchResults { query, results })
                    .await;
            }
        }
    }
}

async fn next_sse_frame(
    stream: &mut Option<EventStream>,
) -> Option<Result<Option<loom::sse::SseFrame>, crate::client::ClientError>> {
    let s = stream.as_mut()?;
    Some(s.next().await)
}

#[cfg(unix)]
async fn wait_sigterm(signal: &mut tokio::signal::unix::Signal) {
    signal.recv().await;
}

#[cfg(not(unix))]
async fn wait_sigterm(_: &mut ()) {
    std::future::pending::<()>().await;
}
