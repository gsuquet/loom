//! Output of `loom status`, decided without I/O.

use std::time::Duration;

use loom_api::{DiskUsage, StatusResponse, UpdateResult, UpdateStatus, VersionInfo};
use loom_config::duration::format_duration;

/// Human output: `loomd <version> running`, then one `field: value` line per field.
#[must_use]
pub fn render_human(status: &StatusResponse) -> String {
    let fields = [
        ("pid", status.pid.to_string()),
        ("started_at", status.started_at.to_string()),
        ("uptime_secs", status.uptime_secs.to_string()),
        ("api_version", status.api_version.to_string()),
        ("commit", status.daemon.commit.clone()),
        ("channel", status.daemon.channel.to_string()),
        ("loom_home", status.loom_home.display().to_string()),
        ("socket_path", status.socket_path.display().to_string()),
        ("config_path", status.config_path.display().to_string()),
    ];
    // Absent from a daemon older than the field.
    let disk = status.disk.as_ref().map(|disk| ("disk", disk_text(disk)));
    let update = status
        .update
        .as_ref()
        .map(update_fields)
        .unwrap_or_default();
    let head = format!("loomd {} running\n", status.daemon.version);
    fields
        .into_iter()
        .chain(disk)
        .chain(update)
        // Values aligned one column after the longest name and its colon.
        .map(|(name, value)| format!("{:<13}{value}\n", format!("{name}:")))
        .fold(head, |text, line| text + &line)
}

/// Lines of the auto-upgrade state. `pending` only while an upgrade runs.
fn update_fields(update: &UpdateStatus) -> Vec<(&'static str, String)> {
    let policy = match update.channel {
        _ if update.policy == loom_api::UpdatePolicyMode::Off => "off".to_owned(),
        None => format!("{}, no channel: not checked", update.policy),
        Some(channel) => format!(
            "{}, channel {channel}{}, every {}",
            update.policy,
            // Absent from a daemon older than the field.
            update
                .channel_source
                .map_or_else(String::new, |source| format!(" ({source})")),
            format_duration(Duration::from_secs(update.check_interval_secs))
        ),
    };
    let mut fields = vec![
        ("updates", policy),
        (
            "last_check",
            update
                .last_check
                .map_or_else(|| "never".to_owned(), |at| at.to_string()),
        ),
        (
            "available",
            update
                .available
                .clone()
                .unwrap_or_else(|| "none".to_owned()),
        ),
    ];
    if let Some(pending) = &update.pending {
        fields.push((
            "pending",
            format!(
                "{} -> {} since {}",
                pending.from, pending.to, pending.started_at
            ),
        ));
    }
    let failed = if update.failed_versions.is_empty() {
        "none".to_owned()
    } else {
        update.failed_versions.join(", ")
    };
    fields.push(("failed", failed));
    fields.push((
        "last_result",
        update
            .last_result
            .as_ref()
            .map_or_else(|| "none".to_owned(), result_text),
    ));
    fields
}

/// One line describing the outcome of the last check or upgrade.
fn result_text(result: &UpdateResult) -> String {
    match result {
        UpdateResult::UpToDate { at } => format!("up to date at {at}"),
        UpdateResult::Available { at, version } => format!("{version} available at {at}"),
        UpdateResult::SkippedFailed { at, version } => {
            format!("{version} skipped at {at}: it failed before")
        }
        UpdateResult::Upgraded { at, from, to } => format!("upgraded {from} -> {to} at {at}"),
        UpdateResult::PreflightFailed {
            at,
            version,
            message,
        } => format!("{version} failed the pre-flight check at {at}: {message}"),
        UpdateResult::RolledBack {
            at,
            from,
            to,
            message,
        } => format!("rolled back {to} -> {from} at {at}: {message}"),
        UpdateResult::Interrupted { at, from, to } => {
            format!("upgrade {from} -> {to} interrupted, found at {at}")
        }
        UpdateResult::Failed { at, message } => format!("failed at {at}: {message}"),
    }
}

