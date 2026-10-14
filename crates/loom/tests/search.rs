//! `loom search` end to end: fixture repositories under a temporary `HOME`, `loom` started as
//! a process against the `loomd` it starts. Includes the latency budget on 1,000 repositories.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fmt::Write as _;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{RepoList, SearchField, SearchResult, SearchResults};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tempfile::TempDir;

mod isolation;

const TIMEOUT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(50);

/// Repositories of the latency test.
const REPOS: usize = 1_000;
/// Timed runs of `loom search` per scenario.
const RUNS: usize = 60;
/// p95 above which the latency test fails. The target is 100 ms; a shared CI runner running a
/// debug build gets slack.
const P95_LIMIT: Duration = Duration::from_millis(250);

const CONFIG: &str = r#"
[repos]
roots = ["~/perso"]

[contexts.personal]
match.paths = ["~/perso"]

[contexts.work]
match.hosts = ["gitlab.com/company"]
"#;

fn loom_bin() -> PathBuf {
    PathBuf::from(assert_cmd::cargo::cargo_bin!("loom"))
}

/// A loom home and a user home under `/tmp` (the macOS `$TMPDIR` is too long for a socket
/// path).
struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let loomd = loom_bin().with_file_name("loomd");
        assert!(loomd.is_file(), "{} is missing", loomd.display());
        // The path is searched: the prefix is glued to the random characters, so that they never
        // form a word of their own that a query term starts (`ap` for `LO ap`).
        let dir = tempfile::Builder::new()
            .prefix("sx")
            .tempdir_in("/tmp")
            .unwrap();
        let sandbox = Self { dir };
        fs::create_dir_all(sandbox.loom_home()).unwrap();
        fs::create_dir_all(sandbox.home()).unwrap();
        fs::write(sandbox.loom_home().join("config.toml"), CONFIG).unwrap();
        sandbox
    }

    fn loom_home(&self) -> PathBuf {
        self.dir.path().join("l")
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("u")
    }

    /// `loom --color=never <args>` run in `cwd`, as a shell would (`PWD` set).
    fn loom_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(loom_bin())
            .env("LOOM_HOME", self.loom_home())
            .envs(isolation::isolated_env(&self.home()))
            .env("PWD", cwd)
            .env_remove("NO_COLOR")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
            .current_dir(cwd)
            .arg("--color=never")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// Runs `args` in the user home, expecting success; stdout.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.loom_in(&self.home(), args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn search(&self, args: &[&str]) -> SearchResults {
        let mut full = vec!["search", "--json"];
        full.extend_from_slice(args);
        valid(&self.ok(&full))
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

