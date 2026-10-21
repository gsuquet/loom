//! The task commands: resolve the context and the task, call loomd. Decisions and output are
//! in `loom::task`.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use loom::cli::{
    AddArgs, EditArgs, FocusArgs, GlobalArgs, GoArgs, LogArgs, LsArgs, SetArgs, ShowArgs,
    SnoozeArgs, TaskArgs,
};
use loom::task::{
    AddRequest, CONTEXT_ENV, ContextError, KindArg, ResolvedContext, TEST_TODAY_ENV,
    changed_after_save, changed_meanwhile, context_with_parent, default_repo, editor,
    focus_api_path, holds_saved, kind_of, list_api_path, new_task, parse_date, render_change,
    render_detail, render_focus, render_go, render_list, resolve_context, set_plan, skipped_notice,
    today, with_parent, with_repo,
};
use loom_api::{
    Actor, ContextName, Date, EventRecord, FocusList, GoResult, TaskActionRequest, TaskDetail,
    TaskEdit, TaskEvents, TaskList, TaskReadOnly,
};
use loom_config::layout::Layout;
use miette::{IntoDiagnostic as _, Result, miette};
use serde::Serialize;
use tracing::{debug, warn};

use crate::client::Client;
use crate::repo_cmd::{api_error, connect, cwd_assignments, cwd_repo, resolve_task_repo};
use crate::{load_config, print};

/// Path of the task list and of the creation endpoint.
const TASKS_PATH: &str = "/v1/tasks";
/// File where `loom go` writes the repository path for the shell wrapper (`loom shell-init`).
const CD_FILE_ENV: &str = "LOOM_CD_FILE";
/// Longest wait for loomd to record a `loom edit`: its watcher reads the file at once.
const EDIT_RECORD_TIMEOUT: Duration = Duration::from_secs(5);
/// Interval between two reads of the edited task.
const EDIT_POLL: Duration = Duration::from_millis(50);
/// Longest wait for a task request: right after it starts, loomd waits up to 10 s for the tasks
/// it indexes before it answers a read or resolves an ID, and the default timeout is 5 s.
const TASK_TIMEOUT: Duration = Duration::from_secs(15);

/// `loom add`.
pub fn add(args: &AddArgs, global: &GlobalArgs, layout: &Layout) -> Result<()> {
    if args.format.is_some() && args.kind != KindArg::Content {
        return Err(miette!("--format applies to --kind content only"));
    }
    if let Some(repo) = &args.repo
        && (repo.trim().is_empty() || repo.chars().any(char::is_control))
    {
        return Err(miette!("invalid --repo: expected a repository or none"));
    }
    let due = args
        .due
        .as_deref()
        .map(|text| parse_date(text, local_today()).into_diagnostic())
        .transpose()?;
    block_on(async {
        let client = client(layout).await?;
        let resolved = context(global, layout, &client).await?;
        // A task and its parent share one context.
        let (context, parent) = match &args.parent {
            Some(prefix) => {
                let parent = get(&client, prefix).await?.summary;
                let context =
                    context_with_parent(resolved, &parent.id, &parent.context).into_diagnostic()?;
                (context, Some(parent.id))
            }
            None => (resolved.into_diagnostic()?.context, None),
        };
        let repo = match args.repo.as_deref() {
            Some("none") => None,
            Some(arg) => Some(resolve_task_repo(&client, &context, arg).await?),
            None => default_repo(cwd_repo(&client).await?.as_ref(), &context),
        };
        let request = new_task(AddRequest {
            title: args.title.join(" "),
            context,
            kind: kind_of(args.kind, args.format),
            importance: args.importance.map(Into::into),
            due,
            parent,
            repo,
            ready: args.ready,
        });
        let created: TaskDetail = client
            .post(TASKS_PATH, Some(&request))
            .await
            .map_err(|error| api_error(&error))?;
        output(&created, args.json, || {
            let summary = &created.summary;
            let title = summary.task.as_ref().map_or("", |task| task.title.as_str());
            format!("{} {} {}\n", summary.prefix, summary.context, title)
        })
    })
}

/// `loom ls`.
pub fn ls(args: &LsArgs, global: &GlobalArgs, layout: &Layout) -> Result<()> {
    if args.all_contexts && global.context.is_some() {
        return Err(miette!("--all-contexts and -c cannot be used together"));
    }
    block_on(async {
        let client = client(layout).await?;
        let context: Option<ContextName> = if args.all_contexts {
            None
        } else {
            Some(
                context(global, layout, &client)
                    .await?
                    .into_diagnostic()?
                    .context,
            )
        };
        let path = list_api_path(
            context.as_ref(),
            args.status.map(loom::cli::StatusArg::as_str),
            args.archived,
        );
        let list: TaskList = client.get(&path).await.map_err(|error| api_error(&error))?;
        output(&list, args.json, || render_list(&list.tasks, args.why))
    })
}

