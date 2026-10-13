//! `loom repo` end to end: fixture repositories under a temporary `HOME` and `LOOM_HOME`, the
//! `loomd` built next to `loom`, started by `loom` itself.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fmt::Write as _;
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{CheckStatus, DoctorReport, RepoList, RepoSummary, ScanResult};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tempfile::TempDir;

mod isolation;

const TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(20);

const CONFIG: &str = r#"
[repos]
roots = ["~/perso", "~/src"]

[contexts.personal]
match.paths = ["~/perso"]

[contexts.work]
match.paths = ["~/work"]
match.hosts = ["gitlab.com/company"]
"#;

fn loom_bin() -> PathBuf {
    PathBuf::from(assert_cmd::cargo::cargo_bin!("loom"))
}

/// A loom home and a user home under `/tmp` (the macOS `$TMPDIR` is too long for a socket
/// path), with fixture repositories:
///
/// - `~/perso/a` (`personal/a`), `~/perso/b`, `~/perso/loom` (`personal/loom`);
/// - `~/perso/clients/loom`, remote on `gitlab.com/company` (`work/loom`);
/// - `~/src/tool`: under a root, no context (`unassigned/tool`);
/// - `~/elsewhere/ext`: outside the roots, not indexed;
/// - `~/work`: a directory of the `work` context, not a repository.
struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new(config: &str) -> Self {
        let loomd = loom_bin().with_file_name("loomd");
        assert!(loomd.is_file(), "{} is missing", loomd.display());
        let dir = tempfile::Builder::new()
            .prefix("loom-repo-")
            .tempdir_in("/tmp")
            .unwrap();
        let sandbox = Self { dir };
        fs::create_dir_all(sandbox.loom_home()).unwrap();
        fs::write(sandbox.loom_home().join("config.toml"), config).unwrap();
        let home = sandbox.home();
        fake_repo(&home.join("perso/a"), &[]);
        fs::create_dir_all(home.join("perso/a/sub/dir")).unwrap();
        fake_repo(&home.join("perso/b"), &[]);
        fake_repo(&home.join("perso/loom"), &[]);
        fake_repo(
            &home.join("perso/clients/loom"),
            &[("origin", "https://gitlab.com/company/loom.git")],
        );
        fake_repo(&home.join("src/tool"), &[]);
        fake_repo(&home.join("elsewhere/ext"), &[]);
        fs::create_dir_all(home.join("elsewhere/ext/src")).unwrap();
        fs::create_dir_all(home.join("work")).unwrap();
        sandbox
    }

    fn loom_home(&self) -> PathBuf {
        self.dir.path().join("l")
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("u")
    }

    fn socket(&self) -> PathBuf {
        self.loom_home().join("run/loomd.sock")
    }

    /// `loom --color=never <args>` run in `cwd` (as a shell would: `PWD` set), with `extra`
    /// environment variables.
    fn loom_with(&self, cwd: &Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut command = Command::new(loom_bin());
        command
            .env("LOOM_HOME", self.loom_home())
            .envs(isolation::isolated_env(&self.home()))
            .env("PWD", cwd)
            .env_remove("NO_COLOR")
            .current_dir(cwd)
            .arg("--color=never")
            .args(args)
            .stdin(Stdio::null());
        for (name, value) in extra {
            command.env(name, value);
        }
        command.output().unwrap()
    }

    fn loom(&self, args: &[&str]) -> Output {
        self.loom_with(&self.home(), args, &[])
    }

    fn loom_in(&self, cwd: &Path, args: &[&str]) -> Output {
        self.loom_with(cwd, args, &[])
    }

    /// Runs `args`, expecting success; stdout.
    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.loom_in(cwd, args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            stderr(&output)
        );
        stdout(&output)
    }

    fn slugs(&self) -> Vec<String> {
        let list: RepoList = valid(&self.ok(&self.home(), &["repo", "ls", "--json"]));
        list.repos
            .into_iter()
            .map(|repo| repo.identity.slug)
            .collect()
    }

    fn show(&self, cwd: &Path, args: &[&str]) -> RepoSummary {
        let mut full = vec!["repo", "show", "--json"];
        full.extend_from_slice(args);
        valid(&self.ok(cwd, &full))
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

/// A repository at `dir` with a hand-written `.git` directory and `remotes` (name, URL).
fn fake_repo(dir: &Path, remotes: &[(&str, &str)]) {
    let git_dir = dir.join(".git");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let mut config = String::new();
    for (name, url) in remotes {
        writeln!(config, "[remote \"{name}\"]\n\turl = {url}").unwrap();
    }
    fs::write(git_dir.join("config"), config).unwrap();
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// `text` parsed as `T`, after checking it against the schema of `T`.
fn valid<T: DeserializeOwned + JsonSchema>(text: &str) -> T {
    let value: Value = serde_json::from_str(text).unwrap();
    let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "schema violations: {errors:?}\n{text}");
    serde_json::from_value(value).unwrap()
}

#[test]
fn ls_starts_loomd_and_lists_in_every_format() {
    let sandbox = Sandbox::new(CONFIG);
    let home = sandbox.home();

    // Starts loomd and waits for its first scan.
    let output = sandbox.loom(&["repo", "ls"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let table = stdout(&output);
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines.len(), 6, "{table}");
    assert!(lines[0].starts_with("  SLUG"), "{table}");
    assert!(lines[4].starts_with("! unassigned/tool"), "{table}");
    assert!(lines[1].starts_with("  personal/a "), "{table}");
    assert!(
        stderr(&output).contains("1 repositories (marked `!`)"),
        "{}",
        stderr(&output)
    );

    let paths = sandbox.ok(&home, &["repo", "ls", "--format", "paths"]);
    let expected: String = [
        "perso/a",
        "perso/b",
        "perso/loom",
        "src/tool",
        "perso/clients/loom",
    ]
    .iter()
    .fold(String::new(), |mut text, path| {
        writeln!(text, "{}", home.join(path).display()).unwrap();
        text
    });
    assert_eq!(paths, expected);

    // Sorted by context, then name.
    assert_eq!(
        sandbox.slugs(),
        [
            "personal/a",
            "personal/b",
            "personal/loom",
            "unassigned/tool",
            "work/loom"
        ]
    );

    let work = sandbox.ok(&home, &["-c", "work", "repo", "ls", "--format=paths"]);
    assert_eq!(
        work,
        format!("{}\n", home.join("perso/clients/loom").display())
    );
    let unassigned = sandbox.ok(&home, &["repo", "ls", "--json", "-c", "unassigned"]);
    let list: RepoList = valid(&unassigned);
    assert_eq!(list.repos.len(), 1);

    let usage = sandbox.loom(&["repo", "ls", "--json", "--format", "paths"]);
    assert_eq!(usage.status.code(), Some(2));
}

#[test]
fn show_resolves_paths_slugs_and_names() {
    let sandbox = Sandbox::new(CONFIG);
    let home = sandbox.home();
    let a = home.join("perso/a");
    sandbox.ok(&home, &["repo", "scan"]);

    // Slug, with schema-valid JSON; showing marks it used.
    let shown = sandbox.show(&home, &["work/loom"]);
    assert_eq!(shown.identity.path, home.join("perso/clients/loom"));
    assert!(shown.last_used.is_some());
    assert!(
        shown.state.is_some(),
        "the state is read when there is none"
    );

    // Paths: relative, absolute, `~`, below the root.
    assert_eq!(
        sandbox.show(&home.join("perso"), &["./b"]).identity.slug,
        "personal/b"
    );
    let sub = a.join("sub/dir");
    assert_eq!(
        sandbox.show(&home, &[sub.to_str().unwrap()]).identity.slug,
        "personal/a"
    );
    assert_eq!(
        sandbox.show(&home, &["~/perso/a/sub"]).identity.slug,
        "personal/a"
    );
    // No argument: the repository of the current directory, also without `PWD`.
    assert_eq!(sandbox.show(&sub, &[]).identity.slug, "personal/a");
    let output = Command::new(loom_bin())
        .env("LOOM_HOME", sandbox.loom_home())
        .envs(isolation::isolated_env(&home))
        .env_remove("PWD")
        .current_dir(&sub)
        .args(["repo", "show", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        valid::<RepoSummary>(&stdout(&output)).identity.slug,
        "personal/a"
    );

    // A unique name.
    assert_eq!(
        sandbox.show(&home, &["tool"]).identity.slug,
        "unassigned/tool"
    );

    // An ambiguous name, from a directory without context: the candidates, exit 1.
    let output = sandbox.loom(&["repo", "show", "loom"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("`loom` names several repositories"), "{text}");
    assert!(
        text.contains(&format!(
            "  personal/loom ({})",
            home.join("perso/loom").display()
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "  work/loom ({})",
            home.join("perso/clients/loom").display()
        )),
        "{text}"
    );
    // The repository of the current directory decides, then `match.paths`, then `-c` first.
    assert_eq!(sandbox.show(&a, &["loom"]).identity.slug, "personal/loom");
    assert_eq!(
        sandbox.show(&home.join("work"), &["loom"]).identity.slug,
        "work/loom"
    );
    assert_eq!(
        sandbox.show(&a, &["-c", "work", "loom"]).identity.slug,
        "work/loom"
    );

    // Not a repository, not a name.
    let output = sandbox.loom(&["repo", "show"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("is not inside a git repository"),
        "{}",
        stderr(&output)
    );
    let output = sandbox.loom(&["repo", "show", "nope"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no repository `nope`"),
        "{}",
        stderr(&output)
    );

    // A path outside the index is registered.
    let ext = sandbox.show(&home, &["~/elsewhere/ext/src"]);
    assert_eq!(ext.identity.slug, "unassigned/ext");
    assert!(ext.registered);

    // Human card.
    let card = sandbox.ok(&home, &["repo", "show", "personal/a"]);
    assert!(card.starts_with("slug:           personal/a\n"), "{card}");
    assert!(
        card.contains("context:        personal (match.paths \"~/perso\")\n"),
        "{card}"
    );
}

#[test]
fn register_and_forget() {
    let sandbox = Sandbox::new(CONFIG);
    let home = sandbox.home();
    let ext = home.join("elsewhere/ext");

    let dry = sandbox.ok(&ext.join("src"), &["repo", "register", "-n"]);
    assert_eq!(dry, format!("would register {}\n", ext.display()));
    // `-n` needs no loomd.
    assert!(!sandbox.socket().exists());

    let registered = sandbox.ok(&home, &["repo", "register", "elsewhere/ext/src"]);
    assert_eq!(
        registered,
        format!("registered unassigned/ext: {}\n", ext.display())
    );
    assert!(sandbox.slugs().contains(&"unassigned/ext".to_owned()));

    let dry = sandbox.ok(&home, &["repo", "forget", "ext", "-n"]);
    assert_eq!(
        dry,
        format!("would forget unassigned/ext: {}\n", ext.display())
    );
    let forgotten: RepoSummary =
        valid(&sandbox.ok(&home, &["repo", "forget", "unassigned/ext", "--json"]));
    assert_eq!(forgotten.identity.path, ext);
    assert!(!sandbox.slugs().contains(&"unassigned/ext".to_owned()));

    let again = sandbox.loom(&["repo", "forget", "unassigned/ext"]);
    assert_eq!(again.status.code(), Some(1));
    // A path of a repository the index does not have is not registered by `forget`.
    let path = sandbox.loom(&["repo", "forget", ext.to_str().unwrap()]);
    assert_eq!(path.status.code(), Some(1));
    assert!(
        stderr(&path).contains("is not in the index"),
        "{}",
        stderr(&path)
    );

    let not_a_repo = sandbox.loom(&["repo", "register", "work"]);
    assert_eq!(not_a_repo.status.code(), Some(1));
    assert!(
        stderr(&not_a_repo).contains("is not inside a git repository"),
        "{}",
        stderr(&not_a_repo)
    );
}

#[test]
fn rebuild_needs_a_confirmation() {
    let sandbox = Sandbox::new(CONFIG);
    let home = sandbox.home();
    sandbox.ok(&home, &["repo", "register", "elsewhere/ext"]);
    fake_repo(&home.join("perso/new"), &[]);

    let dry = sandbox.ok(&home, &["repo", "scan", "-n"]);
    assert_eq!(dry, "would add 1, update 0, remove 0\n");
    let result: ScanResult =
        valid(&sandbox.ok(&home, &["repo", "scan", "--rebuild", "-n", "--json"]));
    assert_eq!((result.added, result.removed), (6, 6));
    assert!(!sandbox.slugs().contains(&"personal/new".to_owned()));

    // stdin is not a terminal and no `-y`: refused, nothing changed.
    let refused = sandbox.loom(&["repo", "scan", "--rebuild"]);
    assert_eq!(refused.status.code(), Some(1));
    let text = stderr(&refused);
    assert!(
        text.contains(
            "confirmation required but stdin is not a terminal; pass -y/--yes to confirm"
        ),
        "{text}"
    );
    assert!(
        text.contains("1 registered repositories outside the roots will be lost"),
        "{text}"
    );
    assert!(
        text.contains(&home.join("elsewhere/ext").display().to_string()),
        "{text}"
    );
    assert!(sandbox.slugs().contains(&"unassigned/ext".to_owned()));

    let rebuilt = sandbox.ok(&home, &["repo", "scan", "--rebuild", "-y"]);
    assert!(
        rebuilt.starts_with("6 added, 0 updated, 6 removed in "),
        "{rebuilt}"
    );
    let slugs = sandbox.slugs();
    assert!(slugs.contains(&"personal/new".to_owned()), "{slugs:?}");
    assert!(!slugs.contains(&"unassigned/ext".to_owned()), "{slugs:?}");
}

/// Polls the index until `path` is in it.
fn wait_for_path(sandbox: &Sandbox, path: &Path) -> RepoSummary {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let list: RepoList = valid(&sandbox.ok(&sandbox.home(), &["repo", "ls", "--json"]));
        if let Some(repo) = list
            .repos
            .into_iter()
            .find(|repo| repo.identity.path == path)
        {
            return repo;
        }
        assert!(
            Instant::now() < deadline,
            "{} never indexed",
            path.display()
        );
        thread::sleep(POLL);
    }
}

#[test]
fn any_command_registers_the_repository_it_runs_in() {
    let sandbox = Sandbox::new(CONFIG);
    let home = sandbox.home();
    let ext = home.join("elsewhere/ext");
    sandbox.ok(&home, &["repo", "scan"]);

    sandbox.ok(&ext.join("src"), &["version"]);
    let repo = wait_for_path(&sandbox, &ext);
    assert!(repo.registered);
    assert!(repo.last_used.is_some());

    // A repository already indexed is only marked used.
    sandbox.ok(&home.join("perso/b"), &["version"]);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        // Not `repo show`, which marks it used itself.
        let b = wait_for_path(&sandbox, &home.join("perso/b"));
        assert!(!b.registered);
        if b.last_used.is_some() {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(POLL);
    }
}

#[test]
fn register_on_use_in_the_background_is_silent_and_never_fails() {
    let sandbox = Sandbox::new(CONFIG);
    let home = sandbox.home();
    let ext = home.join("elsewhere/ext");
    let background = ["repo", "register", "--on-use", "--background"];

    // No loomd: nothing printed, exit 0, loomd not started.
    let output = sandbox.loom_in(&ext, &background);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    assert!(!sandbox.socket().exists());
    // `--on-use` alone fails without loomd, and does not start it either.
    let output = sandbox.loom_in(&ext, &["repo", "register", "--on-use"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("loomd is not running"));
    assert!(!sandbox.socket().exists());
    // Outside a repository, with `-v`: still nothing.
    let output = sandbox.loom_in(
        &home.join("work"),
        &[
            "-v",
            background[0],
            background[1],
            background[2],
            background[3],
        ],
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());

    sandbox.ok(&home, &["repo", "scan"]);
    let output = sandbox.loom_in(&ext.join("src"), &background);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    let repo = wait_for_path(&sandbox, &ext);
    assert!(repo.registered && repo.last_used.is_some());

    // `--on-use` alone: an indexed repository is only marked used.
    let used = sandbox.ok(&home, &["repo", "register", "--on-use", "perso/b"]);
    assert_eq!(
        used,
        format!("used personal/b: {}\n", home.join("perso/b").display())
    );
    let b = wait_for_path(&sandbox, &home.join("perso/b"));
    assert!(!b.registered && b.last_used.is_some());
}

/// The executable `name` in `PATH`; `None`, with a message, when there is none.
fn find_shell(name: &str) -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    });
    if found.is_none() {
        eprintln!("SKIPPED: {name} is not installed on this runner");
    }
    found
}

#[test]
fn entering_a_repository_in_the_shell_registers_it() {
    let sandbox = Sandbox::new(CONFIG);
    let home = sandbox.home();
    sandbox.ok(&home, &["repo", "scan"]);
    let bin = loom_bin().parent().unwrap().to_path_buf();
    let shells: [(&str, &[&str], &str); 2] = [
        ("zsh", &["-f"], ":"),
        // bash runs `PROMPT_COMMAND` at each prompt, which a script has none of.
        (
            "bash",
            &["--norc", "--noprofile"],
            "eval \"$PROMPT_COMMAND\"",
        ),
    ];
    for (name, no_rc, prompt) in shells {
        let Some(shell) = find_shell(name) else {
            continue;
        };
        let repo = home.join("elsewhere").join(name);
        fake_repo(&repo, &[]);
        fs::create_dir_all(repo.join("src")).unwrap();
        let script = format!(
            "eval \"$(loom shell-init {name})\"\n{prompt}\ncd '{}'\n{prompt}\n",
            repo.join("src").display()
        );
        let output = Command::new(shell)
            .env_clear()
            .envs(isolation::isolated_env(&home))
            .env("LOOM_HOME", sandbox.loom_home())
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("PWD", &home)
            .current_dir(&home)
            .args(no_rc)
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{name}: {output:?}");
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{name}: {output:?}"
        );
        let registered = wait_for_path(&sandbox, &repo);
        assert!(registered.registered, "{name}");
        assert!(registered.last_used.is_some(), "{name}");
    }
}

#[test]
fn registration_on_use_never_starts_loomd() {
    let sandbox = Sandbox::new(CONFIG);
    let output = sandbox.loom_in(&sandbox.home().join("perso/a"), &["version"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!sandbox.socket().exists());
    assert!(!sandbox.loom_home().join("run").exists());
}

#[test]
fn registration_on_use_gives_up_after_its_budget() {
    let sandbox = Sandbox::new(CONFIG);
    // Accepts connections, never answers.
    fs::create_dir_all(sandbox.loom_home().join("run")).unwrap();
    let _listener = UnixListener::bind(sandbox.socket()).unwrap();
    let started = Instant::now();
    let output = sandbox.loom_in(&sandbox.home().join("perso/a"), &["version"]);
    let elapsed = started.elapsed();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    // A request without budget would wait 5 s.
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
}

/// An `ssh` that greets `git@good.example` the way GitHub does (exit 1), refuses
/// `git@bad.example` (exit 255), and logs its arguments.
fn ssh_stub(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let log = dir.join("ssh.log");
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{}'\nfor last; do :; done\ncase \"$last\" in\n  git@good.example) echo \"Hi me! You've successfully authenticated, but GitHub does not provide shell access.\" >&2; exit 1 ;;\n  git@bad.example) echo 'git@bad.example: Permission denied (publickey).' >&2; exit 255 ;;\nesac\nexit 255\n",
        log.display()
    );
    let path = bin.join("ssh");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[test]
fn doctor_reports_checks_and_fixes() {
    let config = format!("{CONFIG}\n[contexts.gone]\nmatch.paths = [\"~/missing\"]\n");
    let sandbox = Sandbox::new(&config);
    let home = sandbox.home();
    fake_repo(
        &home.join("perso/remote-good"),
        &[("origin", "git@good.example:me/x.git")],
    );
    fake_repo(
        &home.join("perso/remote-bad"),
        &[
            ("origin", "git@bad.example:me/y.git"),
            ("up", "git@good.example:up/y.git"),
        ],
    );
    let stub = ssh_stub(sandbox.dir.path());
    let path = format!(
        "{}:{}",
        stub.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let agent = sandbox.dir.path().join("agent.sock");
    let _agent = UnixListener::bind(&agent).unwrap();
    let env = [
        ("PATH", path.as_str()),
        ("SSH_AUTH_SOCK", agent.to_str().unwrap()),
    ];

    // Without loomd: the configuration checks only, loomd is not started.
    let output = sandbox.loom_with(&home, &["repo", "doctor"], &env);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("warn  roots: ~/missing (match.paths of gone) does not exist\n"),
        "{text}"
    );
    assert!(
        text.contains("ok    contexts: 3 contexts, no overlapping match.paths\n"),
        "{text}"
    );
    assert!(text.contains("warn  loomd: not running"), "{text}");
    assert!(!sandbox.socket().exists());

    // loomd started from this environment: it sees the agent.
    let output = sandbox.loom_with(&home, &["repo", "scan"], &env);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let output = sandbox.loom_with(&home, &["repo", "doctor"], &env);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("1 check failed"),
        "{}",
        stderr(&output)
    );
    let text = stdout(&output);
    for line in [
        "ok    config: ",
        "ok    loomd: loomd ",
        "warn  unassigned: 1 repository has no context: ",
        "      fix: add a context that matches them to ",
        "             match.paths = [\"~/src\"]\n",
        &format!(
            "ok    ssh-agent: loomd sees an agent socket at {}\n",
            agent.display()
        ),
        &format!("ok    home: loomd expands `~` to {}\n", home.display()),
        "ok    ssh git@good.example: authenticated: Hi me!",
        "fail  ssh git@bad.example: git@bad.example: Permission denied (publickey).\n",
        "      fix: run `ssh -T git@bad.example` in a terminal",
    ] {
        assert!(text.contains(line), "{line:?} not in\n{text}");
    }
    // One check per distinct host, never interactive.
    let log = fs::read_to_string(sandbox.dir.path().join("ssh.log")).unwrap();
    let mut checks: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("BatchMode"))
        .collect();
    checks.sort_unstable();
    assert_eq!(
        checks,
        [
            "-o BatchMode=yes -o ConnectTimeout=5 -T git@bad.example",
            "-o BatchMode=yes -o ConnectTimeout=5 -T git@good.example"
        ],
        "{log}"
    );

    let output = sandbox.loom_with(&home, &["repo", "doctor", "--json"], &env);
    assert_eq!(output.status.code(), Some(1));
    let report: DoctorReport = valid(&stdout(&output));
    assert!(report.failed());
    let status = |name: &str| {
        report
            .checks
            .iter()
            .find(|check| check.check == name)
            .map(|check| check.status)
    };
    assert_eq!(status("ssh git@good.example"), Some(CheckStatus::Ok));
    assert_eq!(status("ssh git@bad.example"), Some(CheckStatus::Fail));
    assert_eq!(status("unassigned"), Some(CheckStatus::Warn));
}
