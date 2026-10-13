//! CLI conventions (ADR-0010) end to end.

use std::io;
use std::process::Stdio;

use assert_cmd::Command;

mod isolation;

fn loom() -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("loom"));
    command.envs(isolation::isolated_env(&isolation::shared_home()));
    command
}

#[test]
fn closed_stdout_is_a_success() {
    // The read end is closed before `loom` starts, so its first write fails
    // with EPIPE whatever the timing.
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin!("loom"))
        .envs(isolation::isolated_env(&isolation::shared_home()))
        .arg("version")
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unknown_flag_is_a_usage_error() {
    loom().args(["--frobnicate", "version"]).assert().code(2);
}

#[test]
fn missing_command_is_a_usage_error() {
    loom().assert().code(2);
}

#[test]
fn quiet_and_verbose_conflict() {
    loom().args(["-q", "-v", "version"]).assert().code(2);
}

#[test]
fn context_flag_rejects_a_path() {
    let assert = loom().args(["-c", "./foo", "version"]).assert().code(2);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("a context name is expected"), "{stderr}");
}

#[test]
fn context_flag_rejects_key_value() {
    let assert = loom().args(["-c", "a=b", "version"]).assert().code(2);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("`key=value`"), "{stderr}");
}

#[test]
fn global_flags_are_accepted_after_the_command() {
    loom()
        .args(["version", "-c", "personal", "-v", "-y", "--color", "never"])
        .assert()
        .success();
}

#[test]
fn help_succeeds() {
    loom().arg("--help").assert().success();
    loom().args(["version", "-h"]).assert().success();
}
