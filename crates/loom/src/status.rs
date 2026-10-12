//! Output of `loom status`, decided without I/O.

use loom_api::{StatusResponse, VersionInfo};

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
    let head = format!("loomd {} running\n", status.daemon.version);
    fields
        .into_iter()
        // Values aligned one column after the longest name and its colon.
        .map(|(name, value)| format!("{:<13}{value}\n", format!("{name}:")))
        .fold(head, |text, line| text + &line)
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
    use super::{render_human, version_mismatch};
    use loom_api::{ApiVersion, Channel, Profile, StatusResponse, Timestamp, VersionInfo};

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
        }
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
