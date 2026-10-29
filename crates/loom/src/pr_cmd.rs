//! `loom pr`: reads the branch with git, pushes it, then asks loomd to open or link its draft
//! MR. Decisions and output are in `loom::pr`.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use loom::cli::{GlobalArgs, PrArgs};
use loom::pr::{PrRefusal, check_branch, check_upstream, default_branch, push_command, render};
use loom::task::focus_api_path;
use loom_api::{FocusList, MrOpenRequest, MrOpenResult, PrReport, PushPlan, RepoSummary};
use loom_config::layout::Layout;
use miette::{IntoDiagnostic as _, Result, miette};

use crate::print;
use crate::repo_cmd::{api_error, cwd_repo, logical_cwd};
use crate::task_cmd::{block_on, client, context};

/// Longest wait for loomd: after a 401, the token command runs twice (10 s each) and up to four
/// forge calls are made (20 s each).
const MR_TIMEOUT: Duration = Duration::from_secs(120);

/// Runs `loom pr`.
pub fn run(args: &PrArgs, global: &GlobalArgs, layout: &Layout) -> Result<()> {
    let cwd = logical_cwd().into_diagnostic()?;
    block_on(async {
        let client = client(layout).await?;
        let repo = cwd_repo(&client)
            .await?
            .ok_or_else(|| miette!("the current directory is not in a git repository"))?;
        let branch = current_branch(&cwd)?;
        let remote = push_remote(&cwd, &branch, &repo)?;
        let location = repo
            .identity
            .remotes
            .iter()
            .find(|candidate| candidate.name == remote)
            .and_then(|candidate| candidate.location.clone())
            .ok_or_else(|| {
                miette!(
                    "the remote `{remote}` has no forge URL (a local path, or a URL loom cannot read)"
                )
            })?;
        let default = git_stdout(
            &cwd,
            &[
                "symbolic-ref",
                "--quiet",
                "--short",
                &format!("refs/remotes/{remote}/HEAD"),
            ],
        )
        .and_then(|text| default_branch(&remote, &text))
        .or_else(|| {
            repo.state
                .as_ref()
                .and_then(|state| state.default_branch.as_known().cloned().flatten())
        })
        .ok_or_else(|| {
            miette!(
                "the default branch of `{remote}` is unknown: run `git remote set-head {remote} --auto`"
            )
        })?;
        let base = args.base.clone().unwrap_or_else(|| default.clone());
        let ahead = commits_ahead(&cwd, &remote, &base)?;
        check_branch(&branch, &base, &default, ahead).map_err(|refusal| miette!("{refusal}"))?;

        let id = match &args.id {
            Some(prefix) => prefix.clone(),
            None => focus_task(global, layout, &client).await?,
        };
        let request = MrOpenRequest {
            host: location.host.clone(),
            project: location.path.clone(),
            head: branch.clone(),
            base,
            dry_run: args.dry_run,
        };
        let command = push_command(&remote, &branch);
        let path = format!("/v1/tasks/{id}/mr");
        let client = client.with_timeout(MR_TIMEOUT);
        // A dry run first: loomd checks the task and the credential, and sends nothing. A
        // refusal comes before the push.
        let planned: MrOpenResult = client
            .post(
                &path,
                Some(&MrOpenRequest {
                    dry_run: true,
                    ..request.clone()
                }),
            )
            .await
            .map_err(|error| api_error(&error))?;
        let result = if args.dry_run {
            planned
        } else {
            push(&cwd, &command)?;
            client.post(&path, Some(&request)).await.map_err(|error| {
                api_error(&error).wrap_err(
                    "the branch is pushed, but no MR is linked: run `loom pr` again to open its MR",
                )
            })?
        };
        let report = PrReport {
            dry_run: args.dry_run,
            push: PushPlan {
                remote,
                branch,
                command,
                pushed: !args.dry_run,
            },
            result,
        };
        let text = if args.json {
            serde_json::to_string_pretty(&report).into_diagnostic()? + "\n"
        } else {
            render(&report)
        };
        print(&text)
    })
}

/// The focus task of the context, resolved as for `loom add`.
async fn focus_task(
    global: &GlobalArgs,
    layout: &Layout,
    client: &crate::client::Client,
) -> Result<String> {
    let context = context(global, layout, client)
        .await?
        .into_diagnostic()?
        .context;
    let list: FocusList = client
        .get(&focus_api_path(Some(&context)))
        .await
        .map_err(|error| api_error(&error))?;
    list.entries
        .first()
        .map(|entry| entry.task.id.to_string())
        .ok_or_else(|| {
            miette!(
                "no focus task in context {context}: run `loom go <id>` to focus a task, or give the task ID"
            )
        })
}

fn git(cwd: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .into_diagnostic()
}

/// The trimmed standard output of a git command that succeeds, `None` otherwise.
fn git_stdout(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = git(cwd, args).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn current_branch(cwd: &Path) -> Result<String> {
    git_stdout(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok_or_else(|| miette!("{}", PrRefusal::Detached))
}

/// The upstream remote of `branch`, otherwise the primary remote of the repository, otherwise
/// `origin`.
fn push_remote(cwd: &Path, branch: &str, repo: &RepoSummary) -> Result<String> {
    let remote = git_stdout(
        cwd,
        &["config", "--get", &format!("branch.{branch}.remote")],
    )
    .or_else(|| repo.identity.primary_remote.clone())
    .unwrap_or_else(|| "origin".to_owned());
    let merge = git_stdout(cwd, &["config", "--get", &format!("branch.{branch}.merge")]);
    check_upstream(branch, &remote, merge.as_deref()).map_err(|refusal| miette!("{refusal}"))?;
    Ok(remote)
}

/// The commits of `HEAD` not in the base: its remote-tracking branch, otherwise the local
/// branch.
fn commits_ahead(cwd: &Path, remote: &str, base: &str) -> Result<u64> {
    let candidates = [
        format!("refs/remotes/{remote}/{base}"),
        format!("refs/heads/{base}"),
    ];
    let reference = candidates
        .iter()
        .find(|candidate| {
            git_stdout(cwd, &["rev-parse", "--verify", "--quiet", candidate]).is_some()
        })
        .ok_or_else(|| {
            miette!("the target branch `{base}` is not found: run `git fetch {remote}` first")
        })?;
    let count = git_stdout(cwd, &["rev-list", "--count", &format!("{reference}..HEAD")])
        .ok_or_else(|| miette!("cannot count the commits ahead of `{base}`"))?;
    count
        .parse()
        .map_err(|_| miette!("unexpected `git rev-list --count` output: {count}"))
}

/// Runs the push. A refused push (a hook, the credentials) fails with the git error, before
/// any forge request.
fn push(cwd: &Path, command: &[String]) -> Result<()> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| miette!("empty push command"))?;
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .into_diagnostic()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(miette!(
        "`{}` failed ({}), no MR opened:\n{}",
        command.join(" "),
        output.status,
        stderr.trim_end()
    ))
}
