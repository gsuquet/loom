//! Scans of temporary trees built with `git init` and hand-written git files.

#![allow(
    clippy::unwrap_used,
    reason = "fixture helpers outside `#[test]` functions: a failing helper must fail the test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use loom_api::ForgeKind;
use loom_config::config::{Config, parse_config};
use loom_repo::scan::{Exclusions, Head, ScanError, ScanReport};
use loom_repo::{ContextRules, ScanOptions, diff, scan};
use tempfile::TempDir;

/// Runs git in `dir`, isolated from the user's and the system's configuration.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} in {}", dir.display());
}

fn git_init(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
}

/// A minimal git directory written by hand: `HEAD` and `config`.
fn write_git_dir(git_dir: &Path, head: &str, config: &str) {
    fs::create_dir_all(git_dir).unwrap();
    fs::write(git_dir.join("HEAD"), head).unwrap();
    fs::write(git_dir.join("config"), config).unwrap();
}

/// A repository at `dir` with a hand-written `.git` directory.
fn fake_repo(dir: &Path) {
    write_git_dir(&dir.join(".git"), "ref: refs/heads/main\n", "");
}

fn config(text: &str) -> (Config, ContextRules) {
    let config = parse_config(text, Path::new("config.toml")).unwrap();
    let rules = ContextRules::new(&config, Path::new("/home/me")).unwrap();
    (config, rules)
}

fn run(roots: &[PathBuf], config_text: &str, max_depth: u32) -> ScanReport {
    let (config, rules) = config(config_text);
    let options = ScanOptions {
        max_depth,
        exclusions: Exclusions::new(&config.repos.exclude).unwrap(),
        rules: &rules,
        forges: &config.forges,
        resolve_alias: &|_| None,
    };
    scan(roots, &options)
}