/// `loom show`.
pub fn show(args: &ShowArgs, layout: &Layout) -> Result<()> {
    block_on(async {
        let client = client(layout).await?;
        let detail = get(&client, &args.id).await?;
        // On standard error, so that `--json` stays one document. A log that cannot be read does
        // not stop the display of the task.
        match event_log(&client, &detail.summary.id.to_string()).await {
            Ok(log) => {
                if let Some(notice) = skipped_notice(&log.skipped) {
                    warn!("{}", notice.trim_end());
                }
            }
            Err(error) => debug!(%error, "cannot read the event log of the task"),
        }
        output(&detail, args.json, || render_detail(&detail))
    })
}

/// `loom set`.
pub fn set(args: &SetArgs, layout: &Layout) -> Result<()> {
    let plan = set_plan(&args.assignments, local_today()).into_diagnostic()?;
    block_on(async {
        let client = client(layout).await?;
        let mut detail = get(&client, &args.id).await?;
        let id = detail.summary.id.to_string();
        let mut edit = plan.edit;
        if let Some(parent) = plan.parent {
            let parent = match parent {
                Some(prefix) => Some(get(&client, &prefix).await?.summary.id),
                None => None,
            };
            let relations = detail
                .summary
                .task
                .as_ref()
                .map(|task| task.relations.clone())
                .unwrap_or_default();
            edit.relations = Some(with_parent(&relations, parent));
        }
        if let Some(repo) = plan.repo {
            let repo = match repo {
                Some(arg) => Some(resolve_task_repo(&client, &detail.summary.context, &arg).await?),
                None => None,
            };
            let links = detail
                .summary
                .task
                .as_ref()
                .map(|task| task.links.clone())
                .unwrap_or_default();
            edit.links = Some(with_repo(&links, repo));
        }
        if edit != TaskEdit::default() {
            detail = client
                .patch(&format!("{TASKS_PATH}/{id}"), &edit)
                .await
                .map_err(|error| api_error(&error))?;
        }
        if let Some(pinned) = plan.pinned {
            let name = if pinned { "pin" } else { "unpin" };
            detail = action(&client, &id, name, None).await?;
        }
        output(&detail, args.json, || render_change(&detail))
    })
}

/// `loom go`.
pub fn go(args: &GoArgs, layout: &Layout) -> Result<()> {
    block_on(async {
        let client = client(layout).await?;
        let result: GoResult = client
            .post(&format!("{TASKS_PATH}/{}/go", args.id), None::<&()>)
            .await
            .map_err(|error| api_error(&error))?;
        if let Some(path) = &result.repo_path
            && let Some(cd_file) = env::var_os(CD_FILE_ENV).filter(|value| !value.is_empty())
            && let Err(error) = fs::write(&cd_file, format!("{}\n", path.display()))
        {
            debug!(%error, "cannot write the repository path to {CD_FILE_ENV}");
        }
        output(&result, args.json, || render_go(&result))
    })
}

/// `loom focus`.
pub fn focus(args: &FocusArgs, global: &GlobalArgs, layout: &Layout) -> Result<()> {
    block_on(async {
        let client = client(layout).await?;
        let path = focus_api_path(global.context.as_ref());
        let list: FocusList = client.get(&path).await.map_err(|error| api_error(&error))?;
        output(&list, args.json, || render_focus(&list))
    })
}

/// `loom log`.
pub fn log(args: &LogArgs, global: &GlobalArgs, layout: &Layout) -> Result<()> {
    let request = TaskActionRequest {
        on: None,
        until: None,
        text: Some(args.text.join(" ")),
    };
    block_on(async {
        let client = client(layout).await?;
        let id = if let Some(prefix) = &args.task {
            prefix.clone()
        } else {
            let context = context(global, layout, &client)
                .await?
                .into_diagnostic()?
                .context;
            let list: FocusList = client
                .get(&focus_api_path(Some(&context)))
                .await
                .map_err(|error| api_error(&error))?;
            let Some(entry) = list.entries.first() else {
                return Err(miette!(
                    "no focus task in context {context}: run `loom go <id>` to focus a task, or pass `--task <id>`"
                ));
            };
            entry.task.id.to_string()
        };
        let detail = action(&client, &id, "log_note", Some(&request)).await?;
        output(&detail, args.json, || render_change(&detail))
    })
}