/// Size in bytes for humans: decimal units (1 KB = 1,000 bytes), one decimal below 10.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    // u128: `bytes * 10` and the rounding never overflow.
    let value = u128::from(bytes);
    let mut unit = 0;
    let mut scale: u128 = 1;
    while unit + 1 < UNITS.len() && value >= scale * 1_000 {
        scale *= 1_000;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    let tenths = (value * 10 + scale / 2) / scale;
    if tenths < 100 {
        return format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit]);
    }
    let whole = (value + scale / 2) / scale;
    if whole >= 1_000 && unit + 1 < UNITS.len() {
        // Rounds up to the next unit: 999,500 bytes is 1.0 MB, not 1000 KB.
        return format!("1.0 {}", UNITS[unit + 1]);
    }
    format!("{whole} {}", UNITS[unit])
}

/// The `disk` line: total, then versions, index, logs and context stores.
#[must_use]
pub fn disk_text(disk: &DiskUsage) -> String {
    format!(
        "{} (versions {}, index {}, logs {}, contexts {})",
        format_bytes(disk.total_bytes),
        format_bytes(disk.versions_bytes),
        format_bytes(disk.index_bytes),
        format_bytes(disk.logs_bytes),
        format_bytes(disk.contexts_bytes()),
    )
}

/// Warning when the `loom` binary (`cli`) and the running daemon (`daemon`) are different
/// builds: their `version` or their `commit` differ. `None` when they match.
#[must_use]
pub fn version_mismatch(cli: &VersionInfo, daemon: &VersionInfo, pid: u32) -> Option<String> {
    if cli.version == daemon.version && cli.commit == daemon.commit {
        return None;
    }
    Some(format!(
        "loom {} (commit {}) and the running loomd {} (commit {}) are different builds: restart loomd (PID {pid}) to run the same build",
        cli.version,
        short_commit(&cli.commit),
        daemon.version,
        short_commit(&daemon.commit),
    ))
}

/// First 12 characters of a commit hash; `unknown` is kept as is.
fn short_commit(commit: &str) -> &str {
    commit
        .char_indices()
        .nth(12)
        .map_or(commit, |(end, _)| &commit[..end])
}

#[cfg(test)]
mod tests {
    use super::{disk_text, format_bytes, render_human, result_text, version_mismatch};
    use loom_api::{
        ApiVersion, Channel, ChannelSource, ContextDiskUsage, DiskUsage, PendingUpgrade, Profile,
        StatusResponse, Timestamp, UpdateChannel, UpdatePolicyMode, UpdateResult, UpdateStatus,
        VersionInfo,
    };

    fn info(version: &str, commit: &str) -> VersionInfo {
        VersionInfo {
            version: version.to_owned(),
            commit: commit.to_owned(),
            commit_date: "2026-10-12T10:00:00+02:00".to_owned(),
            target: "aarch64-apple-darwin".to_owned(),
            channel: Channel::Dev,
            profile: Profile::Release,
        }
    }

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn sample() -> StatusResponse {
        StatusResponse {
            api_version: ApiVersion::V1,
            daemon: info("0.1.0-dev.42", COMMIT),
            pid: 4242,
            started_at: Timestamp::from_unix_secs(1_791_792_000).unwrap(),
            uptime_secs: 90,
            loom_home: "/home/me/.loom".into(),
            socket_path: "/home/me/.loom/run/loomd.sock".into(),
            config_path: "/home/me/.loom/config.toml".into(),
            update: None,
            disk: None,
        }
    }

    fn at() -> Timestamp {
        Timestamp::from_unix_secs(1_791_792_000).unwrap()
    }

    fn update() -> UpdateStatus {
        UpdateStatus {
            policy: UpdatePolicyMode::Auto,
            channel: Some(UpdateChannel::Stable),
            channel_source: Some(ChannelSource::Build),
            check_interval_secs: 21_600,
            last_check: Some(at()),
            available: None,
            pending: None,
            failed_versions: vec![],
            last_result: Some(UpdateResult::UpToDate { at: at() }),
        }
    }

