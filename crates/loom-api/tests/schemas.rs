//! JSON Schemas of the boundary types, committed under `docs/reference/schemas/`.
//!
//! The test fails when a committed schema differs from the one generated from the types.
//! `LOOM_UPDATE_DOCS=1` rewrites the files instead (`mise run docs`).

use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use loom_api::{
    Action, Actor, AmbiguousPrefix, Change, EventId, EventRecord, FieldChange, IllegalTransition,
    Kind, MachineId, NewTask, RankReason, Relations, Status, StatusName, Task, TaskActionName,
    TaskActionRequest, TaskCandidate, TaskDetail, TaskEdit, TaskEvents, TaskId, TaskList, TaskRank,
    TaskReadOnly, TaskSummary, UndoConflict,
};
use loom_api::{
    ApiError, ApiVersion, BackupOutcome, BackupReport, BackupRequest, BackupStep, BackupSync,
    Channel, ChannelSource, CheckStatus, ChosenChannel, ContextAssignment, ContextBackup,
    ContextDiskUsage, ContextName, DaemonDiagnostics, DaemonRestart, DiskUsage, DoctorCheck,
    DoctorReport, ErrorCode, ForgeKind, InstallReport, LastUpdate, MaybeKnown, PendingUpgrade,
    PlannedAction, Profile, RegisterRequest, ReindexResult, Remote, RemoteCheck,
    RemoteCheckOutcome, RemoteCheckResult, RemoteProblem, RemoteScheme, RemoteUrl, RepoHead,
    RepoIdentity, RepoList, RepoOperation, RepoState, RepoSummary, RepoSync, RepoUpdatePolicy,
    RollbackReport, ScanIssue, ScanResult, SearchField, SearchKind, SearchResult, SearchResults,
    ServicePlatform, ServiceStatus, SkipReason, SshAgentSocket, StateField, StatusResponse,
    SyncOutcome, SyncRequest, SyncResult, Timestamp, Untracked, UpdateChannel, UpdateOutcome,
    UpdatePolicyMode, UpdateResult, UpdateState, UpdateStatus, UpdateStep, UpgradeDecision,
    UpgradeReport, UpgradeSkipReason, UpgradeSource, Upstream, VersionInfo,
};
use schemars::{JsonSchema, Schema, schema_for};
use serde::Serialize;
use serde_json::{Value, json};

/// Every boundary type, with the file name of its schema.
fn schemas() -> Vec<(&'static str, Schema)> {
    vec![
        ("api_error", schema_for!(ApiError)),
        ("ambiguous_prefix", schema_for!(AmbiguousPrefix)),
        ("api_version", schema_for!(ApiVersion)),
        ("backup_report", schema_for!(BackupReport)),
        ("backup_request", schema_for!(BackupRequest)),
        ("context_assignment", schema_for!(ContextAssignment)),
        ("daemon_diagnostics", schema_for!(DaemonDiagnostics)),
        ("doctor_report", schema_for!(DoctorReport)),
        ("error_code", schema_for!(ErrorCode)),
        ("illegal_transition", schema_for!(IllegalTransition)),
        ("last_update", schema_for!(LastUpdate)),
        ("new_task", schema_for!(NewTask)),
        ("register_request", schema_for!(RegisterRequest)),
        ("reindex_result", schema_for!(ReindexResult)),
        ("remote_check_result", schema_for!(RemoteCheckResult)),
        ("repo_identity", schema_for!(RepoIdentity)),
        ("repo_list", schema_for!(RepoList)),
        ("repo_state", schema_for!(RepoState)),
        ("repo_summary", schema_for!(RepoSummary)),
        ("rollback_report", schema_for!(RollbackReport)),
        ("scan_result", schema_for!(ScanResult)),
        ("search_results", schema_for!(SearchResults)),
        ("service_platform", schema_for!(ServicePlatform)),
        ("service_status", schema_for!(ServiceStatus)),
        ("status_response", schema_for!(StatusResponse)),
        ("sync_request", schema_for!(SyncRequest)),
        ("sync_result", schema_for!(SyncResult)),
        ("task_action_name", schema_for!(TaskActionName)),
        ("task_action_request", schema_for!(TaskActionRequest)),
        ("task_detail", schema_for!(TaskDetail)),
        ("task_edit", schema_for!(TaskEdit)),
        ("task_events", schema_for!(TaskEvents)),
        ("task_list", schema_for!(TaskList)),
        ("task_read_only", schema_for!(TaskReadOnly)),
        ("undo_conflict", schema_for!(UndoConflict)),
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
        disk: None,
    }
}