/// `loom done` (`done`) and `loom cancel` (`cancelled`).
pub fn status(args: &TaskArgs, name: &str, layout: &Layout) -> Result<()> {
    block_on(async {
        let client = client(layout).await?;
        let detail = action(&client, &args.id, name, None).await?;
        output(&detail, args.json, || render_change(&detail))
    })
}

/// `loom snooze`.
pub fn snooze(args: &SnoozeArgs, layout: &Layout) -> Result<()> {
    let until = parse_date(&args.until, local_today()).into_diagnostic()?;
    let request = TaskActionRequest {
        on: args.on.clone(),
        until: Some(until),
        text: None,
    };
    block_on(async {
        let client = client(layout).await?;
        let detail = action(&client, &args.id, "waiting", Some(&request)).await?;
        output(&detail, args.json, || render_change(&detail))
    })
}

/// `loom undo`.
pub fn undo(args: &TaskArgs, layout: &Layout) -> Result<()> {
    block_on(async {
        let client = client(layout).await?;
        let detail: TaskDetail = client
            .post(&format!("{TASKS_PATH}/{}/undo", args.id), None::<&()>)
            .await
            .map_err(|error| api_error(&error))?;
        output(&detail, args.json, || render_change(&detail))
    })
}

/// `loom edit`: opens the task file in the editor, checks it, then waits for loomd to record
/// it. An invalid file is reported and kept as saved: loomd shows the task read-only until it
/// is fixed.
pub fn edit(args: &EditArgs, layout: &Layout) -> Result<()> {
    let config = load_config(&layout.config)?;
    let user_home = env::home_dir()
        .filter(|home| !home.as_os_str().is_empty())
        .ok_or_else(|| miette!("cannot find the user's home directory: HOME is unset or empty"))?;
    let runtime = runtime()?;
    let client = runtime.block_on(client(layout))?;
    let before = runtime.block_on(get(&client, &args.id))?;
    let summary = &before.summary;
    // A hand edit of an archived file would be recorded by loomd: the command keeps the archive
    // read-only, as the API does.
    if let Some(TaskReadOnly::Archived(year)) = summary.read_only {
        return Err(miette!(
            "task {} is archived (archive/{year}): an archived task is read-only. To reopen it, set its status in archive/{year}/{}/task.md, then move its directory back to tasks/.",
            summary.prefix,
            summary.id
        ));
    }
    // The full ID from here: a task created while the editor is open may share the prefix.
    let id = summary.id.to_string();
    let logged = runtime.block_on(events(&client, &id))?.len();
    let path: PathBuf = config
        .store_paths(&layout.home, &user_home)
        .get(&summary.context)
        .ok_or_else(|| miette!("no store for context {} in config.toml", summary.context))?
        .join("tasks")
        .join(summary.id.as_str())
        .join("task.md");
    let read = |path: &PathBuf| {
        fs::read_to_string(path)
            .into_diagnostic()
            .map_err(|error| miette!("cannot read {}: {error}", path.display()))
    };
    let original = read(&path)?;
    let command = editor(
        env::var("VISUAL").ok().as_deref(),
        env::var("EDITOR").ok().as_deref(),
    );
    // Through the shell, so that an editor with arguments (`code --wait`) works; the path is
    // passed as `$1`, never spliced into the command.
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{command} \"$1\""))
        .arg("sh")
        .arg(&path)
        .status()
        .into_diagnostic()
        .map_err(|error| miette!("cannot run the editor `{command}`: {error}"))?;
    if !status.success() {
        return Err(miette!(
            "the editor `{command}` exited with {status}: {} is left as it is now",
            path.display()
        ));
    }
    let saved = read(&path)?;
    if saved == original {
        return print(&format!("{} unchanged\n", summary.prefix));
    }
    let (saved_task, saved_notes) = match loom_core::parse_task_md(&saved) {
        Ok(document) => (
            document.task(loom_api::Timestamp::EPOCH),
            document.body().to_owned(),
        ),
        Err(error) => {
            return Err(miette!(
                "{}: {error}\nThe file is kept as saved: loomd shows the task read-only until it is fixed (`loom edit {}` again).",
                path.display(),
                summary.prefix
            ));
        }
    };
    let deadline = Instant::now() + EDIT_RECORD_TIMEOUT;
    loop {
        let after = runtime.block_on(get(&client, &id))?;
        if holds_saved(&after, &saved_task, &saved_notes) {
            // Read once the save is indexed: its hand-edit event and what came before it.
            let log = runtime.block_on(events(&client, &id))?;
            // The log is append-only, in the order loomd writes it: the events after the first
            // `logged` arrived while the editor was open. (A merge of logs from several
            // machines that reorders them would break this.)
            let appended = log.get(logged..).unwrap_or_default();
            let others = changed_meanwhile(appended);
            if !others.is_empty() {
                warn!(
                    "the task changed while it was open in the editor (by {}): your save replaced the file and loomd recorded it as an edit; `loom undo {}` reverts it",
                    others.join(", "),
                    summary.prefix
                );
            }
            return print(&render_change(&after));
        }
        if Instant::now() >= deadline {
            let log = runtime.block_on(events(&client, &id))?;
            return print(&timed_out(
                log.get(logged..).unwrap_or_default(),
                &summary.prefix,
                &after,
            ));
        }
        std::thread::sleep(EDIT_POLL);
    }
}

