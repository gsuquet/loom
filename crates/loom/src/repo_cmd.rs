//! `loom repo`: queries loomd and the filesystem. Decisions are in `loom::repo` and
//! `loom::doctor`.

use std::env;
use std::fs;
use std::io::{self, BufRead as _, IsTerminal as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use loom::backup::BACKUP_PATH;
use loom::cli::{
    GlobalArgs, RepoCommand, RepoDoctorArgs, RepoForgetArgs, RepoListFormat, RepoLsArgs,
    RepoRegisterArgs, RepoScanArgs, RepoShowArgs, RepoSyncArgs, VERSION_INFO,
};
use loom::conventions::{CONFIRMATION_REFUSED, Confirmation, confirmation};
use loom::doctor;
use loom::repo::{
    RepoArg, Selection, disambiguation_contexts, find_by_path, is_unassigned, lost_on_rebuild,
    normalize, parse_arg, render_card, render_paths, render_scan, render_sync, render_table,
    repo_api_path, select,
};
use loom::status::version_mismatch;
use loom_api::{
    BackupReport, CheckStatus, ContextAssignment, ContextName, DaemonDiagnostics, DoctorCheck,
    DoctorReport, RegisterRequest, RemoteCheckResult, RepoList, RepoSummary, ScanResult,
    SyncRequest, SyncResult,
};
use loom_config::config::Config;
use loom_config::layout::{Layout, MAX_SOCKET_PATH_BYTES, check_socket_path};
use loom_repo::{ContextRules, find_root};
use miette::{IntoDiagnostic as _, Result, miette};
use serde::Serialize;
use tracing::{debug, info, warn};

use crate::client::{Client, ClientError};
use crate::{current_platform, load_config, print, start};

/// Path of the repository list.
pub const REPOS_PATH: &str = "/v1/repos";
/// Path of the registration endpoint.
pub const REGISTER_PATH: &str = "/v1/repos/register";
/// Path of the scan endpoint.
const SCAN_PATH: &str = "/v1/repos/scan";
/// Path of the update endpoint.
const SYNC_PATH: &str = "/v1/repos/sync";
/// Longest wait for `loom repo sync`: every fetch of the index may take its timeout.
const SYNC_TIMEOUT: Duration = Duration::from_hours(1);
/// Path of the diagnostics endpoint.
const DIAGNOSTICS_PATH: &str = "/v1/diagnostics";
/// Path of the remote check endpoint.
const REMOTE_CHECK_PATH: &str = "/v1/diagnostics/remote-check";
/// Longest wait for the remote checks: they run at once in loomd, each stopped after 20 s
/// (then 5 s for its processes to exit).
const REMOTE_CHECK_TIMEOUT: Duration = Duration::from_secs(60);
/// Longest wait for a scan: it walks every root.
const SCAN_TIMEOUT: Duration = Duration::from_secs(300);
/// Longest wait for a state read on demand: its budget, plus a scan it may wait for.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(60);

/// What every `loom repo` command works from.
struct Env<'a> {
    layout: Layout,
    global: &'a GlobalArgs,
    /// The current directory, as the shell shows it.
    cwd: PathBuf,
    /// The user's home, `None` when `HOME` is unset or empty.
    home: Option<PathBuf>,
}

/// Runs a `loom repo` command.
pub fn run(command: &RepoCommand, global: &GlobalArgs, layout: Layout) -> Result<()> {
    let env = Env {
        layout,
        global,
        cwd: logical_cwd().into_diagnostic()?,
        home: env::home_dir().filter(|home| !home.as_os_str().is_empty()),
    };
    // The async runtime only drives the API client.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;
    runtime.block_on(async {
        match command {
            RepoCommand::Ls(args) => ls(&env, args).await,
            RepoCommand::Show(args) => show(&env, args).await,
            RepoCommand::Scan(args) => scan(&env, args).await,
            RepoCommand::Register(args) => register(&env, args).await,
            RepoCommand::Forget(args) => forget(&env, args).await,
            RepoCommand::Doctor(args) => doctor(&env, args).await,
            RepoCommand::Sync(args) => sync(&env, args).await,
        }
    })
}

