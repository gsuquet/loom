//! Entrypoint and runtime loop of the interactive TUI (`loom` with no subcommand).

use std::env;
use std::io::{self, IsTerminal as _};
use std::time::Duration;

use clap::CommandFactory as _;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use loom::cli::{Cli, GlobalArgs};
use loom::conventions::use_color;
use loom::repo::repo_api_path;
use loom::search::search_api_path;
use loom::task::list_api_path as task_list_api_path;
use loom::tui::color::Theme;
use loom::tui::render::render;
use loom::tui::state::{AppEffect, AppState, ObjectTarget, SseState};
use loom::tui::terminal::TerminalGuard;
use loom_api::{
    ContextName, DEFAULT_SEARCH_LIMIT, EventCursor, RepoList, RepoSummary, SearchResults,
    TaskDetail, TaskId, TaskList,
};
use miette::{IntoDiagnostic as _, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::client::{Client, EventStream};
use crate::repo_cmd::{REPOS_PATH, connect};
use crate::{load_config, print};

/// Timeout for task/repo/search requests made by the TUI.
const TUI_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Poll interval for terminal input and SSE ticks.
const INPUT_TICK: Duration = Duration::from_millis(16);
/// Delay before reconnecting a dropped SSE stream.
const SSE_RECONNECT_DELAY: Duration = Duration::from_millis(500);

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
        let client = connect(&layout, true)
            .await?
            .with_timeout(TUI_REQUEST_TIMEOUT);
        refresh_all(&client, &mut state).await;
        run_tui_loop(&client, &mut state).await
    })
}

async fn run_tui_loop(client: &Client, state: &mut AppState) -> Result<()> {
    let guard = TerminalGuard::enter().into_diagnostic()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).into_diagnostic()?;

    let mut sse_stream: Option<EventStream> = client.subscribe_events(None, None).await.ok();
    state.sse_state = if sse_stream.is_some() {
        SseState::Connected
    } else {
        SseState::Reconnecting
    };
    let mut last_event_id: Option<EventCursor> = None;
    let mut tick = tokio::time::interval(INPUT_TICK);
    let mut reconnect_tick = tokio::time::interval(SSE_RECONNECT_DELAY);

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .into_diagnostic()?;

    let loop_result: Result<()> = async {
        loop {
            terminal
                .draw(|frame| render(frame, state))
                .into_diagnostic()?;

            if state.should_quit {
                break;
            }

            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    break;
                }
                () = wait_sigterm(&mut sigterm) => {
                    break;
                }
                frame_opt = next_sse_frame(&mut sse_stream), if sse_stream.is_some() => {
                    if let Some(Ok(Some(frame))) = frame_opt {
                        if let Some(id) = frame.id {
                            last_event_id = Some(id);
                        }
                        let effect = state.apply_change_event(&frame.event);
                        execute_effect(client, state, effect).await;
                    } else {
                        sse_stream = None;
                        state.sse_state = SseState::Reconnecting;
                    }
                }
                _ = reconnect_tick.tick(), if sse_stream.is_none() => {
                    if let Ok(stream) = client.subscribe_events(None, last_event_id).await {
                        sse_stream = Some(stream);
                        state.sse_state = SseState::Connected;
                        refresh_all(client, state).await;
                    }
                }
                _ = tick.tick() => {
                    while event::poll(Duration::ZERO).into_diagnostic()? {
                        if let Event::Key(key) = event::read().into_diagnostic()? {
                            if key.kind != KeyEventKind::Press {
                                continue;
                            }
                            if key.modifiers.contains(KeyModifiers::CONTROL)
                                && key.code == KeyCode::Char('c')
                            {
                                state.should_quit = true;
                                break;
                            }
                            let effect = state.handle_key(key);
                            execute_effect(client, state, effect).await;
                            if state.should_quit {
                                break;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    guard.restore();
    loop_result
}

#[cfg(unix)]
async fn wait_sigterm(sigterm: &mut tokio::signal::unix::Signal) {
    let _ = sigterm.recv().await;
}

#[cfg(not(unix))]
async fn wait_sigterm(_sigterm: &mut ()) {
    std::future::pending::<()>().await;
}

async fn next_sse_frame(
    stream: &mut Option<EventStream>,
) -> Option<std::result::Result<Option<loom::tui::sse::SseFrame>, crate::client::ClientError>> {
    let active = stream.as_mut()?;
    Some(active.next().await)
}

async fn execute_effect(client: &Client, state: &mut AppState, effect: AppEffect) {
    match effect {
        AppEffect::None | AppEffect::Quit => {}
        AppEffect::RefreshData => {
            refresh_all(client, state).await;
        }
        AppEffect::FetchPreviewTask(id) => {
            fetch_task_into_state(client, state, &id).await;
        }
        AppEffect::FetchObject(target) => match target {
            ObjectTarget::Task(id) => {
                fetch_task_into_state(client, state, &id).await;
            }
            ObjectTarget::Repo(slug) => {
                fetch_repo_into_state(client, state, &slug).await;
            }
        },
        AppEffect::Search(query) => {
            let words: Vec<String> = query.split_whitespace().map(str::to_owned).collect();
            let path = search_api_path(
                &words,
                state.context_filter.as_ref(),
                None,
                DEFAULT_SEARCH_LIMIT,
            );
            match client.get::<SearchResults>(&path).await {
                Ok(found) => state.set_search_results(found.results),
                Err(error) => {
                    state.status_message = Some(error.to_string());
                }
            }
        }
    }
}

async fn refresh_all(client: &Client, state: &mut AppState) {
    let ctx = state.context_filter.as_ref();
    let today_path = task_list_api_path(ctx, None, false);
    let inbox_path = task_list_api_path(ctx, Some("inbox"), false);
    let waiting_path = task_list_api_path(ctx, Some("waiting"), false);
    let repos_path = ctx.map_or_else(
        || REPOS_PATH.to_owned(),
        |context| format!("{REPOS_PATH}?context={context}"),
    );

    let today = client
        .get::<TaskList>(&today_path)
        .await
        .map_or_else(|_| Vec::new(), |list| list.tasks);
    let inbox = client
        .get::<TaskList>(&inbox_path)
        .await
        .map_or_else(|_| Vec::new(), |list| list.tasks);
    let waiting = client
        .get::<TaskList>(&waiting_path)
        .await
        .map_or_else(|_| Vec::new(), |list| list.tasks);
    let mut repos = client
        .get::<RepoList>(&repos_path)
        .await
        .map_or_else(|_| Vec::new(), |list| list.repos);
    loom::repo::sort(&mut repos);

    state.set_queues(today, inbox, waiting, repos);

    if let Some(selected_task) = state.selected_task().cloned() {
        fetch_task_into_state(client, state, &selected_task.id).await;
    } else {
        state.preview_task = None;
    }

    if let Some(top_page) = state.object_stack.last().cloned() {
        match top_page.target {
            ObjectTarget::Task(id) => fetch_task_into_state(client, state, &id).await,
            ObjectTarget::Repo(slug) => fetch_repo_into_state(client, state, &slug).await,
        }
    }
}

async fn fetch_task_into_state(client: &Client, state: &mut AppState, id: &TaskId) {
    let path = format!("/v1/tasks/{id}");
    if let Ok(detail) = client.get::<TaskDetail>(&path).await {
        state.set_task_detail(detail);
    }
}

async fn fetch_repo_into_state(client: &Client, state: &mut AppState, slug: &str) {
    let path = repo_api_path(slug);
    if let Ok(repo) = client.get::<RepoSummary>(&path).await {
        state.set_repo_detail(&repo);
    }
}
