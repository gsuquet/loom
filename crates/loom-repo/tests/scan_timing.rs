//! Scan budget: 1,000 repositories in under 1 s, on a generated tree with the noise of real
//! work directories.
//!
//! Runs with the other tests, in the debug profile: the scan is bound by system calls, so a
//! debug build measures within a few percent of a release one (about 0.3 s on an Apple M-series
//! laptop for both), and no release build is needed in CI.

#![allow(
    clippy::unwrap_used,
    reason = "fixture helpers outside `#[test]` functions: a failing helper must fail the test"
)]

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use loom_config::config::parse_config;
use loom_repo::scan::Exclusions;
use loom_repo::{ContextRules, ScanOptions, scan};
use tempfile::TempDir;

const BUDGET: Duration = Duration::from_secs(1);

/// `root/g<0..10>/t<0..10>/r<0..10>`: 1,000 repositories at depth 3, each with a working tree,
/// a git directory, `node_modules` and `target`; beside them, excluded and hidden directories
/// and non-repository directories deeper than `max_depth`.
fn generate(root: &Path) {
    let config = "[core]\n\trepositoryformatversion = 0\n\tbare = false\n\
                  [remote \"origin\"]\n\turl = git@github.com:gsuquet/repo.git\n\
                  \tfetch = +refs/heads/*:refs/remotes/origin/*\n\
                  [branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n";
    for g in 0..10 {
        let group = root.join(format!("g{g}"));
        for deep in [
            "notes/a/b/c/d/e",
            "docs/x/y/z/w",
            ".cache/a/b/c",
            "node_modules/a/b/c",
        ] {
            fs::create_dir_all(group.join(deep)).unwrap();
        }
        for t in 0..10 {
            let team = group.join(format!("t{t}"));
            fs::create_dir_all(team.join("scratch/one/two/three")).unwrap();
            fs::create_dir_all(team.join("build/out/bin")).unwrap();
            for r in 0..10 {
                let repo = team.join(format!("r{r}"));
                let git_dir = repo.join(".git");
                for dir in ["objects/pack", "refs/heads", "refs/tags", "hooks", "info"] {
                    fs::create_dir_all(git_dir.join(dir)).unwrap();
                }
                fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
                fs::write(git_dir.join("config"), config).unwrap();
                fs::write(git_dir.join("description"), "repo\n").unwrap();
                for dir in [
                    "src/a/b",
                    "tests",
                    "node_modules/x/node_modules/y/lib",
                    "target/debug/deps",
                    "docs",
                ] {
                    fs::create_dir_all(repo.join(dir)).unwrap();
                }
                for file in ["README.md", "Cargo.toml", "package.json", ".gitignore"] {
                    fs::write(repo.join(file), "").unwrap();
                }
            }
        }
    }
}

#[test]
fn scans_1000_repositories_within_budget() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("root");
    generate(&root);

    let config = parse_config(
        "[contexts.personal]\nmatch.hosts = [\"github.com/gsuquet\"]\n",
        Path::new("config.toml"),
    )
    .unwrap();
    let rules = ContextRules::new(&config, Path::new("/home/me")).unwrap();
    let options = ScanOptions {
        max_depth: 4,
        exclusions: Exclusions::new(&config.repos.exclude).unwrap(),
        rules: &rules,
        forges: &config.forges,
        resolve_alias: &|_| None,
    };

    let start = Instant::now();
    let report = scan(std::slice::from_ref(&root), &options);
    let elapsed = start.elapsed();

    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    println!(
        "scanned {} repositories in {} ms ({profile})",
        report.repos.len(),
        elapsed.as_millis()
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.repos.len(), 1000);
    assert!(elapsed < BUDGET, "scan took {elapsed:?}, budget {BUDGET:?}");
}