/// The current directory as the shell shows it: `PWD` when it names the same directory as
/// the physical one (symbolic links kept, as the index keeps them), otherwise the physical
/// one.
///
/// # Errors
///
/// When the current directory cannot be read.
pub fn logical_cwd() -> io::Result<PathBuf> {
    let physical = env::current_dir()?;
    let logical = env::var_os("PWD").map(PathBuf::from).filter(|pwd| {
        pwd.is_absolute()
            && !pwd
                .components()
                .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
            && same_file(pwd, &physical)
    });
    Ok(logical.unwrap_or(physical))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

fn json(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string_pretty(value).into_diagnostic()? + "\n")
}

pub fn api_error(error: &ClientError) -> miette::Report {
    miette!("{error}")
}

/// A client of the running loomd; when none runs and `start`, loomd is started and its scan
/// on start awaited, so that the index is complete.
pub async fn connect(layout: &Layout, start: bool) -> Result<Client> {
    check_socket_path(&layout.socket, MAX_SOCKET_PATH_BYTES).into_diagnostic()?;
    let client = Client::new(layout.socket.clone());
    match client.status().await {
        Ok(status) => {
            if let Some(warning) = version_mismatch(&VERSION_INFO, &status.daemon, status.pid) {
                warn!("{warning}");
            }
            Ok(client)
        }
        Err(ClientError::Unreachable { .. }) if start => {
            start::start(layout, &client).await?;
            // Queued after the scan on start: answered with its result.
            let _: ScanResult = client
                .with_timeout(SCAN_TIMEOUT)
                .post(SCAN_PATH, None::<&()>)
                .await
                .map_err(|error| api_error(&error))?;
            Ok(client)
        }
        Err(ClientError::Unreachable { socket, source }) => Err(miette!(
            "loomd is not running: cannot connect to {}: {source}; run `loom status` to start it",
            socket.display()
        )),
        Err(error) => Err(api_error(&error)),
    }
}

async fn list(client: &Client, context: Option<&ContextName>) -> Result<Vec<RepoSummary>> {
    let path = context.map_or_else(
        || REPOS_PATH.to_owned(),
        |context| format!("{REPOS_PATH}?context={context}"),
    );
    let list: RepoList = client.get(&path).await.map_err(|error| api_error(&error))?;
    Ok(list.repos)
}

async fn ls(env: &Env<'_>, args: &RepoLsArgs) -> Result<()> {
    let client = connect(&env.layout, true).await?;
    let mut repos = list(&client, env.global.context.as_ref()).await?;
    loom::repo::sort(&mut repos);
    let text = if args.json {
        json(&RepoList { repos })?
    } else {
        match args.format {
            RepoListFormat::Paths => render_paths(&repos),
            RepoListFormat::Table => {
                let unassigned = repos.iter().filter(|repo| is_unassigned(repo)).count();
                if unassigned > 0 {
                    warn!(
                        "{unassigned} repositories (marked `!`) match no context: `loom repo doctor` shows how to assign them"
                    );
                }
                render_table(&repos)
            }
        }
    };
    print(&text)
}

/// What a `<repo>` argument resolved to.
enum Resolved {
    /// A repository of the index.
    Indexed(Box<RepoSummary>),
    /// The root of a repository the index does not have.
    Unindexed(PathBuf),
}

fn canonical(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path).ok()
}

/// The context `match.paths` gives the current directory, when the configuration can be
/// read.
fn cwd_rule(env: &Env<'_>) -> Option<ContextAssignment> {
    directory_rule(&env.layout, &env.cwd, env.home.as_deref())
}