/// `text` parsed as `T`, after checking it against the schema of `T`.
fn valid<T: DeserializeOwned + schemars::JsonSchema>(text: &str) -> T {
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

/// The slug of each repository, `task:<id>` for each task, in order.
fn slugs(results: &SearchResults) -> Vec<String> {
    results
        .results
        .iter()
        .map(|result| match result {
            SearchResult::Repo(repo) => repo.slug.clone(),
            SearchResult::Task(task) => format!("task:{}", task.id),
        })
        .collect()
}

#[test]
fn search_prints_ranked_results_as_a_table_or_json() {
    let sandbox = Sandbox::new();
    let perso = sandbox.home().join("perso");
    fake_repo(&perso.join("loom"), &[]);
    fake_repo(&perso.join("loom-api"), &[]);
    fake_repo(&perso.join("Café"), &[]);
    fake_repo(
        &perso.join("clients/tool"),
        &[("origin", "https://gitlab.com/company/loom-tool.git")],
    );

    // Starts loomd, which scans before answering.
    let found = sandbox.search(&["loom"]);
    assert_eq!(
        slugs(&found),
        ["personal/loom", "personal/loom-api", "work/tool"]
    );
    let SearchResult::Repo(first) = &found.results[0] else {
        panic!("{found:?}");
    };
    assert_eq!(first.name, "loom");
    assert_eq!(first.context, "personal");
    assert_eq!(first.path, perso.join("loom"));
    assert_eq!(
        first.matched_fields,
        [SearchField::Name, SearchField::Slug, SearchField::Path]
    );

    // Several words, each a prefix; quoted or not.
    assert_eq!(slugs(&sandbox.search(&["LO", "ap"])), ["personal/loom-api"]);
    assert_eq!(slugs(&sandbox.search(&["lo ap"])), ["personal/loom-api"]);
    assert_eq!(slugs(&sandbox.search(&["cafe"])), ["personal/Café"]);
    assert_eq!(
        slugs(&sandbox.search(&["loom", "--limit", "1"])),
        ["personal/loom"]
    );
    assert_eq!(
        slugs(&sandbox.search(&["-c", "work", "loom"])),
        ["work/tool"]
    );
    assert_eq!(
        slugs(&sandbox.search(&["\"loom\"", "OR"])),
        Vec::<String>::new()
    );

    let table = sandbox.ok(&["search", "loom"]);
    let perso = perso.display();
    assert_eq!(
        table,
        format!(
            "TYPE  ITEM               CONTEXT   STATUS  MATCHED         DETAIL\n\
             repo  personal/loom      personal          name,slug,path  {perso}/loom\n\
             repo  personal/loom-api  personal          name,slug,path  {perso}/loom-api\n\
             repo  work/tool          work              remotes         {perso}/clients/tool\n"
        )
    );
    assert_eq!(sandbox.ok(&["search", "nothing-like-this"]), "");

    let output = sandbox.loom_in(&sandbox.home(), &["search", "x", "--limit", "0"]);
    assert_eq!(output.status.code(), Some(2));
}

/// Durations at the 50th and 95th percentiles.
fn percentiles(mut durations: Vec<Duration>) -> (Duration, Duration) {
    durations.sort();
    let at = |percent: usize| durations[(durations.len() * percent).div_ceil(100) - 1];
    (at(50), at(95))
}

/// Times `RUNS` runs of `loom` in `cwd`, cycling through `commands`; each run must succeed.
fn time_runs(sandbox: &Sandbox, cwd: &Path, commands: &[&[&str]]) -> (Duration, Duration) {
    let durations = (0..RUNS)
        .map(|run| {
            let args = commands[run % commands.len()];
            let started = Instant::now();
            let output = sandbox.loom_in(cwd, args);
            let elapsed = started.elapsed();
            assert_eq!(
                output.status.code(),
                Some(0),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            elapsed
        })
        .collect();
    percentiles(durations)
}

#[test]
fn search_answers_within_budget_on_1000_repositories() {
    const WORDS: [&str; 8] = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
    ];
    let sandbox = Sandbox::new();
    let perso = sandbox.home().join("perso");
    let started = Instant::now();
    for index in 0..REPOS {
        let word = WORDS[index % WORDS.len()];
        let url = format!("https://github.com/me/{word}-service-{index}.git");
        fake_repo(
            &perso.join(format!("group-{}/{word}-{index:04}", index / 100)),
            &[("origin", url.as_str())],
        );
    }
    eprintln!("fixtures: {REPOS} repositories in {:?}", started.elapsed());

    // Starts loomd, waits for its scan, then for the states of every repository, so that the
    // timed runs do not compete with the state threads.
    let started = Instant::now();
    assert_eq!(sandbox.search(&["zzz"]).results.len(), 0);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let list: RepoList = serde_json::from_str(&sandbox.ok(&["repo", "ls", "--json"])).unwrap();
        let read = list
            .repos
            .iter()
            .filter(|repo| repo.state.is_some())
            .count();
        if list.repos.len() == REPOS && read == REPOS {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{} repositories, {read} states read",
            list.repos.len()
        );
        thread::sleep(POLL);
    }
    eprintln!("index ready in {:?}", started.elapsed());

    // `alpha` matches 125 repositories, `service` all of them, `echo 0420` one.
    let searches: [&[&str]; 5] = [
        &["search", "alpha"],
        &["search", "service"],
        &["search", "echo", "0420"],
        &["search", "github me"],
        &["search", "zzz"],
    ];
    assert_eq!(sandbox.search(&["service"]).results.len(), 20);
    assert_eq!(
        slugs(&sandbox.search(&["echo", "0420"])),
        ["personal/echo-0420"]
    );

    // Outside a repository, then inside one (registration on use runs). `loom version`, which
    // starts like every command but sends no request of its own, is the baseline.
    let inside = perso.join("group-4/alpha-0400");
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    for (scenario, cwd) in [
        ("outside a repository", sandbox.home()),
        ("inside a repository", inside),
    ] {
        let (base_p50, base_p95) = time_runs(&sandbox, &cwd, &[&["version"]]);
        let (p50, p95) = time_runs(&sandbox, &cwd, &searches);
        eprintln!(
            "loom search ({profile}, {REPOS} repositories, {RUNS} runs, {scenario}): \
             p50 {:.1} ms, p95 {:.1} ms; loom version: p50 {:.1} ms, p95 {:.1} ms",
            ms(p50),
            ms(p95),
            ms(base_p50),
            ms(base_p95),
        );
        assert!(
            p95 < P95_LIMIT,
            "p95 {p95:?} over {P95_LIMIT:?} ({scenario})"
        );
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}
