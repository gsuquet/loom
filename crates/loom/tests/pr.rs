//! `loom pr` end to end: a repository whose pushes go to a temporary bare remote, the `loomd`
//! built next to `loom`, and the fake forge in place of the GitHub API
//! (`LOOM_TEST_FORGE_API`, debug builds).

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{Link, MrOutcome, PrReport, TaskDetail};
use serde_json::json;
use tempfile::TempDir;

#[path = "../../loom-forge/tests/common/fake_forge.rs"]
mod fake_forge;
mod isolation;

use fake_forge::{FakeForge, Route, fixture};

const TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(20);
const TOKEN_ENV: &str = "LOOM_PR_TEST_TOKEN";
const TOKEN: &str = "fake-token-0123456789";
const FIND: &str = "/repos/acme/shop/pulls?head=acme%3Afix%2Flogin&state=open&per_page=100";
const CREATE: &str = "/repos/acme/shop/pulls";

const CONFIG: &str = r#"
[repos]
roots = ["~/src"]

[contexts.personal]
match.paths = ["~/src"]

[forges."github.com"]
kind = "github"
token_env = "LOOM_PR_TEST_TOKEN"
"#;

fn loom_bin() -> PathBuf {
    PathBuf::from(assert_cmd::cargo::cargo_bin!("loom"))
}

/// A loom home, a user home with the repository `~/src/shop` (remote `origin` on
/// `github.com/acme/shop`, pushed to the bare `~/remote.git`), and a fake forge.
struct Sandbox {
    dir: TempDir,
    forge: FakeForge,
}

impl Sandbox {
    fn new() -> Self {
        let loomd = loom_bin().with_file_name("loomd");
        assert!(loomd.is_file(), "{} is missing", loomd.display());
        let dir = tempfile::Builder::new()
            .prefix("lpr")
            .tempdir_in("/tmp")
            .unwrap();
        let sandbox = Self {
            dir,
            forge: FakeForge::start(),
        };
        fs::create_dir_all(sandbox.loom_home()).unwrap();
        fs::write(sandbox.loom_home().join("config.toml"), CONFIG).unwrap();
        let repo = sandbox.repo();
        fs::create_dir_all(&repo).unwrap();
        sandbox.git(&sandbox.home(), &["init", "--bare", "-q", "remote.git"]);
        sandbox.git(&repo, &["init", "-q", "-b", "main"]);
        sandbox.commit("first");
        sandbox.git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/shop.git",
            ],
        );
        let bare = sandbox.bare();
        sandbox.git(
            &repo,
            &["config", "remote.origin.pushurl", bare.to_str().unwrap()],
        );
        sandbox.git(&repo, &["push", "-q", "origin", "main"]);
        sandbox.git(&repo, &["update-ref", "refs/remotes/origin/main", "main"]);
        sandbox.git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        sandbox
    }

    fn loom_home(&self) -> PathBuf {
        self.dir.path().join("l")
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("u")
    }

    fn repo(&self) -> PathBuf {
        self.home().join("src/shop")
    }

    fn bare(&self) -> PathBuf {
        self.home().join("remote.git")
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .envs(isolation::isolated_env(&self.home()))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {}", stderr(&output));
        stdout(&output)
    }

    fn commit(&self, message: &str) {
        self.git(
            &self.repo(),
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                message,
            ],
        );
    }

    /// `loom --color=never <args>` in the repository, with the token and the fake forge.
    fn loom(&self, args: &[&str]) -> Output {
        Command::new(loom_bin())
            .env("LOOM_HOME", self.loom_home())
            .envs(isolation::isolated_env(&self.home()))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LOOM_TEST_FORGE_API", self.forge.base())
            .env(TOKEN_ENV, TOKEN)
            .env("PWD", self.repo())
            .current_dir(self.repo())
            .arg("--color=never")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.loom(args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            stderr(&output)
        );
        stdout(&output)
    }

    /// A ready task, focused, on the branch `fix/login` with one commit.
    fn task_on_branch(&self) -> String {
        let task: TaskDetail = serde_json::from_str(&self.ok(&[
            "add", "--ready", "--kind", "bug", "--json", "Login", "timeout",
        ]))
        .unwrap();
        let id = task.summary.id.to_string();
        self.ok(&["go", &id]);
        self.git(&self.repo(), &["checkout", "-q", "-b", "fix/login"]);
        self.commit("fix the login timeout");
        id
    }

    fn task(&self, id: &str) -> TaskDetail {
        serde_json::from_str(&self.ok(&["show", id, "--json"])).unwrap()
    }

    /// The branches of the bare remote.
    fn remote_branches(&self) -> String {
        self.git(
            &self.bare(),
            &["for-each-ref", "--format=%(refname)", "refs/heads"],
        )
    }

    /// The `events.jsonl` of the task.
    fn events(&self, id: &str) -> String {
        let mut found = Vec::new();
        find_files(&self.loom_home(), "events.jsonl", &mut found);
        let path = found
            .into_iter()
            .find(|path| path.parent().is_some_and(|dir| dir.ends_with(id)))
            .unwrap();
        fs::read_to_string(path).unwrap()
    }
}