/// A disk usage with a context store and a warning.
fn disk_usage(computed_at: Timestamp, available_bytes: Option<u64>) -> DiskUsage {
    DiskUsage {
        computed_at,
        total_bytes: 35_000_000,
        versions_bytes: 30_000_000,
        index_bytes: 4_200_000,
        index_wal_bytes: 4_096,
        logs_bytes: 24_576,
        state_bytes: 8_192,
        other_bytes: 755_136,
        contexts: vec![ContextDiskUsage {
            name: "personal".to_owned(),
            bytes: 12_096,
        }],
        kept_versions: vec!["0.1.0".to_owned(), "0.2.0".to_owned()],
        available_bytes,
        warnings: vec!["cannot read /l/log: permission denied".to_owned()],
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
        channel: Some(ChosenChannel {
            name: UpdateChannel::Dev,
            chosen_at: at,
        }),
        last_check: Some(at),
        available: Some("0.2.0".to_owned()),
        pending: Some(PendingUpgrade {
            from: "0.1.0".to_owned(),
            to: "0.2.0".to_owned(),
            started_at: at,
        }),
        failed_versions: vec!["0.1.5".to_owned()],
        last_result: None,
        other: serde_json::Map::from_iter([("later".to_owned(), json!(1))]),
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
                    channel_source: Some(ChannelSource::Chosen),
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
                channel_source: None,
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
    for available_bytes in [Some(1_000_000_000), None] {
        errors.extend(
            violations(&StatusResponse {
                disk: Some(disk_usage(started_at, available_bytes)),
                ..status(started_at)
            })
            .unwrap(),
        );
    }
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
        violations(&SearchResults {
            results: vec![SearchResult {
                kind: SearchKind::Repo,
                slug: "personal/loom".to_owned(),
                name: "loom".to_owned(),
                context: "personal".to_owned(),
                path: "/home/me/perso/loom".into(),
                score: 1.25,
                matched_fields: vec![SearchField::Name, SearchField::Slug, SearchField::Path],
            }],
        })
        .unwrap(),
    );
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

/// A state with every field known, one with known `null`s, one with every field unknown.
fn repo_states(at: Timestamp) -> Vec<RepoState> {
    vec![
        RepoState {
            refreshed_at: at,
            head: MaybeKnown::Known(RepoHead::Branch {
                name: "main".to_owned(),
            }),
            dirty: MaybeKnown::Known(2),
            untracked: MaybeKnown::Known(Untracked {
                count: 1000,
                capped: true,
            }),
            operation: MaybeKnown::Known(Some(RepoOperation::CherryPick)),
            upstream: MaybeKnown::Known(Some(Upstream {
                name: "origin/main".to_owned(),
                ahead: MaybeKnown::Known(1),
                behind: MaybeKnown::Unknown,
            })),
            default_branch: MaybeKnown::Known(Some("main".to_owned())),
            last_commit: MaybeKnown::Known(Some(at)),
            last_fetch: MaybeKnown::Known(Some(at)),
            error: Some("ahead/behind: time budget of 2s exceeded".to_owned()),
        },
        RepoState {
            refreshed_at: at,
            head: MaybeKnown::Known(RepoHead::Detached {
                commit: "abc1234".to_owned(),
            }),
            dirty: MaybeKnown::Known(0),
            untracked: MaybeKnown::Known(Untracked {
                count: 0,
                capped: false,
            }),
            operation: MaybeKnown::Known(None),
            upstream: MaybeKnown::Known(None),
            default_branch: MaybeKnown::Known(None),
            last_commit: MaybeKnown::Known(None),
            last_fetch: MaybeKnown::Known(None),
            error: None,
        },
        RepoState {
            refreshed_at: at,
            head: MaybeKnown::Unknown,
            dirty: MaybeKnown::Unknown,
            untracked: MaybeKnown::Unknown,
            operation: MaybeKnown::Unknown,
            upstream: MaybeKnown::Unknown,
            default_branch: MaybeKnown::Unknown,
            last_commit: MaybeKnown::Unknown,
            last_fetch: MaybeKnown::Unknown,
            error: Some("cannot open the repository".to_owned()),
        },
    ]
}

#[test]
fn repo_state_unknown_fields_are_rejected_unless_tagged() {
    let schema = serde_json::to_value(schema_for!(RepoState)).unwrap();
    let mut value = serde_json::to_value(&repo_states(Timestamp::EPOCH)[1]).unwrap();
    assert!(jsonschema::is_valid(&schema, &value));
    assert_eq!(value["upstream"], json!({ "known": null }));
    value["dirty"] = json!(3);
    assert!(!jsonschema::is_valid(&schema, &value), "{value}");
    value["dirty"] = json!("unknown");
    assert!(jsonschema::is_valid(&schema, &value), "{value}");
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
            state: None,
            last_update: None,
        },
        RepoSummary {
            identity: repo_identity(ContextAssignment::Default {
                context: "personal".parse().unwrap(),
            }),
            registered: true,
            last_used: Some(used),
            state: Some(repo_states(used).remove(0)),
            last_update: Some(last_updates(used).remove(0)),
        },
    ];
    for state in repo_states(used) {
        errors.extend(violations(&state).unwrap());
    }
    for summary in &summaries {
        errors.extend(violations(summary).unwrap());
    }
    errors.extend(violations(&RepoList { repos: summaries }).unwrap());
    errors.extend(violations(&RepoList { repos: vec![] }).unwrap());
    errors.extend(
        violations(&RegisterRequest {
            path: "/home/me/src/x".into(),
            on_use: false,
        })
        .unwrap(),
    );
    errors.extend(
        violations(&RegisterRequest {
            path: "/home/me/src/x".into(),
            on_use: true,
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

/// Every kind of update outcome and skip reason, at `at`.
fn last_updates(at: Timestamp) -> Vec<LastUpdate> {
    let s = |text: &str| text.to_owned();
    let id = |c: char| c.to_string().repeat(40);
    let skipped = |reason| UpdateOutcome::Skipped { reason };
    vec![
        LastUpdate {
            time: at,
            policy: RepoUpdatePolicy::Ff,
            outcomes: vec![
                UpdateOutcome::FastForwarded {
                    branch: s("feat/x"),
                    from: id('a'),
                    to: id('b'),
                },
                UpdateOutcome::DefaultBranchMoved {
                    branch: s("main"),
                    from: id('c'),
                    to: id('d'),
                },
            ],
        },
        LastUpdate {
            time: at,
            policy: RepoUpdatePolicy::Fetch,
            outcomes: vec![UpdateOutcome::Fetched],
        },
        LastUpdate {
            time: at,
            policy: RepoUpdatePolicy::Off,
            outcomes: vec![skipped(SkipReason::PolicyOff)],
        },
        LastUpdate {
            time: at,
            policy: RepoUpdatePolicy::Ff,
            outcomes: vec![
                skipped(SkipReason::NoRemote),
                skipped(SkipReason::Unreadable {
                    error: s("no HEAD"),
                }),
                skipped(SkipReason::Unknown {
                    field: StateField::Dirty,
                }),
                skipped(SkipReason::Operation {
                    operation: RepoOperation::Am,
                }),
                skipped(SkipReason::Dirty { paths: 3 }),
                skipped(SkipReason::Detached),
                skipped(SkipReason::Unborn { branch: s("main") }),
                skipped(SkipReason::NoUpstream { branch: s("x") }),
                skipped(SkipReason::Diverged {
                    branch: s("x"),
                    ahead: 1,
                    behind: 2,
                }),
                skipped(SkipReason::MergeRefused {
                    stderr_tail: s("error: untracked working tree files would be overwritten"),
                }),
                skipped(SkipReason::DefaultBranchDiverged {
                    branch: s("main"),
                    ahead: 1,
                    behind: 1,
                }),
                skipped(SkipReason::Busy {
                    step: UpdateStep::FastForward,
                    stderr_tail: s("fatal: Unable to create '/r/.git/index.lock': File exists."),
                }),
            ],
        },
        LastUpdate {
            time: at,
            policy: RepoUpdatePolicy::Ff,
            outcomes: vec![
                UpdateOutcome::Failed {
                    step: UpdateStep::Fetch,
                    error: s("timed out after 120s"),
                    stderr_tail: String::new(),
                },
                UpdateOutcome::Failed {
                    step: UpdateStep::FastForward,
                    error: s("exit status 128"),
                    stderr_tail: s("fatal: x"),
                },
                UpdateOutcome::Failed {
                    step: UpdateStep::MoveDefaultBranch,
                    error: s("cannot start git: not found"),
                    stderr_tail: String::new(),
                },
            ],
        },
    ]
}

#[test]
fn update_samples_validate_against_their_schema() {
    let at = Timestamp::from_unix_secs(1_791_792_000).unwrap();
    let mut errors = Vec::new();
    let updates = last_updates(at);
    for update in &updates {
        errors.extend(violations(update).unwrap());
    }
    let mut repos: Vec<RepoSync> = updates
        .into_iter()
        .map(|update| RepoSync {
            slug: "personal/loom".to_owned(),
            result: SyncOutcome::Updated { update },
        })
        .collect();
    repos.push(RepoSync {
        slug: "personal/x".to_owned(),
        result: SyncOutcome::Planned {
            policy: RepoUpdatePolicy::Ff,
            actions: vec![
                PlannedAction::Fetch,
                PlannedAction::Skip {
                    reason: SkipReason::Detached,
                },
                PlannedAction::DecideAfterFetch,
            ],
        },
    });
    errors.extend(violations(&SyncResult { repos }).unwrap());
    errors.extend(violations(&SyncRequest::default()).unwrap());
    errors.extend(
        violations(&SyncRequest {
            repos: vec!["personal/loom".to_owned()],
            dry_run: true,
        })
        .unwrap(),
    );
    assert!(errors.is_empty(), "schema violations: {errors:?}");

    let schema = serde_json::to_value(schema_for!(SyncRequest)).unwrap();
    assert!(jsonschema::is_valid(&schema, &json!({})));
    assert!(!jsonschema::is_valid(&schema, &json!({ "dryrun": true })));
}

#[test]
fn diagnostics_samples_validate_against_their_schema() {
    let mut errors = Vec::new();
    for ssh_auth_sock in [
        SshAgentSocket::Unset,
        SshAgentSocket::Missing {
            path: "/tmp/gone".into(),
        },
        SshAgentSocket::NotASocket {
            path: "/tmp/file".into(),
        },
        SshAgentSocket::Socket {
            path: "/run/user/1000/ssh-agent.socket".into(),
        },
    ] {
        errors.extend(
            violations(&DaemonDiagnostics {
                ssh_auth_sock: ssh_auth_sock.clone(),
                home: Some("/home/me".into()),
            })
            .unwrap(),
        );
        errors.extend(
            violations(&DaemonDiagnostics {
                ssh_auth_sock,
                home: None,
            })
            .unwrap(),
        );
    }
    let check = |name: &str, status, fix: Option<&str>| DoctorCheck {
        check: name.to_owned(),
        status,
        detail: "detail".to_owned(),
        fix: fix.map(str::to_owned),
    };
    errors.extend(
        violations(&DoctorReport {
            checks: vec![
                check("roots", CheckStatus::Ok, None),
                check(
                    "remote git@example.com (ssh, via personal/loom)",
                    CheckStatus::Fail,
                    Some("ssh-add"),
                ),
                check("unassigned", CheckStatus::Warn, Some("define a context")),
                check("updates", CheckStatus::Warn, Some("forget the clone")),
            ],
        })
        .unwrap(),
    );
    errors.extend(violations(&DoctorReport { checks: vec![] }).unwrap());
    assert!(errors.is_empty(), "schema violations: {errors:?}");
}

#[test]
fn remote_check_samples_validate_against_their_schema() {
    let mut errors = Vec::new();
    let remote = |endpoint: &str, scheme, result| RemoteCheck {
        endpoint: endpoint.to_owned(),
        scheme,
        via: "personal/loom".to_owned(),
        path: "/home/me/perso/loom".into(),
        remote: "origin".to_owned(),
        result,
    };
    let failed = |problem| RemoteCheckOutcome::Failed {
        problem,
        error: "exit status: 128".to_owned(),
        stderr_tail: "fatal: x\n".to_owned(),
    };
    errors.extend(
        violations(&RemoteCheckResult {
            remotes: vec![
                remote(
                    "git@github.com",
                    RemoteScheme::Ssh,
                    RemoteCheckOutcome::Reachable,
                ),
                remote(
                    "git@gitlab.com",
                    RemoteScheme::Ssh,
                    failed(RemoteProblem::SshAuth),
                ),
                remote(
                    "https://gitlab.example",
                    RemoteScheme::Https,
                    failed(RemoteProblem::HttpsAuth),
                ),
                remote(
                    "http://h",
                    RemoteScheme::Http,
                    failed(RemoteProblem::NotFound),
                ),
                remote("git://h", RemoteScheme::Git, failed(RemoteProblem::Network)),
                remote("/srv/x", RemoteScheme::Local, failed(RemoteProblem::Other)),
                remote(
                    "s3://b/x",
                    RemoteScheme::Other,
                    RemoteCheckOutcome::Reachable,
                ),
            ],
        })
        .unwrap(),
    );
    errors.extend(violations(&RemoteCheckResult { remotes: vec![] }).unwrap());
    assert!(errors.is_empty(), "schema violations: {errors:?}");
    for scheme in [
        RemoteScheme::Ssh,
        RemoteScheme::Https,
        RemoteScheme::Http,
        RemoteScheme::Git,
        RemoteScheme::Local,
        RemoteScheme::Other,
    ] {
        assert_eq!(
            serde_json::to_value(scheme).unwrap(),
            json!(scheme.to_string())
        );
    }
    for problem in [
        RemoteProblem::SshAuth,
        RemoteProblem::HttpsAuth,
        RemoteProblem::NotFound,
        RemoteProblem::Network,
        RemoteProblem::Other,
    ] {
        assert_eq!(
            serde_json::to_value(problem).unwrap(),
            json!(problem.to_string())
        );
    }
}

#[test]
fn register_request_on_use_is_optional() {
    let request: RegisterRequest = serde_json::from_value(json!({ "path": "/x" })).unwrap();
    assert!(!request.on_use);
    // Omitted when false: an older daemon, which rejects unknown fields, still reads it.
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({ "path": "/x" })
    );
}

#[test]
fn repo_summary_inlines_the_identity() {
    let summary = RepoSummary {
        identity: repo_identity(ContextAssignment::Unassigned),
        registered: true,
        last_used: None,
        state: None,
        last_update: None,
    };
    let value = serde_json::to_value(&summary).unwrap();
    assert_eq!(value["slug"], "unassigned/loom");
    assert_eq!(value["registered"], true);
    assert!(value.get("last_used").is_none());
    // Not refreshed yet: `null`, present.
    assert_eq!(value["state"], Value::Null);
    assert!(value.get("state").is_some());
    assert_eq!(value["last_update"], Value::Null);
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

fn sample_task(id: &TaskId) -> Result<Task, Box<dyn Error>> {
    Ok(Task {
        id: id.clone(),
        title: "Review the tasks API".to_owned(),
        kind: Kind::Review,
        context: "work".parse()?,
        status: Status::Waiting {
            on: Some("Alice".to_owned()),
            until: Some("2026-10-20".parse()?),
        },
        importance: loom_api::Importance::P1,
        due: Some("2026-10-21".parse()?),
        pinned: true,
        links: Vec::new(),
        relations: Relations::default(),
        created: Timestamp::from_unix_secs(1_791_792_000).ok_or("timestamp")?,
        updated: Timestamp::from_unix_secs(1_791_795_600).ok_or("timestamp")?,
    })
}

#[test]
fn task_samples_validate_against_their_schema() {
    let id: TaskId = "k3q7zm2a".parse().unwrap();
    let task = sample_task(&id).unwrap();
    let summary = TaskSummary {
        id: id.clone(),
        prefix: "k3".to_owned(),
        context: task.context.clone(),
        task: Some(task.clone()),
        read_only: None,
        rank: Some(TaskRank {
            position: 1,
            reason: RankReason::First,
        }),
    };
    let invalid = TaskSummary {
        task: None,
        read_only: Some(TaskReadOnly::InvalidFile("line 2: invalid TOML".to_owned())),
        rank: None,
        ..summary.clone()
    };
    let mut errors = violations(&TaskList {
        tasks: vec![summary.clone(), invalid.clone()],
    })
    .unwrap();
    for summary in [summary, invalid] {
        errors.extend(
            violations(&TaskDetail {
                summary,
                body: "## Notes\n".to_owned(),
                actions: vec![
                    Action::SetStatus(StatusName::Ready),
                    Action::Edit,
                    Action::Unpin,
                    Action::LogNote,
                ],
            })
            .unwrap(),
        );
    }
    let at = Timestamp::from_unix_secs(1_791_795_600).unwrap();
    let event = |id: &str, event| EventRecord {
        v: 1,
        id: id.parse::<EventId>().unwrap(),
        at,
        actor: Actor::User,
        machine_id: MachineId("laptop".to_owned()),
        event,
    };
    errors.extend(
        violations(&TaskEvents {
            id,
            events: vec![
                event(
                    "aaaaaaaa",
                    loom_api::Event::Created {
                        task: Box::new(task),
                    },
                ),
                event(
                    "bbbbbbbb",
                    loom_api::Event::Edited {
                        changes: vec![FieldChange::Pinned(Change {
                            from: false,
                            to: true,
                        })],
                    },
                ),
            ],
            skipped: Vec::new(),
        })
        .unwrap(),
    );
    assert!(errors.is_empty(), "schema violations: {errors:?}");
}

#[test]
fn task_requests_and_problems_validate_against_their_schema() {
    let id: TaskId = "k3q7zm2a".parse().unwrap();
    let mut errors = Vec::new();
    errors.extend(
        violations(&NewTask {
            title: "Write the talk".to_owned(),
            kind: Kind::Chore,
            context: "work".parse().unwrap(),
            status: Status::Inbox,
            importance: None,
            due: None,
            pinned: false,
            links: Vec::new(),
            relations: Relations::default(),
        })
        .unwrap(),
    );
    errors.extend(
        violations(&TaskEdit {
            title: Some("Renamed".to_owned()),
            due: Some(None),
            ..TaskEdit::default()
        })
        .unwrap(),
    );
    errors.extend(
        violations(&TaskActionRequest {
            on: Some("Alice".to_owned()),
            until: Some("2026-10-20".parse().unwrap()),
            text: None,
        })
        .unwrap(),
    );
    errors.extend(violations(&TaskActionName::LogNote).unwrap());
    errors.extend(
        violations(&AmbiguousPrefix {
            prefix: "k3".to_owned(),
            candidates: vec![TaskCandidate {
                id,
                context: "work".parse().unwrap(),
                title: "Review the tasks API".to_owned(),
            }],
        })
        .unwrap(),
    );
    errors.extend(
        violations(&IllegalTransition {
            from: StatusName::Done,
            to: StatusName::Active,
        })
        .unwrap(),
    );
    errors.extend(
        violations(&UndoConflict {
            field: "status".to_owned(),
        })
        .unwrap(),
    );
    errors.extend(violations(&ErrorCode::AmbiguousPrefix).unwrap());
    assert!(errors.is_empty(), "schema violations: {errors:?}");
}

#[test]
fn backup_samples_validate_against_their_schema() {
    let at = Timestamp::from_unix_secs(1_760_000_000).unwrap();
    let outcomes = [
        BackupOutcome::UpToDate,
        BackupOutcome::Pushed { commits: 2 },
        BackupOutcome::Pulled { commits: 1 },
        BackupOutcome::Diverged {
            local: 3,
            remote: 1,
        },
        BackupOutcome::PushRejected {
            stderr_tail: " ! [rejected] HEAD -> main (fetch first)\n".to_owned(),
        },
        BackupOutcome::Failed {
            step: BackupStep::Fetch,
            error: "exit status: 128".to_owned(),
            stderr_tail: "fatal: unable to access\n".to_owned(),
        },
    ];
    let contexts = outcomes
        .into_iter()
        .map(|outcome| ContextBackup {
            context: "work".parse().unwrap(),
            path: "/home/me/.loom/contexts/work".into(),
            remote: Some("https://me:***@example.com/store.git".to_owned()),
            uncommitted: Some("commit failed: exit status: 1".to_owned()),
            last_sync: Some(BackupSync { time: at, outcome }),
        })
        .chain([ContextBackup {
            context: "home".parse().unwrap(),
            path: "/home/me/.loom/contexts/home".into(),
            remote: None,
            uncommitted: None,
            last_sync: None,
        }])
        .collect();
    let mut errors = violations(&BackupReport {
        machine: "laptop".to_owned(),
        contexts,
        not_held: vec!["lab".parse().unwrap()],
    })
    .unwrap();
    errors.extend(violations(&BackupRequest::default()).unwrap());
    errors.extend(
        violations(&BackupRequest {
            context: Some("work".parse().unwrap()),
        })
        .unwrap(),
    );
    assert!(errors.is_empty(), "schema violations: {errors:?}");

    let schema = serde_json::to_value(schema_for!(BackupRequest)).unwrap();
    assert!(jsonschema::is_valid(&schema, &json!({})));
    assert!(!jsonschema::is_valid(
        &schema,
        &json!({ "contexts": ["work"] })
    ));
}
