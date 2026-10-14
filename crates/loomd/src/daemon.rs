//! I/O of the daemon: configuration file, `run/` directory, lock, socket, HTTP server, signals.

use std::env;
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions, TryLockError};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{
    DirBuilderExt as _, FileTypeExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{Method, StatusCode, Uri};
use axum::middleware;
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use loom_api::{
    ApiError, ApiVersion, ContextName, DaemonDiagnostics, RegisterRequest, RepoList, SearchKind,
    SearchResults, StatusResponse, SyncRequest, Timestamp, UpdateState,
};
use loom_config::config::{Config, parse_config};
use loom_config::home::{LOOM_HOME_ENV, resolve_home};
use loom_config::layout::{
    Layout, MAX_SOCKET_PATH_BYTES, RUN_DIR_MODE, RUN_FILE_MODE, check_socket_path,
    lock_taken_message, parse_lock_pid,
};
use loom_config::machine::{MachineName, parse_machine, render_machine};
use loom_core::MachineId;
use loom_store::RepoFilter;
use loom_update::auto;
use loomd::api::{
    DIAGNOSTICS_PATH, REMOTE_CHECK_PATH, REPO_PATH, REPOS_PATH, REPOS_REGISTER_PATH,
    REPOS_SCAN_PATH, REPOS_SYNC_PATH, RepoGetQuery, RepoQuery, SEARCH_PATH, STATUS_PATH, ScanQuery,
    SearchQuery, TASK_ACTION_PATH, TASK_EVENTS_PATH, TASK_PATH, TASK_UNDO_PATH, TASKS_PATH,
    TASKS_REINDEX_PATH, agent_socket, clock_error, home_value, invalid_request, method_not_allowed,
    not_found, register_path, repo_filter, search_request, slug, status_at, timestamp,
};
use loomd::cli::{Cli, VERSION_INFO};
use loomd::exit::Shutdown;
use loomd::hooks;
use loomd::task_api::{
    TEST_TODAY_ENV, TaskListQuery, action_command, action_name_of, action_request, new_task,
    task_edit, task_filter, today,
};
use loomd::trace::request_span;
use miette::{Context as _, IntoDiagnostic as _, Result, miette};
use semver::Version;
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use crate::disk_usage::DiskCache;
use crate::repos::{self, Failure, RepoHandle};
use crate::tasks::{self, TaskHandle};
use crate::updater::{self, Settings};

/// What the daemon reads before it changes anything.
pub struct Setup {
    started_at: Timestamp,
    layout: Layout,
    /// The configuration, defaults when the file is missing.
    pub config: Config,
    config_missing: bool,
}

/// Resolves the loom home and reads the configuration, without side effects: a bad
/// configuration leaves nothing behind.
pub fn load(cli: &Cli) -> Result<Setup> {
    let started_at = timestamp(SystemTime::now())
        .ok_or_else(|| miette!("the system clock is outside the range of timestamps"))?;
    let cwd = env::current_dir()
        .into_diagnostic()
        .wrap_err("cannot read the current directory")?;
    let home = resolve_home(
        cli.loom_home.as_deref(),
        env::var_os(LOOM_HOME_ENV).as_deref(),
        env::home_dir().as_deref(),
        &cwd,
    )
    .into_diagnostic()?;
    let layout = Layout::new(home);
    if let Some(message) = hooks::fail_unless_home_prefix(
        env::var(hooks::FAIL_UNLESS_HOME_PREFIX_ENV).ok().as_deref(),
        &layout.home,
    ) {
        return Err(miette!("{message}"));
    }
    let (config, config_missing) = load_config(&layout.config)?;
    check_socket_path(&layout.socket, MAX_SOCKET_PATH_BYTES).into_diagnostic()?;
    Ok(Setup {
        started_at,
        layout,
        config,
        config_missing,
    })
}

/// Starts the daemon and serves until `SIGTERM` or `SIGINT`.
pub fn run(setup: Setup) -> Result<Shutdown> {
    let Setup {
        started_at,
        layout,
        config,
        config_missing,
    } = setup;
    if config_missing {
        debug!(path = %layout.config.display(), "no configuration file, using defaults");
    }

    create_run_dir(&layout)?;
    // Held, and the lock with it, until the process exits.
    let _lock = acquire_lock(&layout.lock)?;
    remove_stale_socket(&layout.socket)?;
    // `~` of `[repos]` and `[contexts]` is the user's home, not the loom home.
    let user_home = env::home_dir().ok_or_else(|| {
        miette!("cannot find the user's home directory (HOME) to expand `~` in the configuration")
    })?;
    let machine = load_machine(&layout.machine)?;
    let stores = config.stores(&layout.home, &user_home, &machine);
    if !stores.not_held.is_empty() {
        info!(
            machine = %machine,
            contexts = ?stores.not_held.iter().map(ContextName::as_str).collect::<Vec<_>>(),
            "contexts not held on this machine are not loaded"
        );
    }
    // Opened under the lock: this daemon is the only writer of the index.
    let (index, repo_handle) = repos::start(&layout.index_db, &config, &user_home)
        .wrap_err("cannot start the repository index")?;
    let store_paths = stores
        .held
        .iter()
        .map(|(context, store)| (context.clone(), store.path.clone()))
        .collect();
    let (task_store, task_handle) = tasks::start(
        &layout.index_db,
        &store_paths,
        MachineId(machine.as_str().to_owned()),
    )
    .wrap_err("cannot start the task store")?;

    let base = StatusResponse {
        api_version: ApiVersion::CURRENT,
        daemon: VERSION_INFO.clone(),
        pid: process::id(),
        started_at,
        uptime_secs: 0,
        loom_home: layout.home.clone(),
        socket_path: layout.socket.clone(),
        config_path: layout.config.clone(),
        update: None,
        disk: None,
    };
    let settings = update_settings(&config);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()
        .wrap_err("cannot start the async runtime")?;
    let (upgraded, upgrade_requested) = oneshot::channel();
    let updater = updater::spawn(
        layout.clone(),
        settings.clone(),
        updater::Sources::from_env(),
        started_at,
        upgraded,
    )
    .into_diagnostic()
    .wrap_err("cannot start the updater thread")?;
    let app = App {
        base,
        settings,
        update_state: layout.update_state.clone(),
        running: Version::parse(&VERSION_INFO.version).ok(),
        repos: repo_handle,
        tasks: task_handle,
        layout: layout.clone(),
        disk: DiskCache::default(),
    };
    let served = runtime.block_on(serve(&layout, app, upgrade_requested));
    updater.stop();
    task_store.stop();
    index.stop();
    served
}

/// Effective `[update]` settings, with the debug-only test hooks applied.
fn update_settings(config: &Config) -> Settings {
    let hook = |name: &str| {
        hooks::duration_hook(name, env::var(name).ok().as_deref()).unwrap_or_else(|message| {
            warn!("{message}");
            None
        })
    };
    let build = VERSION_INFO.channel;
    Settings {
        policy: auto::effective_policy(config.update.policy, build),
        configured_channel: config.update.channel,
        build_channel: build,
        interval: hook(hooks::CHECK_INTERVAL_ENV)
            .unwrap_or_else(|| config.update.check_interval.get()),
        deadline: hook(hooks::HEALTH_DEADLINE_ENV).unwrap_or(auto::HEALTH_DEADLINE),
    }
}

/// Shared state of the API handlers.
struct App {
    /// Status without uptime and update state.
    base: StatusResponse,
    settings: Settings,
    /// `state/update.json`, read at each status request: the watchdog writes it too.
    update_state: PathBuf,
    running: Option<Version>,
    /// The repository index.
    repos: RepoHandle,
    /// The task store.
    tasks: TaskHandle,
    /// Paths under the loom home.
    layout: Layout,
    /// Last disk usage of the loom home, a new process starts without one.
    disk: DiskCache,
}

/// Reads the configuration, and whether the file is missing: a missing file gives the
/// defaults.
fn load_config(path: &Path) -> Result<(Config, bool)> {
    match fs::read_to_string(path) {
        Ok(text) => Ok((parse_config(&text, path).into_diagnostic()?, false)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok((Config::default(), true)),
        Err(error) => Err(error)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot read the configuration {}", path.display())),
    }
}

/// The name of this machine from `machine.toml`. A missing file is created (mode 0600) with
/// the host name; an invalid one stops loomd.
fn load_machine(path: &Path) -> Result<MachineName> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(parse_machine(&text, path).into_diagnostic()?.id),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let host = rustix::system::uname()
                .nodename()
                .to_string_lossy()
                .into_owned();
            let id = MachineName::from_host_name(&host);
            let created = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(RUN_FILE_MODE)
                .open(path)
                .and_then(|mut file| {
                    file.write_all(render_machine(&id).as_bytes())?;
                    file.sync_all()
                });
            created
                .into_diagnostic()
                .wrap_err_with(|| format!("cannot create {}", path.display()))?;
            info!(path = %path.display(), machine = %id, "machine named after the host");
            Ok(id)
        }
        Err(error) => Err(error)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot read {}", path.display())),
    }
}