/// What `loom edit` prints when the index did not hold the save in time. A hand-edit event in
/// `appended` means loomd recorded the save and the task changed again after it (a `loom done`
/// elsewhere, a schedule, a sync); that is told, with the actors.
fn timed_out(appended: &[EventRecord], prefix: &str, after: &TaskDetail) -> String {
    if !appended.iter().any(|record| record.actor == Actor::File) {
        return format!(
            "{prefix} saved; loomd has not recorded it within {} s\n",
            EDIT_RECORD_TIMEOUT.as_secs()
        );
    }
    let after_save = changed_after_save(appended);
    let by = if after_save.is_empty() {
        String::new()
    } else {
        format!(" (by {})", after_save.join(", "))
    };
    warn!("the task changed again after the save{by}: `loom show {prefix}` shows it");
    format!(
        "{prefix} saved; the task changed again after the save\n{}",
        render_change(after)
    )
}

/// `GET /v1/tasks/{id}/events`: the event log of the task.
async fn event_log(client: &Client, id: &str) -> Result<TaskEvents> {
    client
        .get(&format!("{TASKS_PATH}/{id}/events"))
        .await
        .map_err(|error| api_error(&error))
}

/// The events of the task, in log order.
async fn events(client: &Client, id: &str) -> Result<Vec<EventRecord>> {
    Ok(event_log(client, id).await?.events)
}

/// `POST /v1/tasks/{prefix}/actions/{name}`.
async fn action(
    client: &Client,
    prefix: &str,
    name: &str,
    body: Option<&TaskActionRequest>,
) -> Result<TaskDetail> {
    client
        .post(&format!("{TASKS_PATH}/{prefix}/actions/{name}"), body)
        .await
        .map_err(|error| api_error(&error))
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    // The async runtime only drives the API client.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()
}

fn block_on<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    runtime()?.block_on(future)
}

async fn client(layout: &Layout) -> Result<Client> {
    Ok(connect(layout, true).await?.with_timeout(TASK_TIMEOUT))
}

/// The context of the command, from `-c`, the environment, then the current directory. Only
/// reading the current directory fails the command; a context that no rule gives is left to
/// the caller.
async fn context(
    global: &GlobalArgs,
    layout: &Layout,
    client: &Client,
) -> Result<Result<ResolvedContext, ContextError>> {
    let env_value = env::var(CONTEXT_ENV).ok();
    // The current directory is read only when `-c` and the environment give nothing.
    let (repo, rule) = if global.context.is_none() && env_value.as_deref().is_none_or(str::is_empty)
    {
        cwd_assignments(layout, client).await?
    } else {
        (None, None)
    };
    let resolved = resolve_context(
        global.context.as_ref(),
        env_value.as_deref(),
        repo.as_ref(),
        rule.as_ref(),
    );
    if let Ok(resolved) = &resolved {
        debug!(context = %resolved.context, source = ?resolved.source, "context resolved");
    }
    Ok(resolved)
}

/// `GET /v1/tasks/{prefix}`.
async fn get(client: &Client, prefix: &str) -> Result<TaskDetail> {
    client
        .get(&format!("{TASKS_PATH}/{prefix}"))
        .await
        .map_err(|error| api_error(&error))
}

fn output(value: &impl Serialize, json: bool, text: impl FnOnce() -> String) -> Result<()> {
    let text = if json {
        serde_json::to_string_pretty(value).into_diagnostic()? + "\n"
    } else {
        text()
    };
    print(&text)
}

/// The local date, as loomd reads it: relative due dates match its priority list.
fn local_today() -> Date {
    today(
        env::var(TEST_TODAY_ENV).ok().as_deref(),
        cfg!(debug_assertions),
        || jiff::Zoned::now().date(),
    )
}