fn find_files(dir: &Path, name: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_files(&path, name, found);
        } else if path.file_name().is_some_and(|file| file == name) {
            found.push(path);
        }
    }
}

/// Stops the loomd of the sandbox, if one runs, before the directory is removed.
impl Drop for Sandbox {
    fn drop(&mut self) {
        let lock = self.loom_home().join("run/loomd.lock");
        let Ok(file) = File::open(&lock) else {
            return;
        };
        if file.try_lock().is_ok() {
            return;
        }
        if let Some(pid) = fs::read_to_string(&lock)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
        }
        let deadline = Instant::now() + TIMEOUT;
        while file.try_lock().is_err() && Instant::now() < deadline {
            thread::sleep(POLL);
        }
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn draft_pull() -> Vec<u8> {
    let mut pull: serde_json::Value =
        serde_json::from_slice(&fixture("github/pull-412.v1.json")).unwrap();
    pull["draft"] = json!(true);
    serde_json::to_vec(&pull).unwrap()
}

fn delivery_keys(task: &TaskDetail) -> Vec<String> {
    task.summary
        .task
        .as_ref()
        .unwrap()
        .links
        .iter()
        .filter_map(|link| match link {
            Link::Delivery(mr) => Some(mr.key.clone()),
            Link::Origin(_) | Link::Related(_) | Link::Repo { .. } => None,
        })
        .collect()
}

#[test]
fn pr_pushes_the_branch_and_opens_a_draft_linked_to_the_focus_task() {
    let sandbox = Sandbox::new();
    let id = sandbox.task_on_branch();
    sandbox
        .forge
        .route(Route::get(FIND).times(1).body("[]"))
        .route(Route::post(CREATE).status(201).body(draft_pull()));

    let report: PrReport = serde_json::from_str(&sandbox.ok(&["pr", "--json"])).unwrap();
    assert!(!report.dry_run);
    assert!(report.push.pushed);
    assert_eq!(
        report.push.command,
        ["git", "push", "-u", "origin", "fix/login"]
    );
    assert_eq!(report.result.outcome, MrOutcome::Opened);
    assert_eq!(report.result.task.to_string(), id);
    assert_eq!(report.result.mr.as_ref().unwrap().key, "acme/shop#412");

    // Pushed with its upstream set.
    assert!(
        sandbox.remote_branches().contains("refs/heads/fix/login"),
        "{}",
        sandbox.remote_branches()
    );
    assert_eq!(
        sandbox
            .git(&sandbox.repo(), &["config", "branch.fix/login.remote"])
            .trim(),
        "origin"
    );
    // The draft request: title and kind template of the task, never its notes.
    let requests = sandbox.forge.requests();
    let lines: Vec<String> = requests.iter().map(fake_forge::Recorded::line).collect();
    assert_eq!(lines, [format!("GET {FIND}"), format!("POST {CREATE}")]);
    assert_eq!(
        requests[1].json(),
        json!({
            "title": "Login timeout",
            "head": "fix/login",
            "base": "main",
            "body": "## Symptoms\n\n## Cause\n\n## Fix\n\n## Verification\n",
            "draft": true,
        })
    );
    assert_eq!(
        requests[1].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    // The delivery link and the `mr` event.
    assert_eq!(delivery_keys(&sandbox.task(&id)), ["acme/shop#412"]);
    let events = sandbox.events(&id);
    assert!(
        events.contains(r#""type":"mr","action":"opened""#),
        "{events}"
    );

    // Again: the open MR of the branch is linked, none is opened, and nothing is recorded twice.
    sandbox
        .forge
        .route(Route::get(FIND).body(format!("[{}]", String::from_utf8(draft_pull()).unwrap())));
    let text = sandbox.ok(&["pr", &id]);
    assert!(text.contains("linked open MR acme/shop#412"), "{text}");
    assert_eq!(delivery_keys(&sandbox.task(&id)), ["acme/shop#412"]);
    assert_eq!(sandbox.events(&id), events);
}

#[test]
fn pr_links_the_open_mr_of_the_branch_instead_of_opening_one() {
    let sandbox = Sandbox::new();
    let id = sandbox.task_on_branch();
    sandbox
        .forge
        .route(Route::get(FIND).body(format!("[{}]", String::from_utf8(draft_pull()).unwrap())));
    let text = sandbox.ok(&["pr"]);
    assert!(text.starts_with("pushed fix/login to origin\n"), "{text}");
    assert!(text.contains("linked open MR acme/shop#412"), "{text}");
    assert_eq!(sandbox.forge.lines(), [format!("GET {FIND}")]);
    assert_eq!(delivery_keys(&sandbox.task(&id)), ["acme/shop#412"]);
    assert!(sandbox.events(&id).contains(r#""action":"linked""#));
}

#[test]
fn a_dry_run_prints_the_push_and_the_requests_and_does_neither() {
    let sandbox = Sandbox::new();
    let id = sandbox.task_on_branch();
    let text = sandbox.ok(&["pr", "-n", "--base", "main"]);
    assert!(
        text.starts_with("would run: git push -u origin fix/login\n"),
        "{text}"
    );
    let base = sandbox.forge.base();
    assert!(
        text.contains(&format!("would send: GET {base}{FIND}\n")),
        "{text}"
    );
    assert!(
        text.contains(&format!("would send: POST {base}{CREATE}\n")),
        "{text}"
    );
    assert!(text.contains("\"draft\": true"), "{text}");
    let report: PrReport = serde_json::from_str(&sandbox.ok(&["pr", "-n", "--json"])).unwrap();
    assert!(report.dry_run && !report.push.pushed);
    assert_eq!(report.result.outcome, MrOutcome::Planned);
    assert_eq!(report.result.mr, None);

    assert_eq!(sandbox.forge.lines(), [] as [String; 0]);
    assert!(!sandbox.remote_branches().contains("fix/login"));
    assert_eq!(delivery_keys(&sandbox.task(&id)), [] as [String; 0]);
}

#[test]
fn pr_refuses_the_default_branch_and_a_branch_with_nothing_ahead() {
    let sandbox = Sandbox::new();
    let id = sandbox.task_on_branch();
    sandbox.git(&sandbox.repo(), &["checkout", "-q", "main"]);
    let output = sandbox.loom(&["pr", &id]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("`main` is the default branch"),
        "{}",
        stderr(&output)
    );
    sandbox.git(&sandbox.repo(), &["checkout", "-q", "-b", "empty"]);
    let output = sandbox.loom(&["pr", &id]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("`empty` has no commit ahead of `main`"),
        "{}",
        stderr(&output)
    );
    let output = sandbox.loom(&["pr", &id, "--base", "empty"]);
    assert!(
        stderr(&output).contains("`empty` is the target branch"),
        "{}",
        stderr(&output)
    );
    assert_eq!(sandbox.forge.lines(), [] as [String; 0]);
    assert_eq!(sandbox.remote_branches().trim(), "refs/heads/main");
}

#[test]
fn a_refused_push_shows_the_git_error_and_opens_no_mr() {
    let sandbox = Sandbox::new();
    let id = sandbox.task_on_branch();
    let hook = sandbox.bare().join("hooks/pre-receive");
    fs::write(
        &hook,
        "#!/bin/sh\necho 'pushes to fix/* are closed today' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();

    let output = sandbox.loom(&["pr", &id]);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("git push -u origin fix/login"), "{error}");
    assert!(error.contains("no MR opened"), "{error}");
    assert!(
        error.contains("pushes to fix/* are closed today"),
        "{error}"
    );
    assert_eq!(sandbox.forge.lines(), [] as [String; 0]);
    assert_eq!(delivery_keys(&sandbox.task(&id)), [] as [String; 0]);
}

#[test]
fn a_context_without_a_credential_for_the_host_opens_nothing() {
    let sandbox = Sandbox::new();
    fs::write(
        sandbox.loom_home().join("config.toml"),
        CONFIG.replace("token_env = \"LOOM_PR_TEST_TOKEN\"\n", ""),
    )
    .unwrap();
    let id = sandbox.task_on_branch();
    let output = sandbox.loom(&["pr", "-n", &id]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("context `personal` has no forge credential for `github.com`"),
        "{}",
        stderr(&output)
    );
    assert_eq!(sandbox.forge.lines(), [] as [String; 0]);
}