/// What the context rules give the directory `dir` itself, when the configuration can be
/// read.
fn directory_rule(layout: &Layout, dir: &Path, home: Option<&Path>) -> Option<ContextAssignment> {
    let home = home?;
    let config = load_config(&layout.config)
        .map_err(|error| debug!(%error, "configuration ignored for disambiguation"))
        .ok()?;
    let rules = ContextRules::new(&config, home)
        .map_err(|error| debug!(%error, "context rules ignored for disambiguation"))
        .ok()?;
    Some(rules.assign(dir, None))
}

/// Registers the repository at `root` on use and returns it; `None`, logged, when loomd
/// refuses it.
async fn register_on_use(client: &Client, root: PathBuf) -> Option<RepoSummary> {
    let request = RegisterRequest {
        path: root,
        on_use: true,
    };
    client
        .post::<RepoSummary>(REGISTER_PATH, Some(&request))
        .await
        .map_err(|error| debug!(%error, "cannot register the repository of the current directory"))
        .ok()
}

/// The indexed repository holding the current directory, registered on use when it is not in
/// the index yet; `None` outside a git repository.
///
/// # Errors
///
/// When the current directory cannot be read or loomd cannot list the repositories.
pub async fn cwd_repo(client: &Client) -> Result<Option<RepoSummary>> {
    let cwd = logical_cwd().into_diagnostic()?;
    let Some(root) = find_root(&cwd).ok().flatten() else {
        return Ok(None);
    };
    let repos = list(client, None).await?;
    Ok(match find_by_path(&repos, &root, canonical) {
        Some(repo) => Some(repo.clone()),
        // Not indexed yet (a fresh clone, the registration on use still running):
        // registered now, so its remote decides as it will for the index.
        None => register_on_use(client, root).await,
    })
}

/// The context assignments of the current directory: the one of the indexed repository
/// holding it, and the one the rules give the directory itself (`match.paths`, then
/// `default_context`).
///
/// # Errors
///
/// When the current directory cannot be read or loomd cannot list the repositories.
pub async fn cwd_assignments(
    layout: &Layout,
    client: &Client,
) -> Result<(Option<ContextAssignment>, Option<ContextAssignment>)> {
    let cwd = logical_cwd().into_diagnostic()?;
    let home = env::home_dir().filter(|home| !home.as_os_str().is_empty());
    let repo = cwd_repo(client).await?.map(|repo| repo.identity.context);
    Ok((repo, directory_rule(layout, &cwd, home.as_deref())))
}

/// Resolves a `--repo` or `repo=` argument of a task in `task_context` to a repository slug.
///
/// # Errors
///
/// When the argument is ambiguous or names neither a repository nor a `<context>/<name>` slug.
pub async fn resolve_task_repo(
    client: &Client,
    task_context: &ContextName,
    arg: &str,
) -> Result<String> {
    let cwd = logical_cwd().into_diagnostic()?;
    let home = env::home_dir().filter(|home| !home.as_os_str().is_empty());
    let repos = list(client, None).await?;
    let parsed = parse_arg(Some(arg), &cwd, home.as_deref()).into_diagnostic()?;
    let contexts = vec![task_context.to_string()];
    match select(&parsed, &repos, &contexts).into_diagnostic()? {
        Selection::Repo(repo) => Ok(repo.identity.slug.clone()),
        Selection::Here => Err(miette!("expected a repository or none")),
        Selection::Path { path, unmatched } => {
            if fs::metadata(&path).is_ok() {
                let root = find_root(&path)
                    .into_diagnostic()?
                    .ok_or_else(|| miette!("{} is not inside a git repository", path.display()))?;
                if let Some(repo) = find_by_path(&repos, &root, canonical) {
                    return Ok(repo.identity.slug.clone());
                }
                let request = RegisterRequest {
                    path: root,
                    on_use: true,
                };
                let repo: RepoSummary = client
                    .post(REGISTER_PATH, Some(&request))
                    .await
                    .map_err(|error| api_error(&error))?;
                return Ok(repo.identity.slug);
            }
            if let RepoArg::SlugOrPath { slug, .. } = &parsed
                && is_slug(slug)
            {
                return Ok(slug.clone());
            }
            Err(unmatched.map_or_else(
                || miette!("{} does not exist", path.display()),
                |arg| {
                    miette!(
                        "no repository `{arg}` in the index, and no directory {}",
                        path.display()
                    )
                },
            ))
        }
    }
}