    #[test]
    fn update_lines_follow_the_status() {
        let status = StatusResponse {
            update: Some(update()),
            ..sample()
        };
        assert!(
            render_human(&status).ends_with(
                "config_path: /home/me/.loom/config.toml\n\
                 updates:     auto, channel stable (build), every 6h\n\
                 last_check:  2026-10-12T08:00:00Z\n\
                 available:   none\n\
                 failed:      none\n\
                 last_result: up to date at 2026-10-12T08:00:00Z\n"
            ),
            "{}",
            render_human(&status)
        );

        let notify = UpdateStatus {
            policy: UpdatePolicyMode::Notify,
            last_check: None,
            available: Some("0.2.0".to_owned()),
            pending: Some(PendingUpgrade {
                from: "0.1.0".to_owned(),
                to: "0.2.0".to_owned(),
                started_at: at(),
            }),
            failed_versions: vec!["0.1.5".to_owned(), "0.1.6".to_owned()],
            last_result: None,
            ..update()
        };
        let text = render_human(&StatusResponse {
            update: Some(notify),
            ..sample()
        });
        assert!(text.contains("\nlast_check:  never\n"), "{text}");
        assert!(text.contains("\navailable:   0.2.0\n"), "{text}");
        assert!(
            text.contains("\npending:     0.1.0 -> 0.2.0 since 2026-10-12T08:00:00Z\n"),
            "{text}"
        );
        assert!(text.contains("\nfailed:      0.1.5, 0.1.6\n"), "{text}");
        assert!(text.ends_with("\nlast_result: none\n"), "{text}");
    }

    #[test]
    fn update_policy_line() {
        let line = |update: UpdateStatus| {
            let text = render_human(&StatusResponse {
                update: Some(update),
                ..sample()
            });
            text.lines()
                .find(|line| line.starts_with("updates:"))
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            line(UpdateStatus {
                policy: UpdatePolicyMode::Off,
                ..update()
            }),
            "updates:     off"
        );
        assert_eq!(
            line(UpdateStatus {
                policy: UpdatePolicyMode::Notify,
                channel: None,
                ..update()
            }),
            "updates:     notify, no channel: not checked"
        );
        for (source, name) in [
            (ChannelSource::Config, "config"),
            (ChannelSource::Chosen, "chosen"),
            (ChannelSource::Build, "build"),
        ] {
            assert_eq!(
                line(UpdateStatus {
                    channel: Some(UpdateChannel::Dev),
                    channel_source: Some(source),
                    ..update()
                }),
                format!("updates:     auto, channel dev ({name}), every 6h")
            );
        }
        // A daemon older than `channel_source`.
        assert_eq!(
            line(UpdateStatus {
                channel_source: None,
                ..update()
            }),
            "updates:     auto, channel stable, every 6h"
        );
    }

