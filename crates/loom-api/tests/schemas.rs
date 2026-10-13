//! JSON Schemas of the boundary types, committed under `docs/reference/schemas/`.
//!
//! The test fails when a committed schema differs from the one generated from the types.
//! `LOOM_UPDATE_DOCS=1` rewrites the files instead (`mise run docs`).

use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use loom_api::{
    ApiError, ApiVersion, Channel, ContextAssignment, ContextName, DaemonRestart, ErrorCode,
    ForgeKind, InstallReport, PendingUpgrade, Profile, RegisterRequest, Remote, RemoteUrl,
    RepoIdentity, RepoList, RepoSummary, RollbackReport, ScanIssue, ScanResult, ServicePlatform,
    ServiceStatus, StatusResponse, Timestamp, UpdateChannel, UpdatePolicyMode, UpdateResult,
    UpdateState, UpdateStatus, UpgradeDecision, UpgradeReport, UpgradeSkipReason, UpgradeSource,
    VersionInfo,
};
use schemars::{JsonSchema, Schema, schema_for};
use serde::Serialize;
use serde_json::{Value, json};

/// Every boundary type, with the file name of its schema.
fn schemas() -> Vec<(&'static str, Schema)> {
    vec![
        ("api_error", schema_for!(ApiError)),
        ("api_version", schema_for!(ApiVersion)),
        ("context_assignment", schema_for!(ContextAssignment)),
        ("error_code", schema_for!(ErrorCode)),
        ("register_request", schema_for!(RegisterRequest)),
        ("repo_identity", schema_for!(RepoIdentity)),
        ("repo_list", schema_for!(RepoList)),
        ("repo_summary", schema_for!(RepoSummary)),
        ("rollback_report", schema_for!(RollbackReport)),
        ("scan_result", schema_for!(ScanResult)),
        ("service_platform", schema_for!(ServicePlatform)),
        ("service_status", schema_for!(ServiceStatus)),
        ("status_response", schema_for!(StatusResponse)),
        ("update_state", schema_for!(UpdateState)),
        ("upgrade_report", schema_for!(UpgradeReport)),
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
        update: None,
    }
}

/// Every kind of update result, at `at`.
fn update_results(at: Timestamp) -> Vec<UpdateResult> {
    let s = |text: &str| text.to_owned();
    vec![
        UpdateResult::UpToDate { at },
        UpdateResult::Available {
            at,
            version: s("0.2.0"),
        },
        UpdateResult::SkippedFailed {
            at,
            version: s("0.2.0"),
        },
        UpdateResult::Upgraded {
            at,
            from: s("0.1.0"),
            to: s("0.2.0"),
        },
        UpdateResult::PreflightFailed {
            at,
            version: s("0.2.0"),
            message: s("exited"),
        },
        UpdateResult::RolledBack {
            at,
            from: s("0.1.0"),
            to: s("0.2.0"),
            message: s("no answer"),
        },
        UpdateResult::Interrupted {
            at,
            from: s("0.1.0"),
            to: s("0.2.0"),
        },
        UpdateResult::Failed {
            at,
            message: s("HTTP 500"),
        },
    ]
}

fn update_states(at: Timestamp) -> Vec<UpdateState> {
    let full = UpdateState {
        last_check: Some(at),
        available: Some("0.2.0".to_owned()),
        pending: Some(PendingUpgrade {
            from: "0.1.0".to_owned(),
            to: "0.2.0".to_owned(),
            started_at: at,
        }),
        failed_versions: vec!["0.1.5".to_owned()],
        last_result: None,
    };
    let mut states = vec![UpdateState::default()];
    states.extend(update_results(at).into_iter().map(|result| UpdateState {
        last_result: Some(result),
        ..full.clone()
    }));
    states
}