/// Creates the loom home and `run/` (mode 0700), and restricts an existing `run/` to 0700.
fn create_run_dir(layout: &Layout) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(RUN_DIR_MODE)
        .create(&layout.run_dir)
        .and_then(|()| fs::set_permissions(&layout.run_dir, Permissions::from_mode(RUN_DIR_MODE)))
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot create {}", layout.run_dir.display()))
}

/// Takes the single-instance lock and writes this process's PID in the lock file.
fn acquire_lock(path: &Path) -> Result<File> {
    // No truncation before the lock is held: the file holds the PID of the running daemon.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(RUN_FILE_MODE)
        .open(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot open the lock file {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let mut text = String::new();
            let holder = file
                .read_to_string(&mut text)
                .ok()
                .and_then(|_| parse_lock_pid(&text));
            return Err(miette!("{}", lock_taken_message(path, holder)));
        }
        Err(TryLockError::Error(error)) => {
            return Err(error)
                .into_diagnostic()
                .wrap_err_with(|| format!("cannot lock {}", path.display()));
        }
    }
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| writeln!(file, "{}", process::id()))
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot write the PID in {}", path.display()))?;
    Ok(file)
}

/// Removes a socket left by a daemon that did not shut down. Only called with the lock held,
/// so no running daemon owns it.
fn remove_stale_socket(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            info!(socket = %path.display(), "removed stale socket");
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot remove the stale socket {}", path.display())),
    }
}

