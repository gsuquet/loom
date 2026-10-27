//! `loom prompt`: reads the current directory, asks loomd within its budget, prints the line.
//! Decisions and rendering are in `loom::prompt`. Never fails, never prints on stderr.

use std::env;
use std::fs;
use std::io;
use std::time::{Duration, Instant};

use loom::cli::{GlobalArgs, PromptArgs};
use loom::conventions::write_out;
use loom::prompt::{CwdRepo, TEST_BUDGET_ENV, budget, needs_cwd, prompt_context, render};
use loom::repo::find_by_path;
use loom::task::{CONTEXT_ENV, focus_api_path};
use loom_api::{ContextAssignment, ContextName, FocusList, PromptSegment, RepoList};
use loom_config::layout::Layout;
use loom_repo::find_root;

use crate::client::Client;
use crate::layout;
use crate::repo_cmd::{REPOS_PATH, directory_rule, logical_cwd};

/// Runs `loom prompt`. Whatever fails gives a shorter line, or an empty one.
pub fn run(args: &PromptArgs, global: &GlobalArgs) {
    let started = Instant::now();
    let budget = budget(
        env::var(TEST_BUDGET_ENV).ok().as_deref(),
        cfg!(debug_assertions),
    );
    let deadline = started.checked_add(budget).unwrap_or(started);
    let segment = layout()
        .ok()
        .map(|layout| segment(&layout, global.context.as_ref(), deadline))
        .unwrap_or_default();
    let text = if args.json {
        serde_json::to_string_pretty(&segment)
            .map(|text| text + "\n")
            .unwrap_or_default()
    } else {
        render(&segment, args.max_title, args.shell)
    };
    // A closed stdout is not an error, and nothing goes to stderr.
    let _ = write_out(&mut io::stdout().lock(), &text);
}

fn segment(layout: &Layout, flag: Option<&ContextName>, deadline: Instant) -> PromptSegment {
    let env_value = env::var(CONTEXT_ENV).ok();
    let env_value = env_value.as_deref();
    let (root, rule) = if needs_cwd(flag, env_value) {
        match logical_cwd() {
            Ok(cwd) => {
                let home = env::home_dir().filter(|home| !home.as_os_str().is_empty());
                let rule = directory_rule(layout, &cwd, home.as_deref());
                (find_root(&cwd).ok().flatten(), rule)
            }
            Err(_) => return PromptSegment::default(),
        }
    } else {
        (None, None)
    };

    // No socket: loomd is not running, and it is never started for this.
    let answers = if fs::symlink_metadata(&layout.socket).is_ok() {
        ask_loomd(layout, root.is_some(), deadline)
    } else {
        Answers::default()
    };

    let repo_context: Option<ContextAssignment> = match (&root, &answers.repos) {
        (Some(root), Some(list)) => {
            find_by_path(&list.repos, root, |path| fs::canonicalize(path).ok())
                .map(|repo| repo.identity.context.clone())
        }
        _ => None,
    };
    let repo = match (&root, &repo_context) {
        (None, _) => CwdRepo::Outside,
        (Some(_), Some(assignment)) => CwdRepo::Indexed(assignment),
        (Some(_), None) => CwdRepo::Unknown,
    };
    let context = prompt_context(flag, env_value, repo, rule.as_ref());
    let focus = context.as_ref().and_then(|context| {
        answers.focus.and_then(|list| {
            list.entries
                .into_iter()
                .find(|entry| entry.context == *context)
        })
    });
    PromptSegment { context, focus }
}

/// What loomd answered within the budget.
#[derive(Default)]
struct Answers {
    /// Focus of every context.
    focus: Option<FocusList>,
    /// The repository index, when the current directory is in a repository.
    repos: Option<RepoList>,
}

/// Asks loomd for the focus list, and for the repositories when `in_repo`, both at once,
/// until `deadline`.
fn ask_loomd(layout: &Layout, in_repo: bool, deadline: Instant) -> Answers {
    let left: Duration = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Answers::default();
    }
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return Answers::default();
    };
    let client = Client::new(layout.socket.clone()).with_timeout(left);
    let focus_path = focus_api_path(None);
    runtime.block_on(async {
        let focus = client.get::<FocusList>(&focus_path);
        let repos = async {
            if in_repo {
                client.get::<RepoList>(REPOS_PATH).await.ok()
            } else {
                None
            }
        };
        let (focus, repos) = tokio::join!(focus, repos);
        Answers {
            focus: focus.ok(),
            repos,
        }
    })
}
