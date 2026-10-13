//! I/O of the daemon: configuration file, `run/` directory, lock, socket, HTTP server, signals.

use std::env;
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions, TryLockError};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use axum::Json;
use axum::Router;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{Method, StatusCode, Uri};
use axum::middleware;
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use loom_api::{
    ApiError, ApiVersion, RegisterRequest, RepoList, StatusResponse, Timestamp, UpdateState,
};
use loom_config::config::{Config, parse_config};
use loom_config::home::{LOOM_HOME_ENV, resolve_home};
use loom_config::layout::{
    Layout, MAX_SOCKET_PATH_BYTES, RUN_DIR_MODE, RUN_FILE_MODE, check_socket_path,
    lock_taken_message, parse_lock_pid,
};
use loom_store::RepoFilter;
use loom_update::auto;
use loomd::api::{
    REPO_PATH, REPOS_PATH, REPOS_REGISTER_PATH, REPOS_SCAN_PATH, RepoQuery, STATUS_PATH,
    clock_error, invalid_request, method_not_allowed, not_found, register_path, repo_filter, slug,
    status_at, timestamp,
};
use loomd::cli::{Cli, VERSION_INFO};
use loomd::exit::Shutdown;
use loomd::hooks;
use loomd::trace::request_span;
use miette::{Context as _, IntoDiagnostic as _, Result, miette};
use semver::Version;
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use crate::repos::{self, Failure, RepoHandle};
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
    // Opened under the lock: this daemon is the only writer of the index.
    let (index, repo_handle) = repos::start(&layout.index_db, &config, &user_home)
        .wrap_err("cannot start the repository index")?;

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
    };
    let served = runtime.block_on(serve(&layout, app, upgrade_requested));
    updater.stop();
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
        .route(REPO_PATH, get(get_repo).delete(forget_repo))
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

async fn get_repo(
    State(app): State<Arc<App>>,
    path: Result<UrlPath<(String, String)>, PathRejection>,
) -> Response {
    match path {
        Ok(UrlPath((context, name))) => respond(app.repos.get(slug(&context, &name)).await),
        Err(rejection) => error_response(
            StatusCode::BAD_REQUEST,
            invalid_request(rejection.body_text()),
        ),
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

async fn scan_repos(State(app): State<Arc<App>>) -> Response {
    respond(app.repos.scan().await)
}

async fn register_repo(
    State(app): State<Arc<App>>,
    body: Result<Json<RegisterRequest>, JsonRejection>,
) -> Response {
    let path = body
        .map_err(|rejection| invalid_request(rejection.body_text()))
        .and_then(|Json(request)| register_path(&request.path));
    match path {
        Ok(path) => respond(app.repos.register(path).await),
        Err(error) => error_response(StatusCode::BAD_REQUEST, error),
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