fn is_slug(slug: &str) -> bool {
    let Some((context, name)) = slug.split_once('/') else {
        return false;
    };
    context.parse::<ContextName>().is_ok()
        && !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.chars().any(char::is_whitespace)
}

async fn resolve(env: &Env<'_>, client: &Client, arg: Option<&str>) -> Result<Resolved> {
    let repos = list(client, None).await?;
    let parsed = parse_arg(arg, &env.cwd, env.home.as_deref()).into_diagnostic()?;
    let cwd_root = find_root(&env.cwd).ok().flatten();
    let contexts = if matches!(parsed, RepoArg::Name { .. }) {
        let cwd_repo = cwd_root
            .as_deref()
            .and_then(|root| find_by_path(&repos, root, canonical));
        disambiguation_contexts(
            env.global.context.as_ref().map(ContextName::as_str),
            cwd_repo,
            cwd_rule(env).as_ref(),
        )
    } else {
        Vec::new()
    };
    let by_root = |root: PathBuf| {
        find_by_path(&repos, &root, canonical).map_or(Resolved::Unindexed(root), |repo| {
            Resolved::Indexed(Box::new(repo.clone()))
        })
    };
    match select(&parsed, &repos, &contexts).into_diagnostic()? {
        Selection::Repo(repo) => Ok(Resolved::Indexed(Box::new(repo.clone()))),
        Selection::Here => {
            let root = cwd_root.ok_or_else(|| {
                miette!(
                    "{} is not inside a git repository: give a repository",
                    env.cwd.display()
                )
            })?;
            Ok(by_root(root))
        }
        Selection::Path { path, unmatched } => {
            if fs::metadata(&path).is_err() {
                return Err(unmatched.map_or_else(
                    || miette!("{} does not exist", path.display()),
                    |arg| {
                        miette!(
                            "no repository `{arg}` in the index, and no directory {}",
                            path.display()
                        )
                    },
                ));
            }
            let root = find_root(&path)
                .into_diagnostic()?
                .ok_or_else(|| miette!("{} is not inside a git repository", path.display()))?;
            Ok(by_root(root))
        }
    }
}

async fn show(env: &Env<'_>, args: &RepoShowArgs) -> Result<()> {
    let client = connect(&env.layout, true).await?;
    let path = match resolve(env, &client, args.repo.as_deref()).await? {
        Resolved::Indexed(repo) => repo.identity.path,
        Resolved::Unindexed(root) => {
            info!(path = %root.display(), "not in the index: registering it");
            root
        }
    };
    // Marks it used, and registers it when the index does not have it.
    let request = RegisterRequest { path, on_use: true };
    let mut repo: RepoSummary = client
        .post(REGISTER_PATH, Some(&request))
        .await
        .map_err(|error| api_error(&error))?;
    if args.refresh || repo.state.is_none() {
        let path = format!("{}?refresh=true", repo_api_path(&repo.identity.slug));
        repo = client
            .with_timeout(REFRESH_TIMEOUT)
            .get(&path)
            .await
            .map_err(|error| api_error(&error))?;
    }
    print(&if args.json {
        json(&repo)?
    } else {
        render_card(&repo)
    })
}