/// Binds the socket, serves the API until a signal, then removes the socket.
async fn serve(
    layout: &Layout,
    app: App,
    upgrade_requested: oneshot::Receiver<()>,
) -> Result<Shutdown> {
    // Handlers are installed before the socket exists, so that a client that sees the socket
    // can always stop the daemon gracefully.
    let mut terminate = signal(SignalKind::terminate())
        .into_diagnostic()
        .wrap_err("cannot handle SIGTERM")?;
    let mut interrupt = signal(SignalKind::interrupt())
        .into_diagnostic()
        .wrap_err("cannot handle SIGINT")?;

    let listener = UnixListener::bind(&layout.socket)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot listen on {}", layout.socket.display()))?;
    let served = async {
        fs::set_permissions(&layout.socket, Permissions::from_mode(RUN_FILE_MODE))
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot set the mode of {}", layout.socket.display()))?;
        info!(
            version = %app.base.daemon.version,
            home = %layout.home.display(),
            socket = %layout.socket.display(),
            "loomd started"
        );

        let received = Arc::new(OnceLock::new());
        let signal_received = Arc::clone(&received);
        let upgrade = async move {
            // A dropped sender (updater stopped without an upgrade) is not a request.
            if upgrade_requested.await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        let shutdown = async move {
            let shutdown = tokio::select! {
                _ = terminate.recv() => Shutdown::Terminated,
                _ = interrupt.recv() => Shutdown::Interrupted,
                () = upgrade => Shutdown::Upgraded,
            };
            info!(signal = ?shutdown, "stopping: finishing in-flight requests");
            // Only this future sets it, once.
            let _ = signal_received.set(shutdown);
        };
        axum::serve(listener, router(app))
            .with_graceful_shutdown(shutdown)
            .await
            .into_diagnostic()
            .wrap_err("the API server failed")?;
        received
            .get()
            .copied()
            .ok_or_else(|| miette!("the API server stopped without a signal"))
    }
    .await;

    if let Err(error) = fs::remove_file(&layout.socket) {
        warn!(socket = %layout.socket.display(), %error, "cannot remove the socket");
    }
    if served.is_ok() {
        info!("loomd stopped");
    }
    served
}

/// Routes of the API. Every error body is an [`ApiError`].
fn router(app: App) -> Router {
    Router::new()
        .route(STATUS_PATH, get(status))
        .route(REPOS_PATH, get(list_repos))
        .route(REPOS_SCAN_PATH, post(scan_repos))
        .route(REPOS_REGISTER_PATH, post(register_repo))
        .route(REPOS_SYNC_PATH, post(sync_repos))
        .route(REPO_PATH, get(get_repo).delete(forget_repo))
        .route(SEARCH_PATH, get(search))
        .route(DIAGNOSTICS_PATH, get(diagnostics))
        .route(REMOTE_CHECK_PATH, post(remote_check))
        .route(TASKS_REINDEX_PATH, post(reindex_tasks))
        .route(TASKS_PATH, get(list_tasks).post(create_task))
        .route(TASK_PATH, get(get_task).patch(edit_task))
        .route(TASK_ACTION_PATH, post(task_action))
        .route(TASK_EVENTS_PATH, get(task_events))
        .route(TASK_UNDO_PATH, post(undo_task))
        .fallback(fallback_not_found)
        .method_not_allowed_fallback(fallback_method_not_allowed)
        .layer(middleware::from_fn(request_span))
        .with_state(Arc::new(app))
}

async fn status(State(app): State<Arc<App>>) -> Response {
    let Some(now) = timestamp(SystemTime::now()) else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, clock_error());
    };
    let mut status = status_at(&app.base, now);
    // A small file, replaced atomically: read without the state lock.
    let state = loom_update::state::read(&app.update_state).unwrap_or_else(|error| {
        warn!(%error, "cannot read the update state");
        UpdateState::default()
    });
    let channel = app.settings.channel(&state);
    status.update = Some(auto::status(
        state,
        app.settings.policy,
        channel,
        app.settings.interval,
        app.running.as_ref(),
    ));
    status.disk = app.disk.get(&app.layout, now).await;
    Json(status).into_response()
}

