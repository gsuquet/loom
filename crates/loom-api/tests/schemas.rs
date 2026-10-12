//! JSON Schemas of the boundary types, committed under `docs/reference/schemas/`.
//!
//! The test fails when a committed schema differs from the one generated from the types.
//! `LOOM_UPDATE_DOCS=1` rewrites the files instead (`mise run docs`).

use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use loom_api::{
    ApiError, ApiVersion, Channel, ErrorCode, Profile, ServicePlatform, ServiceStatus,
    StatusResponse, Timestamp, VersionInfo,
};
use schemars::{JsonSchema, Schema, schema_for};
use serde::Serialize;
use serde_json::{Value, json};

/// Every boundary type, with the file name of its schema.
fn schemas() -> Vec<(&'static str, Schema)> {
    vec![
        ("api_error", schema_for!(ApiError)),
        ("api_version", schema_for!(ApiVersion)),
        ("error_code", schema_for!(ErrorCode)),
        ("service_platform", schema_for!(ServicePlatform)),
        ("service_status", schema_for!(ServiceStatus)),
        ("status_response", schema_for!(StatusResponse)),
        ("version_info", schema_for!(VersionInfo)),
    ]
}

fn schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference/schemas")
}

/// Pretty JSON with a trailing newline.
fn render(schema: &Schema) -> serde_json::Result<String> {
    serde_json::to_string_pretty(schema).map(|text| text + "\n")
}

#[test]
fn committed_schemas_match_the_types() {
    let dir = schemas_dir();
    let update = env::var_os("LOOM_UPDATE_DOCS").is_some_and(|value| value == "1");
    let mut stale = Vec::new();

    for (name, schema) in schemas() {
        let path = dir.join(format!("{name}.json"));
        let expected = render(&schema).unwrap();
        if update {
            fs::create_dir_all(&dir).unwrap();
            fs::write(&path, expected).unwrap();
        } else if fs::read_to_string(&path).ok().as_deref() != Some(expected.as_str()) {
            stale.push(path.display().to_string());
        }
    }

    assert!(
        stale.is_empty(),
        "stale or missing schemas, run `mise run docs`: {stale:?}"
    );
}

/// Schema violations of `sample` serialized as JSON, against the schema of `T`.
fn violations<T: Serialize + JsonSchema>(sample: &T) -> Result<Vec<String>, Box<dyn Error>> {
    let schema = serde_json::to_value(schema_for!(T))?;
    let instance = serde_json::to_value(sample)?;
    let validator = jsonschema::validator_for(&schema)?;
    Ok(validator
        .iter_errors(&instance)
        .map(|error| format!("{instance}: {error}"))
        .collect())
}

fn status(started_at: Timestamp) -> StatusResponse {
    StatusResponse {
        api_version: ApiVersion::V1,
        daemon: VersionInfo {
            version: "0.1.0".to_owned(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            commit_date: "2026-10-12T10:00:00+02:00".to_owned(),
            target: "x86_64-unknown-linux-gnu".to_owned(),
            channel: Channel::Stable,
            profile: Profile::Release,
        },
        pid: 1,
        started_at,
        uptime_secs: 0,
        loom_home: "/var/lib/loom".into(),
        socket_path: "/run/loom/loomd.sock".into(),
        config_path: "/etc/loom/config.toml".into(),
    }
}

#[test]
fn samples_validate_against_their_schema() {
    let started_at = Timestamp::from_unix_secs(1_791_792_000).unwrap();
    let mut errors = violations(&status(started_at)).unwrap();
    errors.extend(violations(&ApiVersion::V1).unwrap());
    errors.extend(
        violations(&ServiceStatus {
            platform: ServicePlatform::Linux,
            unit_path: "/home/me/.config/systemd/user/loomd.service".into(),
            installed: true,
            loaded: true,
            running: true,
        })
        .unwrap(),
    );
    errors.extend(violations(&ErrorCode::VersionMismatch).unwrap());
    errors.extend(violations(&ApiError::new(ErrorCode::Unavailable, "shutting down")).unwrap());
    errors.extend(
        violations(&ApiError {
            details: Some(json!({ "supported": ["v1"] })),
            ..ApiError::new(ErrorCode::VersionMismatch, "unsupported API version")
        })
        .unwrap(),
    );
    assert!(errors.is_empty(), "schema violations: {errors:?}");
}

#[test]
fn schema_rejects_a_timestamp_with_an_offset() {
    let schema = serde_json::to_value(schema_for!(StatusResponse)).unwrap();
    let started_at = Timestamp::from_unix_secs(1_791_792_000).unwrap();
    let mut instance = serde_json::to_value(status(started_at)).unwrap();
    instance["started_at"] = Value::from("2026-10-12T10:00:00+02:00");
    assert!(!jsonschema::is_valid(&schema, &instance));
}