async fn scan(env: &Env<'_>, args: &RepoScanArgs) -> Result<()> {
    let client = connect(&env.layout, true).await?;
    if args.rebuild && !args.dry_run {
        confirm_rebuild(env, &client).await?;
    }
    let query = match (args.rebuild, args.dry_run) {
        (false, false) => "",
        (true, false) => "?rebuild=true",
        (false, true) => "?dry_run=true",
        (true, true) => "?rebuild=true&dry_run=true",
    };
    let result: ScanResult = client
        .with_timeout(SCAN_TIMEOUT)
        .post(&format!("{SCAN_PATH}{query}"), None::<&()>)
        .await
        .map_err(|error| api_error(&error))?;
    print(&if args.json {
        json(&result)?
    } else {
        render_scan(&result, args.dry_run)
    })
}

/// Warns what a rebuild loses, then asks (ADR-0010): `-y` proceeds, a terminal is asked,
/// anything else fails.
async fn confirm_rebuild(env: &Env<'_>, client: &Client) -> Result<()> {
    let repos = list(client, None).await?;
    let roots: Vec<PathBuf> = match (load_config(&env.layout.config), env.home.as_deref()) {
        (Ok(config), Some(home)) => doctor::roots(&config, home)
            .into_iter()
            .map(|root| root.path)
            .collect(),
        _ => Vec::new(),
    };
    let lost = lost_on_rebuild(&repos, &roots);
    warn!(
        "a rebuild drops the whole index (states, last_used, registrations), then scans the roots again"
    );
    if !lost.is_empty() {
        let paths: Vec<String> = lost
            .iter()
            .map(|repo| repo.identity.path.display().to_string())
            .collect();
        warn!(
            "{} registered repositories outside the roots will be lost (register them again with `loom repo register`): {}",
            lost.len(),
            paths.join(", ")
        );
    }
    match confirmation(env.global.yes, io::stdin().is_terminal()) {
        Confirmation::Proceed => Ok(()),
        Confirmation::Refuse => Err(miette!("{CONFIRMATION_REFUSED}")),
        Confirmation::Ask => {
            let mut stderr = io::stderr().lock();
            write!(
                stderr,
                "Rebuild the repository index ({} registered repositories outside the roots lost)? [y/N] ",
                lost.len()
            )
            .into_diagnostic()?;
            stderr.flush().into_diagnostic()?;
            let mut answer = String::new();
            io::stdin()
                .lock()
                .read_line(&mut answer)
                .into_diagnostic()?;
            if matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
                Ok(())
            } else {
                Err(miette!("rebuild cancelled: nothing changed"))
            }
        }
    }
}

async fn register(env: &Env<'_>, args: &RepoRegisterArgs) -> Result<()> {
    let path = args
        .path
        .as_ref()
        .map_or_else(|| env.cwd.clone(), |path| normalize(&env.cwd.join(path)));
    if fs::metadata(&path).is_err() {
        return Err(miette!("{} does not exist", path.display()));
    }
    let root = find_root(&path)
        .into_diagnostic()?
        .ok_or_else(|| miette!("{} is not inside a git repository", path.display()))?;
    if args.dry_run {
        return print(&format!("would register {}\n", root.display()));
    }
    // A use never starts loomd, as the registration on use of the other commands.
    let client = connect(&env.layout, !args.on_use).await?;
    let request = RegisterRequest {
        path: root,
        on_use: args.on_use,
    };
    let repo: RepoSummary = client
        .post(REGISTER_PATH, Some(&request))
        .await
        .map_err(|error| api_error(&error))?;
    print(&if args.json {
        json(&repo)?
    } else {
        format!(
            "{} {}: {}\n",
            if args.on_use { "used" } else { "registered" },
            repo.identity.slug,
            repo.identity.path.display()
        )
    })
}