async fn list_repos(
    State(app): State<Arc<App>>,
    query: Result<Query<RepoQuery>, QueryRejection>,
) -> Response {
    let filter = query
        .map_err(|rejection| invalid_request(rejection.body_text()))
        .and_then(|Query(query)| repo_filter(query));
    let (context, query) = match filter {
        Ok(filter) => filter,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error),
    };
    respond(
        app.repos
            .list(RepoFilter { context, query })
            .await
            .map(|repos| RepoList { repos }),
    )
}

async fn search(
    State(app): State<Arc<App>>,
    query: Result<Query<SearchQuery>, QueryRejection>,
) -> Response {
    let request = query
        .map_err(|rejection| invalid_request(rejection.body_text()))
        .and_then(|Query(query)| search_request(query));
    match request {
        Ok(request) => {
            if request.kind != Some(SearchKind::Repo)
                && !app.tasks.wait_ready(tasks::READY_TIMEOUT).await
            {
                warn!("tasks still being indexed: the search may miss some");
            }
            respond(
                app.repos
                    .search(request)
                    .await
                    .map(|results| SearchResults { results }),
            )
        }
        Err(error) => error_response(StatusCode::BAD_REQUEST, error),
    }
}

async fn get_repo(
    State(app): State<Arc<App>>,
    path: Result<UrlPath<(String, String)>, PathRejection>,
    query: Result<Query<RepoGetQuery>, QueryRejection>,
) -> Response {
    let query = query.map_err(|rejection| invalid_request(rejection.body_text()));
    match (path, query) {
        (Ok(UrlPath((context, name))), Ok(Query(query))) => {
            let slug = slug(&context, &name);
            if query.refresh {
                respond(app.repos.refresh(slug).await)
            } else {
                respond(app.repos.get(slug).await)
            }
        }
        (Err(rejection), _) => error_response(
            StatusCode::BAD_REQUEST,
            invalid_request(rejection.body_text()),
        ),
        (_, Err(error)) => error_response(StatusCode::BAD_REQUEST, error),
    }
}