#[test]
fn samples_validate_against_their_schema() {
    let started_at = Timestamp::from_unix_secs(1_791_792_000).unwrap();
    let mut errors = violations(&status(started_at)).unwrap();
    for state in update_states(started_at) {
        errors.extend(violations(&state).unwrap());
        errors.extend(
            violations(&StatusResponse {
                update: Some(UpdateStatus {
                    policy: UpdatePolicyMode::Auto,
                    channel: Some(UpdateChannel::Dev),
                    check_interval_secs: 21_600,
                    last_check: state.last_check,
                    available: state.available,
                    pending: state.pending,
                    failed_versions: state.failed_versions,
                    last_result: state.last_result,
                }),
                ..status(started_at)
            })
            .unwrap(),
        );
    }
    errors.extend(
        violations(&StatusResponse {
            update: Some(UpdateStatus {
                policy: UpdatePolicyMode::Notify,
                channel: None,
                check_interval_secs: 600,
                last_check: None,
                available: None,
                pending: None,
                failed_versions: vec![],
                last_result: None,
            }),
            ..status(started_at)
        })
        .unwrap(),
    );
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
    for report in upgrade_reports() {
        errors.extend(violations(&report).unwrap());
    }
    errors.extend(
        violations(&RollbackReport {
            current_version: "0.2.0".to_owned(),
            dry_run: true,
            install: install(DaemonRestart::Skipped),
        })
        .unwrap(),
    );
    assert!(errors.is_empty(), "schema violations: {errors:?}");
}

fn install(daemon: DaemonRestart) -> InstallReport {
    InstallReport {
        version: "0.2.0".to_owned(),
        install_dir: "/home/me/.local/bin".into(),
        verified: true,
        installed: true,
        saved_to: Some("/home/me/.loom/versions/0.1.0".into()),
        pruned: vec!["0.0.1".to_owned()],
        daemon,
    }
}

fn upgrade_reports() -> Vec<UpgradeReport> {
    let base = UpgradeReport {
        current_version: "0.1.0".to_owned(),
        source: UpgradeSource::Release {
            channel: Some(Channel::Stable),
            pinned: None,
        },
        decision: UpgradeDecision::UpToDate,
        check: true,
        dry_run: false,
        install: None,
    };
    let skipped = |reason| UpgradeReport {
        decision: UpgradeDecision::Skipped { reason },
        ..base.clone()
    };
    vec![
        skipped(UpgradeSkipReason::UpdatesOff),
        skipped(UpgradeSkipReason::PinnedNotReleased {
            pinned: "9.0.0".to_owned(),
        }),
        skipped(UpgradeSkipReason::PinnedBelowCurrent {
            pinned: "0.0.1".to_owned(),
        }),
        skipped(UpgradeSkipReason::Failed {
            version: "0.2.0".to_owned(),
        }),
        UpgradeReport {
            decision: UpgradeDecision::Available {
                version: "0.2.0".to_owned(),
            },
            ..base.clone()
        },
        UpgradeReport {
            source: UpgradeSource::Release {
                channel: None,
                pinned: Some("0.2.0".to_owned()),
            },
            decision: UpgradeDecision::Apply {
                version: "0.2.0".to_owned(),
            },
            check: false,
            install: Some(install(DaemonRestart::Signalled { pid: 42 })),
            ..base.clone()
        },
        UpgradeReport {
            source: UpgradeSource::Local {
                path: "/tmp/loom.tar.gz".into(),
            },
            decision: UpgradeDecision::Apply {
                version: "0.2.0".to_owned(),
            },
            check: false,
            install: Some(InstallReport {
                verified: false,
                ..install(DaemonRestart::Failed {
                    pid: Some(42),
                    message: "kill: permission denied".to_owned(),
                })
            }),
            ..base.clone()
        },
        UpgradeReport {
            install: Some(install(DaemonRestart::NotRunning)),
            ..base
        },
    ]
}

/// Every kind of context assignment, to `context`.
fn context_assignments(context: &ContextName) -> Vec<ContextAssignment> {
    vec![
        ContextAssignment::Override {
            context: context.clone(),
        },
        ContextAssignment::Host {
            context: context.clone(),
            pattern: "gitlab.com/company-a".to_owned(),
        },
        ContextAssignment::Path {
            context: context.clone(),
            prefix: "~/perso".to_owned(),
        },
        ContextAssignment::Default {
            context: context.clone(),
        },
        ContextAssignment::Unassigned,
    ]
}

fn repo_identity(context: ContextAssignment) -> RepoIdentity {
    let remote = |name: &str, url: &str, location: Option<RemoteUrl>| Remote {
        name: name.to_owned(),
        url: url.to_owned(),
        location,
    };
    RepoIdentity {
        path: "/home/me/perso/loom".into(),
        name: "loom".to_owned(),
        slug: format!("{}/loom", context.context_name()),
        context,
        remotes: vec![
            remote(
                "origin",
                "https://github.com/gsuquet/loom.git",
                Some(RemoteUrl {
                    host: "github.com".to_owned(),
                    port: None,
                    path: "gsuquet/loom".to_owned(),
                }),
            ),
            remote(
                "mirror",
                "ssh://mirror.example:2222/loom",
                Some(RemoteUrl {
                    host: "mirror.example".to_owned(),
                    port: Some(2222),
                    path: "loom".to_owned(),
                }),
            ),
            remote("backup", "/srv/backup/loom", None),
        ],
        primary_remote: Some("origin".to_owned()),
        forge: Some(ForgeKind::Github),
    }
}