/// Reported paths relative to `root`.
fn found(report: &ScanReport, root: &Path) -> Vec<String> {
    report
        .repos
        .iter()
        .map(|repo| {
            repo.identity
                .path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
fn finds_repositories_worktrees_and_gitdir_files() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("root");
    let main = root.join("main");
    git_init(&main);
    git(
        &main,
        &["remote", "add", "origin", "git@github.com:gsuquet/loom.git"],
    );
    git(
        &main,
        &["remote", "add", "fork", "https://gitlab.com/me/loom.git"],
    );
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(&main, &["worktree", "add", "-q", "-b", "feature", "../wt"]);
    // A repository nested in another one is not found through it.
    git_init(&main.join("nested"));
    // A submodule-style checkout: `.git` file pointing to a hidden git directory.
    let id = "0123456789abcdef0123456789abcdef01234567";
    write_git_dir(
        &root.join(".modules/sub"),
        id,
        "[remote \"origin\"]\n\turl = https://example.com/sub.git\n",
    );
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub/.git"), "gitdir: ../.modules/sub\n").unwrap();

    let report = run(
        std::slice::from_ref(&root),
        "[contexts.personal]\nmatch.hosts = [\"github.com/gsuquet\"]\n",
        4,
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(found(&report, &root), ["main", "sub", "wt"]);

    let [main_repo, sub, wt] = &report.repos[..] else {
        panic!("{report:?}")
    };
    // Paths keep the form under the configured root (the temporary directory may be
    // behind a symbolic link, as on macOS).
    assert_eq!(main_repo.identity.path, main);
    assert_eq!(main_repo.identity.slug, "personal/main");
    assert_eq!(main_repo.identity.forge, Some(ForgeKind::Github));
    assert_eq!(main_repo.head, Some(Head::Branch("main".to_owned())));
    assert_eq!(main_repo.git_dir, main.join(".git"));

    // The worktree has its own HEAD and the main repository's remotes, through `commondir`.
    assert_eq!(wt.head, Some(Head::Branch("feature".to_owned())));
    let names = |repo: &loom_repo::scan::ScannedRepo| {
        repo.identity
            .remotes
            .iter()
            .map(|remote| remote.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(wt), ["origin", "fork"]);
    assert_eq!(wt.identity.slug, "personal/wt");
    assert_eq!(
        fs::canonicalize(&wt.common_dir).unwrap(),
        fs::canonicalize(main.join(".git")).unwrap()
    );

    assert_eq!(sub.head, Some(Head::Detached(id.to_owned())));
    assert_eq!(names(sub), ["origin"]);
    assert_eq!(sub.git_dir, root.join("sub/../.modules/sub"));
    assert_eq!(sub.identity.slug, "unassigned/sub");
}

#[test]
fn walk_honours_depth_exclusions_hidden_directories_and_symbolic_links() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("root");
    for repo in [
        "a",
        "x/y/z/at-max-depth",
        "x/y/z/w/too-deep",
        "node_modules/pkg",
        "p/target/built",
        ".hidden/repo",
        "archive/old",
        "keep/archive/old",
        "clients/old/repo",
        "clients/new/repo",
        "my-old/repo",
    ] {
        fake_repo(&root.join(repo));
    }
    // Symbolic links are never followed, even to a repository.
    let outside = tmp.path().join("outside/repo");
    fake_repo(&outside);
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    std::os::unix::fs::symlink(tmp.path().join("outside"), root.join("dir-link")).unwrap();

    let report = run(
        std::slice::from_ref(&root),
        "[repos]\nexclude = [\"archive\", \"clients/old\", \"*-old\"]\n",
        4,
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(
        found(&report, &root),
        ["a", "clients/new/repo", "x/y/z/at-max-depth"]
    );

    // Level 0 is the root itself.
    let report = run(&[root.join("a")], "", 0);
    assert_eq!(found(&report, &root), ["a"]);
    let report = run(std::slice::from_ref(&root), "", 0);
    assert_eq!(found(&report, &root), Vec::<String>::new());
}

#[test]
fn overlapping_roots_report_a_repository_once() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("root");
    fake_repo(&root.join("inner/x"));
    fake_repo(&root.join("y"));
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();

    let report = run(&[root.join("inner"), alias.clone(), root.clone()], "", 4);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let paths: Vec<_> = report
        .repos
        .iter()
        .map(|repo| repo.identity.path.clone())
        .collect();
    // Each under the first root that found it, in the form of that root.
    assert_eq!(paths, [alias.join("y"), root.join("inner/x")]);
}

#[test]
fn errors_are_collected_without_aborting_the_scan() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("root");
    fake_repo(&root.join("good"));
    fs::create_dir_all(root.join("broken")).unwrap();
    fs::write(root.join("broken/.git"), "gitdir: ../missing\n").unwrap();
    fs::create_dir_all(root.join("garbage")).unwrap();
    fs::write(root.join("garbage/.git"), "not a pointer\n").unwrap();
    write_git_dir(
        &root.join("bad-config/.git"),
        "ref: refs/heads/main\n",
        "[remote \"x\"\n",
    );
    write_git_dir(&root.join("bad-head/.git"), "\n", "");
    let file_root = tmp.path().join("file");
    fs::write(&file_root, "").unwrap();

    let report = run(
        &[
            tmp.path().join("missing-root"),
            file_root.clone(),
            root.clone(),
        ],
        "",
        4,
    );
    assert_eq!(found(&report, &root), ["bad-head", "good"]);
    assert_eq!(report.repos[0].head, None);
    let errors: Vec<_> = report
        .errors
        .iter()
        .map(|error| {
            let kind = match error {
                ScanError::Root { .. } => "root",
                ScanError::ReadDir { .. } => "read-dir",
                ScanError::GitDir { .. } => "git-dir",
                ScanError::Config { .. } => "config",
                ScanError::Head { .. } => "head",
            };
            (error.path().to_path_buf(), kind)
        })
        .collect();
    assert_eq!(
        errors,
        [
            (file_root, "root"),
            (root.join("bad-config"), "config"),
            (root.join("bad-head"), "head"),
            (root.join("broken"), "git-dir"),
            (root.join("garbage"), "git-dir"),
        ]
    );
}

#[test]
fn unreadable_directory_is_an_error_and_keeps_its_repositories() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("root");
    fake_repo(&root.join("locked/repo"));
    let first = run(std::slice::from_ref(&root), "", 4);
    let previous: Vec<_> = first
        .repos
        .iter()
        .map(|repo| repo.identity.clone())
        .collect();

    let locked = root.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::read_dir(&locked).is_ok(); // true when running as root
    let report = run(std::slice::from_ref(&root), "", 4);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        return;
    }
    assert_eq!(report.repos, []);
    assert!(
        matches!(&report.errors[..], [ScanError::ReadDir { path, .. }] if *path == locked),
        "{:?}",
        report.errors
    );
    let changes = diff(&previous, std::slice::from_ref(&root), &report);
    assert_eq!(changes.removed, []);
}

#[test]
fn removal_is_reported_only_when_its_root_is_rescanned() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().join("work");
    let perso = tmp.path().join("perso");
    git_init(&work.join("app"));
    git_init(&work.join("lib"));
    git_init(&perso.join("dots"));
    let roots = [work.clone(), perso.clone()];

    let first = run(&roots, "", 4);
    let added = diff(&[], &roots, &first).added;
    assert_eq!(added.len(), 3);

    fs::remove_dir_all(work.join("lib")).unwrap();

    let other_root = run(std::slice::from_ref(&perso), "", 4);
    let changes = diff(&added, std::slice::from_ref(&perso), &other_root);
    assert_eq!(changes.removed, []);
    assert_eq!(changes.added, []);
    assert_eq!(changes.updated, []);

    let own_root = run(std::slice::from_ref(&work), "", 4);
    let changes = diff(&added, std::slice::from_ref(&work), &own_root);
    let removed: Vec<_> = changes.removed.iter().map(|repo| &repo.path).collect();
    assert_eq!(removed, [&work.join("lib")]);
    assert_eq!(changes.added, []);
    assert_eq!(changes.updated, []);
}