async fn forget_repo(
    State(app): State<Arc<App>>,
    path: Result<UrlPath<(String, String)>, PathRejection>,
) -> Response {
    match path {
        Ok(UrlPath((context, name))) => respond(app.repos.forget(slug(&context, &name)).await),
        Err(rejection) => error_response(
            StatusCode::BAD_REQUEST,
            invalid_request(rejection.body_text()),
        ),
    }
}

async fn scan_repos(
    State(app): State<Arc<App>>,
    query: Result<Query<ScanQuery>, QueryRejection>,
) -> Response {
    match query {
        Ok(Query(mode)) => respond(app.repos.scan(mode).await),
        Err(rejection) => error_response(
            StatusCode::BAD_REQUEST,
            invalid_request(rejection.body_text()),
        ),
    }
}

/// The environment of the daemon its git operations depend on, read at each request.
async fn diagnostics() -> Response {
    let ssh_auth_sock = agent_socket(env::var_os("SSH_AUTH_SOCK").as_deref(), |path| {
        fs::metadata(path)
            .ok()
            .map(|metadata| metadata.file_type().is_socket())
    });
    Json(DaemonDiagnostics {
        ssh_auth_sock,
        home: home_value(env::var_os("HOME").as_deref()),
    })
    .into_response()
}

/// The tasks index rebuilt from the files; the repositories are left as they are.
async fn reindex_tasks(State(app): State<Arc<App>>) -> Response {
    respond(app.tasks.reindex().await)
}

async fn list_tasks(
    State(app): State<Arc<App>>,
    query: Result<Query<TaskListQuery>, QueryRejection>,
) -> Response {
    let filter = query
        .map_err(|rejection| invalid_request(rejection.body_text()))
        .and_then(|Query(query)| task_filter(query));
    match filter {
        Ok(filter) => respond(app.tasks.list(filter, local_today()).await),
        Err(error) => error_response(StatusCode::BAD_REQUEST, error),
    }
}

async fn create_task(
    State(app): State<Arc<App>>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Response {
    let new = body
        .map_err(|rejection| invalid_request(rejection.body_text()))
        .and_then(|Json(body)| new_task(body));
    match new {
        Ok(new) => respond(app.tasks.create(new).await),
        Err(error) => error_response(StatusCode::BAD_REQUEST, error),
    }
}

async fn get_task(
    State(app): State<Arc<App>>,
    path: Result<UrlPath<String>, PathRejection>,
) -> Response {
    match path {
        Ok(UrlPath(prefix)) => respond(app.tasks.get(prefix).await),
        Err(rejection) => error_response(
            StatusCode::BAD_REQUEST,
            invalid_request(rejection.body_text()),
        ),
    }
}

async fn edit_task(
    State(app): State<Arc<App>>,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Response {
    let request = path
        .map_err(|rejection| invalid_request(rejection.body_text()))
        .and_then(|UrlPath(prefix)| {
            let body = body.map_err(|rejection| invalid_request(rejection.body_text()))?;
            Ok((prefix, task_edit(body.0)?))
        });
    let (prefix, edit) = match request {
        Ok(request) => request,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error),
    };
    respond(
        resolve_then(&app, prefix, |id| {
            app.tasks.apply(id, loom_core::Command::Edit(edit))
        })
        .await,
    )
}

async fn task_action(
    State(app): State<Arc<App>>,
    path: Result<UrlPath<(String, String)>, PathRejection>,
    uri: Uri,
    body: Bytes,
) -> Response {
    let (prefix, action) = match path {
        Ok(UrlPath(path)) => path,
        Err(rejection) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                invalid_request(rejection.body_text()),
            );
        }
    };
    // An action that does not exist is a path no endpoint serves.
    let Some(action) = action_name_of(&action) else {
        return error_response(StatusCode::NOT_FOUND, not_found(uri.path()));
    };
    let command = match action_request(&body).and_then(|request| action_command(action, request)) {
        Ok(command) => command,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error),
    };
    respond(resolve_then(&app, prefix, |id| app.tasks.apply(id, command)).await)
}