#[test]
fn repo_samples_validate_against_their_schema() {
    let mut errors = Vec::new();
    for assignment in context_assignments(&"company-a".parse().unwrap()) {
        errors.extend(violations(&assignment).unwrap());
        errors.extend(violations(&repo_identity(assignment)).unwrap());
    }
    errors.extend(
        violations(&RepoIdentity {
            remotes: vec![],
            primary_remote: None,
            forge: None,
            ..repo_identity(ContextAssignment::Unassigned)
        })
        .unwrap(),
    );
    errors.extend(
        violations(&RepoIdentity {
            forge: Some(ForgeKind::Gitlab),
            ..repo_identity(ContextAssignment::Unassigned)
        })
        .unwrap(),
    );
    let used = Timestamp::from_unix_secs(1_791_792_000).unwrap();
    let summaries = vec![
        RepoSummary {
            identity: repo_identity(ContextAssignment::Unassigned),
            registered: false,
            last_used: None,
        },
        RepoSummary {
            identity: repo_identity(ContextAssignment::Default {
                context: "personal".parse().unwrap(),
            }),
            registered: true,
            last_used: Some(used),
        },
    ];
    for summary in &summaries {
        errors.extend(violations(summary).unwrap());
    }
    errors.extend(violations(&RepoList { repos: summaries }).unwrap());
    errors.extend(violations(&RepoList { repos: vec![] }).unwrap());
    errors.extend(
        violations(&RegisterRequest {
            path: "/home/me/src/x".into(),
        })
        .unwrap(),
    );
    errors.extend(
        violations(&ScanResult {
            added: 2,
            updated: 1,
            removed: 0,
            errors: vec![ScanIssue {
                path: "/home/me/work/locked".into(),
                message: "cannot read directory /home/me/work/locked: permission denied".to_owned(),
            }],
            duration_ms: 42,
        })
        .unwrap(),
    );
    assert!(errors.is_empty(), "schema violations: {errors:?}");
}

#[test]
fn repo_summary_inlines_the_identity() {
    let summary = RepoSummary {
        identity: repo_identity(ContextAssignment::Unassigned),
        registered: true,
        last_used: None,
    };
    let value = serde_json::to_value(&summary).unwrap();
    assert_eq!(value["slug"], "unassigned/loom");
    assert_eq!(value["registered"], true);
    assert!(value.get("last_used").is_none());
    // A client reading only the identity reads a summary.
    let identity: RepoIdentity = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(identity, summary.identity);
    let back: RepoSummary = serde_json::from_value(value).unwrap();
    assert_eq!(back, summary);
}

#[test]
fn register_request_rejects_unknown_fields() {
    let error = serde_json::from_value::<RegisterRequest>(json!({ "path": "/x", "force": true }));
    assert!(error.is_err());
    let schema = serde_json::to_value(schema_for!(RegisterRequest)).unwrap();
    assert!(!jsonschema::is_valid(
        &schema,
        &json!({ "path": "/x", "force": true })
    ));
}

#[test]
fn schema_rejects_an_invalid_context_name() {
    let schema = serde_json::to_value(schema_for!(ContextAssignment)).unwrap();
    let valid = json!({ "rule": "default", "context": "personal" });
    assert!(jsonschema::is_valid(&schema, &valid));
    for name in ["Personal", "2fa", "a-", "", "a_b"] {
        let instance = json!({ "rule": "default", "context": name });
        assert!(!jsonschema::is_valid(&schema, &instance), "{name}");
    }
}

#[test]
fn schema_rejects_a_timestamp_with_an_offset() {
    let schema = serde_json::to_value(schema_for!(StatusResponse)).unwrap();
    let started_at = Timestamp::from_unix_secs(1_791_792_000).unwrap();
    let mut instance = serde_json::to_value(status(started_at)).unwrap();
    instance["started_at"] = Value::from("2026-10-12T10:00:00+02:00");
    assert!(!jsonschema::is_valid(&schema, &instance));
}