async fn forget(env: &Env<'_>, args: &RepoForgetArgs) -> Result<()> {
    let client = connect(&env.layout, true).await?;
    let repo = match resolve(env, &client, Some(&args.repo)).await? {
        Resolved::Indexed(repo) => *repo,
        Resolved::Unindexed(root) => {
            return Err(miette!("{} is not in the index", root.display()));
        }
    };
    if args.dry_run {
        return print(&format!(
            "would forget {}: {}\n",
            repo.identity.slug,
            repo.identity.path.display()
        ));
    }
    let removed: RepoSummary = client
        .delete(&repo_api_path(&repo.identity.slug))
        .await
        .map_err(|error| api_error(&error))?;
    print(&if args.json {
        json(&removed)?
    } else {
        format!(
            "forgot {}: {}\n",
            removed.identity.slug,
            removed.identity.path.display()
        )
    })
}

/// `loom repo sync`: updates the repositories given (all of them when none) through loomd.
async fn sync(env: &Env<'_>, args: &RepoSyncArgs) -> Result<()> {
    let client = connect(&env.layout, true).await?;
    let mut slugs = Vec::with_capacity(args.repos.len());
    for arg in &args.repos {
        match resolve(env, &client, Some(arg)).await? {
            Resolved::Indexed(repo) => slugs.push(repo.identity.slug),
            Resolved::Unindexed(root) => {
                return Err(miette!("{} is not in the index", root.display()));
            }
        }
    }
    let request = SyncRequest {
        repos: slugs,
        dry_run: args.dry_run,
    };
    let result: SyncResult = client
        .with_timeout(SYNC_TIMEOUT)
        .post(SYNC_PATH, Some(&request))
        .await
        .map_err(|error| api_error(&error))?;
    print(&if args.json {
        json(&result)?
    } else {
        render_sync(&result)
    })
}

fn check(name: &str, status: CheckStatus, detail: String, fix: Option<&str>) -> DoctorCheck {
    DoctorCheck {
        check: name.to_owned(),
        status,
        detail,
        fix: fix.map(str::to_owned),
    }
}

