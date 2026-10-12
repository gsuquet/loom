//! Captures the build information exposed by `loom_api::VersionInfo::current`.
//!
//! Both binaries read it through `loom-api`, so a workspace build stamps them identically.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Value used when a git fact cannot be read (no git, not a checkout).
const UNKNOWN: &str = "unknown";

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=LOOM_BUILD_VERSION");
    println!("cargo::rerun-if-env-changed=LOOM_CHANNEL");

    let manifest_dir = PathBuf::from(required_env("CARGO_MANIFEST_DIR"));
    let version = env::var("LOOM_BUILD_VERSION")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| required_env("CARGO_PKG_VERSION"));
    let channel = channel_variant(env::var("LOOM_CHANNEL").ok().as_deref());
    let profile = profile_variant(&required_env("PROFILE"));
    let target = required_env("TARGET");
    let commit = git(&manifest_dir, &["rev-parse", "HEAD"]).unwrap_or_else(|| UNKNOWN.to_owned());
    let commit_date = git(&manifest_dir, &["show", "-s", "--format=%cI", "HEAD"])
        .unwrap_or_else(|| UNKNOWN.to_owned());

    watch_git_head(&manifest_dir);

    let mut generated = String::new();
    // `{:?}` produces a valid, escaped Rust string literal.
    let _ = writeln!(generated, "const BUILD_VERSION: &str = {version:?};");
    let _ = writeln!(generated, "const BUILD_COMMIT: &str = {commit:?};");
    let _ = writeln!(
        generated,
        "const BUILD_COMMIT_DATE: &str = {commit_date:?};"
    );
    let _ = writeln!(generated, "const BUILD_TARGET: &str = {target:?};");
    let _ = writeln!(
        generated,
        "const BUILD_CHANNEL: Channel = Channel::{channel};"
    );
    let _ = writeln!(
        generated,
        "const BUILD_PROFILE: Profile = Profile::{profile};"
    );

    let out = PathBuf::from(required_env("OUT_DIR")).join("build_info.rs");
    if let Err(error) = fs::write(&out, generated) {
        panic!("cannot write {}: {error}", out.display());
    }
}

/// Reads a variable Cargo always sets for build scripts.
fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("cargo did not set {name}"))
}

/// Maps `LOOM_CHANNEL` to a `Channel` variant. Unset or empty means a local build.
fn channel_variant(value: Option<&str>) -> &'static str {
    match value.map(str::trim) {
        None | Some("" | "local") => "Local",
        Some("dev") => "Dev",
        Some("stable") => "Stable",
        Some(other) => {
            panic!("LOOM_CHANNEL must be `stable`, `dev` or `local` (or unset), got `{other}`")
        }
    }
}

/// Maps Cargo's `PROFILE` (`debug` or `release`) to a `Profile` variant.
fn profile_variant(value: &str) -> &'static str {
    match value {
        "debug" => "Debug",
        "release" => "Release",
        other => panic!("unexpected cargo PROFILE `{other}`"),
    }
}

/// Runs `git` in `dir` and returns its trimmed stdout, or `None` on any failure.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Reruns this script when HEAD moves: HEAD itself, the branch it points to, packed refs.
fn watch_git_head(dir: &Path) {
    let mut paths = vec!["HEAD".to_owned(), "packed-refs".to_owned()];
    if let Some(reference) = git(dir, &["symbolic-ref", "-q", "HEAD"]) {
        paths.push(reference);
    }
    for path in paths {
        let Some(resolved) = git(dir, &["rev-parse", "--git-path", &path]) else {
            continue;
        };
        let resolved = dir.join(resolved);
        // A missing path would make Cargo rerun the script on every build.
        if resolved.exists() {
            println!("cargo::rerun-if-changed={}", resolved.display());
        }
    }
}
