//! I/O of the daemon: configuration file, `run/` directory, lock, socket, HTTP server, signals.

use std::env;
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions, TryLockError};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;
use std::process;
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use loom_api::{ApiError, ApiVersion, StatusResponse, VersionInfo};
use loom_config::config::{Config, parse_config};
use loom_config::home::{LOOM_HOME_ENV, resolve_home};
use loom_config::layout::{
    Layout, MAX_SOCKET_PATH_BYTES, RUN_DIR_MODE, RUN_FILE_MODE, check_socket_path,
    lock_taken_message, parse_lock_pid,
};
use loomd::api::{STATUS_PATH, clock_error, method_not_allowed, not_found, status_at, timestamp};
use loomd::cli::Cli;
use loomd::exit::Shutdown;
use miette::{Context as _, IntoDiagnostic as _, Result, miette};
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tracing::{debug, info, warn};

/// Starts the daemon and serves until `SIGTERM` or `SIGINT`.
pub fn run(cli: &Cli) -> Result<Shutdown> {
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

    // Checks without side effects first: a bad configuration leaves nothing behind.
    let _config = load_config(&layout.config)?;
    check_socket_path(&layout.socket, MAX_SOCKET_PATH_BYTES).into_diagnostic()?;

    create_run_dir(&layout)?;
    // Held, and the lock with it, until the process exits.
    let _lock = acquire_lock(&layout.lock)?;
    remove_stale_socket(&layout.socket)?;

    let base = StatusResponse {
        api_version: ApiVersion::CURRENT,
        daemon: VersionInfo::current(),
        pid: process::id(),
        started_at,
        uptime_secs: 0,
        loom_home: layout.home.clone(),
        socket_path: layout.socket.clone(),
        config_path: layout.config.clone(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()
        .wrap_err("cannot start the async runtime")?;
    runtime.block_on(serve(&layout, base))
}

/// Reads the configuration; a missing file gives the defaults.
fn load_config(path: &Path) -> Result<Config> {
    match fs::read_to_string(path) {
        Ok(text) => parse_config(&text, path).into_diagnostic(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            debug!(path = %path.display(), "no configuration file, using defaults");
            Ok(Config::default())
        }
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
async fn serve(layout: &Layout, base: StatusResponse) -> Result<Shutdown> {
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
            version = %base.daemon.version,
            home = %layout.home.display(),
            socket = %layout.socket.display(),
            "loomd started"
        );

        let received = Arc::new(OnceLock::new());
        let signal_received = Arc::clone(&received);
        let shutdown = async move {
            let shutdown = tokio::select! {
                _ = terminate.recv() => Shutdown::Terminated,
                _ = interrupt.recv() => Shutdown::Interrupted,
            };
            info!(signal = ?shutdown, "stopping: finishing in-flight requests");
            // Only this future sets it, once.
            let _ = signal_received.set(shutdown);
        };
        axum::serve(listener, router(base))
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
fn router(base: StatusResponse) -> Router {
    Router::new()
        .route(STATUS_PATH, get(status))
        .fallback(fallback_not_found)
        .method_not_allowed_fallback(fallback_method_not_allowed)
        .with_state(Arc::new(base))
}

async fn status(State(base): State<Arc<StatusResponse>>) -> Response {
    timestamp(SystemTime::now()).map_or_else(
        || error_response(StatusCode::INTERNAL_SERVER_ERROR, clock_error()),
        |now| Json(status_at(&base, now)).into_response(),
    )
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