async fn doctor(env: &Env<'_>, args: &RepoDoctorArgs) -> Result<()> {
    let config_path = &env.layout.config;
    let mut checks = Vec::new();
    let config: Option<Config> = match load_config(config_path) {
        Ok(config) => {
            let detail = if config_path.exists() {
                format!("{} is valid", config_path.display())
            } else {
                format!("no {}: defaults apply", config_path.display())
            };
            checks.push(check("config", CheckStatus::Ok, detail, None));
            Some(config)
        }
        Err(error) => {
            checks.push(check(
                "config",
                CheckStatus::Fail,
                error.to_string(),
                Some("fix the configuration: loomd refuses to start with an invalid one"),
            ));
            None
        }
    };
    match (&config, env.home.as_deref()) {
        (Some(config), Some(home)) => {
            let roots = doctor::roots(config, home);
            checks.extend(doctor::roots_checks(&roots, Path::is_dir, config_path));
            checks.push(doctor::contexts_check(config, home, config_path));
        }
        (Some(_), None) => checks.push(check(
            "roots",
            CheckStatus::Warn,
            "HOME is unset: `~` in the configuration cannot be expanded".to_owned(),
            None,
        )),
        (None, _) => {}
    }
    checks.extend(daemon_checks(env).await);

    let report = DoctorReport { checks };
    print(&if args.json {
        json(&report)?
    } else {
        doctor::render(&report)
    })?;
    let failed = report
        .checks
        .iter()
        .filter(|check| check.status == CheckStatus::Fail)
        .count();
    if failed > 0 {
        return Err(miette!(
            "{failed} check{} failed",
            if failed == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}

/// Checks that need loomd: it runs, its index, its environment, each remote endpoint reached
/// by loomd's own git, the last updates. loomd is not started: its environment is what is
/// checked.
async fn daemon_checks(env: &Env<'_>) -> Vec<DoctorCheck> {
    if let Err(error) = check_socket_path(&env.layout.socket, MAX_SOCKET_PATH_BYTES) {
        return vec![check("loomd", CheckStatus::Fail, error.to_string(), None)];
    }
    let client = Client::new(env.layout.socket.clone());
    let status = match client.status().await {
        Ok(status) => status,
        Err(ClientError::Unreachable { .. }) => {
            return vec![check(
                "loomd",
                CheckStatus::Warn,
                "not running: the disk usage, the index, loomd's environment, the remotes, the updates and the backups are not checked"
                    .to_owned(),
                Some("`loom status` starts it; `loom service install` keeps it running"),
            )];
        }
        Err(error) => return vec![check("loomd", CheckStatus::Fail, error.to_string(), None)],
    };
    let mut checks = vec![check(
        "loomd",
        CheckStatus::Ok,
        format!(
            "loomd {} running, pid {}",
            status.daemon.version, status.pid
        ),
        None,
    )];
    match &status.disk {
        Some(disk) => checks.extend(doctor::disk_checks(
            disk,
            &status.loom_home,
            &status.daemon.version,
        )),
        None => checks.push(check(
            "disk",
            CheckStatus::Warn,
            format!(
                "loomd {} cannot report the disk usage of the loom home",
                status.daemon.version
            ),
            Some("restart loomd with the version of this loom"),
        )),
    }
    let repos = match list(&client, None).await {
        Ok(repos) => repos,
        Err(error) => {
            checks.push(check("index", CheckStatus::Fail, error.to_string(), None));
            return checks;
        }
    };
    checks.push(doctor::unassigned_check(
        &repos,
        env.home.as_deref(),
        &env.layout.config,
    ));
    match client.get::<DaemonDiagnostics>(DIAGNOSTICS_PATH).await {
        Ok(diagnostics) => {
            checks.push(doctor::agent_check(
                &diagnostics,
                current_platform(),
                doctor::uses_ssh(&repos),
            ));
            checks.push(doctor::home_check(
                diagnostics.home.as_deref(),
                env.home.as_deref(),
            ));
        }
        Err(ClientError::Api { status: 404, .. }) => checks.push(check(
            "ssh-agent",
            CheckStatus::Warn,
            format!(
                "loomd {} cannot report its environment",
                status.daemon.version
            ),
            Some("restart loomd with the version of this loom"),
        )),
        Err(error) => checks.push(check(
            "ssh-agent",
            CheckStatus::Fail,
            error.to_string(),
            None,
        )),
    }
    checks.extend(remote_checks(&client, &status.daemon.version).await);
    checks.extend(doctor::updates_checks(&repos));
    checks.extend(backup_checks(env, &client, &status.daemon.version).await);
    checks
}

/// One check per remote endpoint, reached by loomd's own git.
async fn remote_checks(client: &Client, version: &str) -> Vec<DoctorCheck> {
    match client
        .with_timeout(REMOTE_CHECK_TIMEOUT)
        .post::<RemoteCheckResult>(REMOTE_CHECK_PATH, None::<&()>)
        .await
    {
        Ok(result) => doctor::remote_checks(&result),
        Err(ClientError::Api { status: 404, .. }) => vec![check(
            "remote",
            CheckStatus::Warn,
            format!("loomd {version} cannot check the remotes"),
            Some("restart loomd with the version of this loom"),
        )],
        Err(error) => vec![check("remote", CheckStatus::Fail, error.to_string(), None)],
    }
}

/// The `machine` and `backup <context>` checks, from loomd's backup state.
async fn backup_checks(env: &Env<'_>, client: &Client, version: &str) -> Vec<DoctorCheck> {
    match client.get::<BackupReport>(BACKUP_PATH).await {
        Ok(report) => doctor::backup_checks(&report, env.home.as_deref()),
        Err(ClientError::Api { status: 404, .. }) => vec![check(
            "backup",
            CheckStatus::Warn,
            format!("loomd {version} cannot report the backup of the context stores"),
            Some("restart loomd with the version of this loom"),
        )],
        Err(error) => vec![check("backup", CheckStatus::Fail, error.to_string(), None)],
    }
}