async fn task_events(
    State(app): State<Arc<App>>,
    path: Result<UrlPath<String>, PathRejection>,
) -> Response {
    match path {
        Ok(UrlPath(prefix)) => respond(resolve_then(&app, prefix, |id| app.tasks.events(id)).await),
        Err(rejection) => error_response(
            StatusCode::BAD_REQUEST,
            invalid_request(rejection.body_text()),
        ),
    }
}

/// Reverts the last change of the task: loomd computes the compensating change from its log;
/// the request has no body.
async fn undo_task(
    State(app): State<Arc<App>>,
    path: Result<UrlPath<String>, PathRejection>,
) -> Response {
    match path {
        Ok(UrlPath(prefix)) => respond(resolve_then(&app, prefix, |id| app.tasks.undo(id)).await),
        Err(rejection) => error_response(
            StatusCode::BAD_REQUEST,
            invalid_request(rejection.body_text()),
        ),
    }
}

/// Resolves the task ID `prefix`, then runs `then` on the ID.
async fn resolve_then<T, F>(
    app: &App,
    prefix: String,
    then: impl FnOnce(loom_api::TaskId) -> F,
) -> Result<T, Failure>
where
    F: Future<Output = Result<T, Failure>>,
{
    let id = app.tasks.resolve(prefix).await?;
    then(id).await
}

/// The date of the priority list: the local date, or `LOOM_TEST_TODAY` in a debug build.
fn local_today() -> loom_api::Date {
    today(
        env::var(TEST_TODAY_ENV).ok().as_deref(),
        cfg!(debug_assertions),
        || jiff::Zoned::now().date(),
    )
}

/// `git ls-remote` of each remote endpoint of the index, in loomd's environment.
async fn remote_check(State(app): State<Arc<App>>) -> Response {
    respond(app.repos.remote_check().await)
}

async fn register_repo(
    State(app): State<Arc<App>>,
    body: Result<Json<RegisterRequest>, JsonRejection>,
) -> Response {
    let path = body
        .map_err(|rejection| invalid_request(rejection.body_text()))
        .and_then(|Json(request)| Ok((register_path(&request.path)?, request.on_use)));
    match path {
        Ok((path, on_use)) => respond(app.repos.register(path, on_use).await),
        Err(error) => error_response(StatusCode::BAD_REQUEST, error),
    }
}

async fn sync_repos(
    State(app): State<Arc<App>>,
    body: Result<Json<SyncRequest>, JsonRejection>,
) -> Response {
    match body {
        Ok(Json(request)) => respond(app.repos.sync(request).await),
        Err(rejection) => error_response(
            StatusCode::BAD_REQUEST,
            invalid_request(rejection.body_text()),
        ),
    }
}

/// `200` with the JSON of `result`, or its error.
fn respond<T: serde::Serialize>(result: Result<T, Failure>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err((status, error)) => error_response(status, error),
    }
}

async fn fallback_not_found(uri: Uri) -> Response {
    error_response(StatusCode::NOT_FOUND, not_found(uri.path()))
}

async fn fallback_method_not_allowed(method: Method, uri: Uri) -> Response {
    error_response(
        StatusCode::METHOD_NOT_ALLOWED,
        method_not_allowed(method.as_str(), uri.path()),
    )
}

/// `Json` sets `content-type: application/json`.
fn error_response(status: StatusCode, error: ApiError) -> Response {
    (status, Json(error)).into_response()
}