    #[test]
    fn every_result_has_a_distinct_line() {
        let s = |text: &str| text.to_owned();
        let results = [
            UpdateResult::UpToDate { at: at() },
            UpdateResult::Available {
                at: at(),
                version: s("0.2.0"),
            },
            UpdateResult::SkippedFailed {
                at: at(),
                version: s("0.2.0"),
            },
            UpdateResult::Upgraded {
                at: at(),
                from: s("0.1.0"),
                to: s("0.2.0"),
            },
            UpdateResult::PreflightFailed {
                at: at(),
                version: s("0.2.0"),
                message: s("m"),
            },
            UpdateResult::RolledBack {
                at: at(),
                from: s("0.1.0"),
                to: s("0.2.0"),
                message: s("m"),
            },
            UpdateResult::Interrupted {
                at: at(),
                from: s("0.1.0"),
                to: s("0.2.0"),
            },
            UpdateResult::Failed {
                at: at(),
                message: s("m"),
            },
        ];
        let lines: Vec<String> = results.iter().map(result_text).collect();
        assert_eq!(
            lines[5],
            "rolled back 0.2.0 -> 0.1.0 at 2026-10-12T08:00:00Z: m"
        );
        for (i, a) in lines.iter().enumerate() {
            assert!(a.contains("2026-10-12T08:00:00Z"), "{a}");
            for b in &lines[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn sizes_use_decimal_units() {
        let cases = [
            (0, "0 B"),
            (999, "999 B"),
            (1_000, "1.0 KB"),
            (4_200_000, "4.2 MB"),
            (4_249_999, "4.2 MB"),
            (4_250_000, "4.3 MB"),
            (9_949, "9.9 KB"),
            (9_950, "10 KB"),
            (24_576, "25 KB"),
            (35_000_000, "35 MB"),
            (999_499, "999 KB"),
            (999_500, "1.0 MB"),
            (2_000_000_000, "2.0 GB"),
            (u64::MAX, "18 EB"),
        ];
        for (bytes, text) in cases {
            assert_eq!(format_bytes(bytes), text, "{bytes}");
        }
    }

    fn disk() -> DiskUsage {
        DiskUsage {
            computed_at: at(),
            total_bytes: 35_000_000,
            versions_bytes: 30_000_000,
            index_bytes: 4_200_000,
            index_wal_bytes: 4_096,
            logs_bytes: 24_000,
            state_bytes: 8_192,
            other_bytes: 767_808,
            contexts: vec![],
            kept_versions: vec!["0.1.0".to_owned()],
            available_bytes: Some(1),
            warnings: vec![],
        }
    }

    #[test]
    fn disk_line_has_the_total_and_the_main_parts() {
        assert_eq!(
            disk_text(&disk()),
            "35 MB (versions 30 MB, index 4.2 MB, logs 24 KB, contexts 0 B)"
        );
        let with_contexts = DiskUsage {
            contexts: vec![
                ContextDiskUsage {
                    name: "a".to_owned(),
                    bytes: 1_500,
                },
                ContextDiskUsage {
                    name: "b".to_owned(),
                    bytes: 500,
                },
            ],
            ..disk()
        };
        assert!(disk_text(&with_contexts).ends_with(", contexts 2.0 KB)"));
    }

    #[test]
    fn disk_line_follows_the_paths() {
        let text = render_human(&StatusResponse {
            disk: Some(disk()),
            update: Some(update()),
            ..sample()
        });
        assert!(
            text.contains(
                "\nconfig_path: /home/me/.loom/config.toml\n\
                 disk:        35 MB (versions 30 MB, index 4.2 MB, logs 24 KB, contexts 0 B)\n\
                 updates:"
            ),
            "{text}"
        );
    }

    #[test]
    fn human_output_has_one_field_per_line() {
        assert_eq!(
            render_human(&sample()),
            "loomd 0.1.0-dev.42 running\n\
             pid:         4242\n\
             started_at:  2026-10-12T08:00:00Z\n\
             uptime_secs: 90\n\
             api_version: v1\n\
             commit:      0123456789abcdef0123456789abcdef01234567\n\
             channel:     dev\n\
             loom_home:   /home/me/.loom\n\
             socket_path: /home/me/.loom/run/loomd.sock\n\
             config_path: /home/me/.loom/config.toml\n"
        );
    }

    #[test]
    fn same_version_and_commit_match() {
        let mut daemon = info("0.1.0", COMMIT);
        // Other fields do not matter.
        daemon.channel = Channel::Local;
        daemon.profile = Profile::Debug;
        assert_eq!(version_mismatch(&info("0.1.0", COMMIT), &daemon, 1), None);
    }

    #[test]
    fn different_version_is_reported_with_both_versions() {
        let warning =
            version_mismatch(&info("0.1.0", COMMIT), &info("0.2.0", COMMIT), 4242).unwrap();
        assert_eq!(
            warning,
            "loom 0.1.0 (commit 0123456789ab) and the running loomd 0.2.0 (commit 0123456789ab) \
             are different builds: restart loomd (PID 4242) to run the same build"
        );
    }

    #[test]
    fn different_commit_is_reported() {
        let warning =
            version_mismatch(&info("0.1.0", COMMIT), &info("0.1.0", "unknown"), 7).unwrap();
        assert!(warning.contains("(commit 0123456789ab)"), "{warning}");
        assert!(warning.contains("(commit unknown)"), "{warning}");
        assert!(warning.contains("restart loomd (PID 7)"), "{warning}");
    }
}
