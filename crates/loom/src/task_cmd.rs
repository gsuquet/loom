//! `loom add|ls|show`: resolves the context and calls loomd. Decisions and output are in
//! `loom::task`.

use std::env;
use std::time::Duration;

use loom::cli::{AddArgs, GlobalArgs, LsArgs, ShowArgs};
use loom::task::{
    AddRequest, CONTEXT_ENV, KindArg, ResolvedContext, TEST_TODAY_ENV, kind_of, list_api_path,
    new_task, parse_date, render_detail, render_list, resolve_context, today,
};
use loom_api::{ContextName, Date, TaskDetail, TaskId, TaskList};
use loom_config::layout::Layout;
use miette::{IntoDiagnostic as _, Result, miette};
use serde::Serialize;
use tracing::debug;

use crate::client::Client;
use crate::print;
use crate::repo_cmd::{api_error, connect, cwd_assignments};

/// Path of the task list and of the creation endpoint.
const TASKS_PATH: &str = "/v1/tasks";
/// Longest wait for a task request: right after it starts, loomd waits up to 10 s for the tasks
/// it indexes before it answers a read or resolves an ID, and the default timeout is 5 s.
const TASK_TIMEOUT: Duration = Duration::from_secs(15);

/// `loom add`.
pub fn add(args: &AddArgs, global: &GlobalArgs, layout: &Layout) -> Result<()> {
    if args.format.is_some() && args.kind != KindArg::Content {
        return Err(miette!("--format applies to --kind content only"));
    }
    let due = args
        .due
        .as_deref()
        .map(|text| parse_date(text, local_today()).into_diagnostic())
        .transpose()?;
    block_on(async {
        let client = client(layout).await?;
        let resolved = context(global, layout, &client).await?;
        let parent = match &args.parent {
            Some(prefix) => Some(resolve_id(&client, prefix).await?),
            None => None,
        };
        let request = new_task(AddRequest {
            title: args.title.join(" "),
            context: resolved.context,
            kind: kind_of(args.kind, args.format),
            importance: args.importance.map(Into::into),
            due,
            parent,
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
            Some(context(global, layout, &client).await?.context)
        };
        let path = list_api_path(
            context.as_ref(),
            args.status.map(loom::cli::StatusArg::as_str),
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
        output(&detail, args.json, || render_detail(&detail))
    })
}

fn block_on<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    // The async runtime only drives the API client.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?
        .block_on(future)
}

async fn client(layout: &Layout) -> Result<Client> {
    Ok(connect(layout, true).await?.with_timeout(TASK_TIMEOUT))
}

/// The context of the command, from `-c`, the environment, then the current directory.
async fn context(global: &GlobalArgs, layout: &Layout, client: &Client) -> Result<ResolvedContext> {
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
    )
    .into_diagnostic()?;
    debug!(context = %resolved.context, source = ?resolved.source, "context resolved");
    Ok(resolved)
}

/// `GET /v1/tasks/{prefix}`.
async fn get(client: &Client, prefix: &str) -> Result<TaskDetail> {
    client
        .get(&format!("{TASKS_PATH}/{prefix}"))
        .await
        .map_err(|error| api_error(&error))
}

/// The ID a prefix stands for.
async fn resolve_id(client: &Client, prefix: &str) -> Result<TaskId> {
    Ok(get(client, prefix).await?.summary.id)
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
