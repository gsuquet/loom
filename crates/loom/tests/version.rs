//! `loom version` end to end.

use assert_cmd::Command;
use loom_api::VersionInfo;
use serde_json::Value;

mod isolation;

fn loom() -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("loom"));
    command.envs(isolation::isolated_env(&isolation::shared_home()));
    command
}

#[test]
fn json_output_validates_against_the_schema() {
    let output = loom().args(["version", "--json"]).assert().success();
    let instance: Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    let schema = serde_json::to_value(schemars::schema_for!(VersionInfo)).unwrap();

    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "schema violations: {errors:?}");

    for field in [
        "version",
        "commit",
        "commit_date",
        "target",
        "channel",
        "profile",
    ] {
        assert!(instance.get(field).is_some(), "missing field {field}");
    }
}

#[test]
fn json_output_matches_the_embedded_build_info() {
    let output = loom().args(["version", "--json"]).assert().success();
    let reported: VersionInfo = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(reported, VersionInfo::current());
}

#[test]
fn human_output_lists_every_field() {
    let expected = format!("loom {}\n", VersionInfo::current());
    loom().arg("version").assert().success().stdout(expected);
}

#[test]
fn long_version_flag_matches_version_command() {
    let expected = format!("loom {}\n", VersionInfo::current());
    loom().arg("--version").assert().success().stdout(expected);
}

#[test]
fn unknown_flag_fails() {
    loom()
        .args(["version", "--yaml"])
        .assert()
        .failure()
        .code(2);
}
