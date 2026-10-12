//! `loom upgrade` and `loom rollback`: wiring of `loom-update`, and their human output.

use std::env;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command as Process;
use std::time::{SystemTime, UNIX_EPOCH};

use loom_api::{
    Channel as BuildChannel, DaemonRestart, InstallReport, RollbackReport, UpdateState,
    UpgradeDecision, UpgradeReport, UpgradeSkipReason, UpgradeSource, VersionInfo,
};
use loom_config::layout::Layout;
use loom_update::auto;
use loom_update::github::{self, Client};
use loom_update::{
    Channel, Installed, Lookup, Paths, SkipReason, Staged, UpdateDecision, UpdateError, UpdateMode,
    UpdatePolicy,
};
use miette::{IntoDiagnostic as _, Result, WrapErr as _, miette};
use semver::Version;
use tracing::{info, warn};

use loom::cli::{ReleaseChannel, RollbackArgs, UpgradeArgs};

/// Runs `loom upgrade`. Returns the text to print on stdout.
pub fn upgrade(info: &VersionInfo, args: &UpgradeArgs, layout: &Layout) -> Result<String> {
    let current = running_version(info)?;
    let paths = paths(layout)?;
    let state = read_state(layout);
    if !args.check {
        refuse_while_pending(&state, unix_now())?;
    }
    if let Some(from) = &args.from {
        return upgrade_from(&current, from, &paths, args);
    }

    let (lookup, channel) = lookup(info.channel, args)?;
    let policy = UpdatePolicy {
        mode: UpdateMode::Auto,
        pinned: args.version.clone(),
    };
    let client = client(info);
    let discovery = loom_update::discover(&client, &lookup).map_err(|e| failure(&e))?;
    // `--version` may retry a failed version on purpose; a channel lookup never picks one.
    let failed = if args.version.is_some() {
        Vec::new()
    } else {
        auto::failed_versions(&state)
    };
    let decision = loom_update::decide(&current, channel, &discovery.versions(), &policy, &failed);
    let mut report = UpgradeReport {
        current_version: current.to_string(),
        source: UpgradeSource::Release {
            channel: match lookup {
                Lookup::Channel(Channel::Stable) => Some(BuildChannel::Stable),
                Lookup::Channel(Channel::Dev) => Some(BuildChannel::Dev),
                Lookup::Version(_) => None,
            },
            pinned: args.version.as_ref().map(ToString::to_string),
        },
        decision: api_decision(&decision),
        check: args.check,
        dry_run: args.dry_run,
        install: None,
    };
    if args.check {
        return render(&report, args.json, upgrade_text);
    }
    let target = match decision {
        UpdateDecision::UpToDate => return render(&report, args.json, upgrade_text),
        UpdateDecision::Skipped { reason } => {
            return Err(miette!("{}", skip_message(&reason, &current)));
        }
        UpdateDecision::Apply { version } | UpdateDecision::Available { version } => version,
    };
    let Some(release) = discovery.release(&target) else {
        return Err(miette!("internal error: no release for version {target}"));
    };
    loom_update::install::check_writable(&paths.install_dir).map_err(|e| not_writable(&e))?;

    let public_key = env::var(loom_update::PUBLIC_KEY_ENV).ok();
    let staged = loom_update::download(
        &client,
        release,
        &target,
        &loom_update::release::release_target(&info.target),
        loom_update::public_key(public_key.as_deref()),
        &paths.staging_dir,
        &mut |line: &str| info!("{line}"),
    )
    .map_err(|e| failure(&e))?;
    report.install = Some(apply(&staged, &paths, &current, true, args.dry_run)?);
    render(&report, args.json, upgrade_text)
}

fn upgrade_from(
    current: &Version,
    from: &Path,
    paths: &Paths,
    args: &UpgradeArgs,
) -> Result<String> {
    let from = std::path::absolute(from)
        .into_diagnostic()
        .wrap_err_with(|| format!("invalid path {}", from.display()))?;
    warn!(
        "{} is a local build: its signature and checksum are not verified",
        from.display()
    );
    loom_update::install::check_writable(&paths.install_dir).map_err(|e| not_writable(&e))?;
    let staged =
        loom_update::stage::from_local(&from, &paths.staging_dir).map_err(|e| failure(&e))?;
    let install = apply(&staged, paths, current, false, args.dry_run)?;
    let report = UpgradeReport {
        current_version: current.to_string(),
        source: UpgradeSource::Local { path: from },
        decision: UpgradeDecision::Apply {
            version: staged.version.to_string(),
        },
        check: false,
        dry_run: args.dry_run,
        install: Some(install),
    };
    render(&report, args.json, upgrade_text)
}

/// Runs `loom rollback`. Returns the text to print on stdout.
pub fn rollback(info: &VersionInfo, args: &RollbackArgs, layout: &Layout) -> Result<String> {
    let current = running_version(info)?;
    refuse_while_pending(&read_state(layout), unix_now())?;
    let paths = paths(layout)?;
    loom_update::install::check_writable(&paths.install_dir).map_err(|e| not_writable(&e))?;
    let staged = loom_update::rollback_source(&paths, &current).map_err(|error| match error {
        UpdateError::NothingToRollBack { .. } => miette!("{error}"),
        other => failure(&other),
    })?;
    let report = RollbackReport {
        current_version: current.to_string(),
        dry_run: args.dry_run,
        install: apply(&staged, &paths, &current, false, args.dry_run)?,
    };
    render(&report, args.json, rollback_text)
}

/// Installs `staged`, or describes it with `--dry-run`.
fn apply(
    staged: &Staged,
    paths: &Paths,
    current: &Version,
    verified: bool,
    dry_run: bool,
) -> Result<InstallReport> {
    let mut report = InstallReport {
        version: staged.version.to_string(),
        install_dir: paths.install_dir.clone(),
        verified,
        installed: false,
        saved_to: None,
        pruned: Vec::new(),
        daemon: DaemonRestart::Skipped,
    };
    if dry_run {
        return Ok(report);
    }
    let Installed {
        saved_to,
        pruned,
        daemon,
        warnings,
    } = loom_update::install(staged, paths, current).map_err(|e| failure(&e))?;
    for warning in &warnings {
        warn!("{warning}");
    }
    if let DaemonRestart::Failed { message, .. } = &daemon {
        warn!("loomd keeps running the replaced version: {message}");
    }
    report.installed = true;
    report.saved_to = Some(saved_to);
    report.pruned = pruned.iter().map(ToString::to_string).collect();
    report.daemon = daemon;
    Ok(report)
}

/// The update state of loomd; an unreadable one is logged and treated as empty.
fn read_state(layout: &Layout) -> UpdateState {
    loom_update::state::read(&layout.update_state).unwrap_or_else(|error| {
        warn!("{error}");
        UpdateState::default()
    })
}

/// Fails while loomd is in the middle of an upgrade (a `pending` that is not stale).
fn refuse_while_pending(state: &UpdateState, now: u64) -> Result<()> {
    auto::blocking_pending(state, now).map_or(Ok(()), |pending| {
        Err(miette!(
            "loomd is upgrading from {} to {} (started {}): retry when it is done, in a minute or two",
            pending.from,
            pending.to,
            pending.started_at
        ))
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn running_version(info: &VersionInfo) -> Result<Version> {
    Version::parse(&info.version)
        .into_diagnostic()
        .wrap_err_with(|| format!("this build's version `{}` is not semver", info.version))
}

/// What to look up and the channel to decide on. A local build has no channel of its own.
fn lookup(build: BuildChannel, args: &UpgradeArgs) -> Result<(Lookup, Channel)> {
    let channel = match (args.channel, build) {
        (Some(ReleaseChannel::Stable), _) | (None, BuildChannel::Stable) => Some(Channel::Stable),
        (Some(ReleaseChannel::Dev), _) | (None, BuildChannel::Dev) => Some(Channel::Dev),
        (None, BuildChannel::Local) => None,
    };
    match (&args.version, channel) {
        // The channel does not matter for a pinned version.
        (Some(version), channel) => Ok((
            Lookup::Version(version.clone()),
            channel.unwrap_or(Channel::Stable),
        )),
        (None, Some(channel)) => Ok((Lookup::Channel(channel), channel)),
        (None, None) => Err(miette!(
            "this is a local build (channel local): pass --channel, --version or --from"
        )),
    }
}

/// Paths from the running binary and the loom home.
fn paths(layout: &Layout) -> Result<Paths> {
    let exe = env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .into_diagnostic()
        .wrap_err("cannot find the running loom binary")?;
    let install_dir = exe
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| miette!("{} has no parent directory", exe.display()))?;
    Ok(Paths {
        install_dir,
        versions_dir: layout.versions_dir.clone(),
        staging_dir: layout.staging_dir.clone(),
        lock: layout.lock.clone(),
    })
}

fn client(info: &VersionInfo) -> Client {
    let releases_url = env::var(github::RELEASES_URL_ENV).ok();
    let base = github::base_url(releases_url.as_deref());
    let token = github::resolve_token(
        env::var_os(github::TOKEN_ENVS[0]).as_deref(),
        env::var_os(github::TOKEN_ENVS[1]).as_deref(),
        releases_url.as_deref().is_some_and(|url| !url.is_empty()),
        gh_auth_token,
    );
    Client::new(base, token, &format!("loom/{}", info.version))
}

/// `gh auth token`, when `gh` is installed and logged in.
fn gh_auth_token() -> Option<String> {
    let output = Process::new("gh").args(["auth", "token"]).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

fn failure(error: &UpdateError) -> miette::Report {
    miette!("{error}; nothing was installed")
}

fn not_writable(error: &loom_update::install::IoError) -> miette::Report {
    miette!(
        "cannot upgrade {}: the directory is not writable ({})",
        error.path.display(),
        error.source
    )
}

fn skip_message(reason: &SkipReason, current: &Version) -> String {
    match reason {
        SkipReason::UpdatesOff => "updates are turned off".to_owned(),
        SkipReason::PinnedNotReleased { pinned } => format!("version {pinned} is not released"),
        SkipReason::PinnedBelowCurrent { pinned } => format!(
            "version {pinned} is lower than the running version {current}; use loom rollback to go back"
        ),
        SkipReason::Failed { version } => format!(
            "version {version} failed a health check of the auto-upgrade; pass --version {version} to install it anyway"
        ),
    }
}

fn api_decision(decision: &UpdateDecision) -> UpgradeDecision {
    match decision {
        UpdateDecision::UpToDate => UpgradeDecision::UpToDate,
        UpdateDecision::Available { version } => UpgradeDecision::Available {
            version: version.to_string(),
        },
        UpdateDecision::Apply { version } => UpgradeDecision::Apply {
            version: version.to_string(),
        },
        UpdateDecision::Skipped { reason } => UpgradeDecision::Skipped {
            reason: match reason {
                SkipReason::UpdatesOff => UpgradeSkipReason::UpdatesOff,
                SkipReason::PinnedNotReleased { pinned } => UpgradeSkipReason::PinnedNotReleased {
                    pinned: pinned.to_string(),
                },
                SkipReason::PinnedBelowCurrent { pinned } => {
                    UpgradeSkipReason::PinnedBelowCurrent {
                        pinned: pinned.to_string(),
                    }
                }
                SkipReason::Failed { version } => UpgradeSkipReason::Failed {
                    version: version.to_string(),
                },
            },
        },
    }
}

fn render<T: serde::Serialize>(report: &T, json: bool, text: fn(&T) -> String) -> Result<String> {
    if json {
        let mut out = serde_json::to_string_pretty(report).into_diagnostic()?;
        out.push('\n');
        Ok(out)
    } else {
        Ok(text(report))
    }
}

/// Human output of `loom upgrade`.
fn upgrade_text(report: &UpgradeReport) -> String {
    let current = &report.current_version;
    let source = match &report.source {
        UpgradeSource::Release {
            channel: Some(channel),
            ..
        } => format!(" ({channel})"),
        UpgradeSource::Release { channel: None, .. } | UpgradeSource::Local { .. } => String::new(),
    };
    let mut out = match &report.decision {
        UpgradeDecision::UpToDate => format!("loom {current} is up to date{source}\n"),
        UpgradeDecision::Available { version } | UpgradeDecision::Apply { version } => report
            .install
            .as_ref()
            .filter(|_| !report.check)
            .map_or_else(
                || format!("loom {version} is available{source}, running {current}\n"),
                |install| install_text("upgrade", current, version, install),
            ),
        UpgradeDecision::Skipped { reason } => match reason {
            UpgradeSkipReason::UpdatesOff => "no upgrade: updates are turned off\n".to_owned(),
            UpgradeSkipReason::PinnedNotReleased { pinned } => {
                format!("no upgrade: version {pinned} is not released\n")
            }
            UpgradeSkipReason::PinnedBelowCurrent { pinned } => {
                format!(
                    "no upgrade: version {pinned} is lower than the running version {current}\n"
                )
            }
            UpgradeSkipReason::Failed { version } => {
                format!("no upgrade: version {version} failed a health check\n")
            }
        },
    };
    if let UpgradeSource::Local { path } = &report.source {
        let _ = writeln!(out, "source:   {} (not verified)", path.display());
    }
    out
}

/// Human output of `loom rollback`.
fn rollback_text(report: &RollbackReport) -> String {
    install_text(
        "roll back",
        &report.current_version,
        &report.install.version,
        &report.install,
    )
}

fn install_text(verb: &str, from: &str, to: &str, install: &InstallReport) -> String {
    let dir = install.install_dir.display();
    if !install.installed {
        return format!("would {verb} loom and loomd from {from} to {to} in {dir}\n");
    }
    let done = match verb {
        "upgrade" => "upgraded",
        _ => "rolled back",
    };
    let mut out = format!("{done} loom and loomd from {from} to {to} in {dir}\n");
    if let Some(saved) = &install.saved_to {
        let _ = writeln!(out, "saved {from} in {}", saved.display());
    }
    if !install.pruned.is_empty() {
        let _ = writeln!(out, "removed saved versions {}", install.pruned.join(", "));
    }
    let _ = writeln!(out, "{}", daemon_text(&install.daemon));
    out
}

fn daemon_text(daemon: &DaemonRestart) -> String {
    match daemon {
        DaemonRestart::Skipped => "loomd left alone".to_owned(),
        DaemonRestart::NotRunning => "loomd is not running".to_owned(),
        DaemonRestart::Signalled { pid } => {
            format!("sent SIGTERM to loomd (PID {pid}) so that it restarts on the new version")
        }
        DaemonRestart::Failed { .. } => {
            "loomd could not be stopped: it keeps running the replaced version".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(installed: bool, daemon: DaemonRestart) -> InstallReport {
        InstallReport {
            version: "0.2.0".to_owned(),
            install_dir: "/bin".into(),
            verified: true,
            installed,
            saved_to: installed.then(|| "/h/versions/0.1.0".into()),
            pruned: if installed {
                vec!["0.0.1".to_owned()]
            } else {
                Vec::new()
            },
            daemon,
        }
    }

    fn report(
        decision: UpgradeDecision,
        check: bool,
        install: Option<InstallReport>,
    ) -> UpgradeReport {
        UpgradeReport {
            current_version: "0.1.0".to_owned(),
            source: UpgradeSource::Release {
                channel: Some(BuildChannel::Stable),
                pinned: None,
            },
            decision,
            check,
            dry_run: false,
            install,
        }
    }

    fn apply() -> UpgradeDecision {
        UpgradeDecision::Apply {
            version: "0.2.0".to_owned(),
        }
    }

    #[test]
    fn check_output() {
        assert_eq!(
            upgrade_text(&report(UpgradeDecision::UpToDate, true, None)),
            "loom 0.1.0 is up to date (stable)\n"
        );
        assert_eq!(
            upgrade_text(&report(apply(), true, None)),
            "loom 0.2.0 is available (stable), running 0.1.0\n"
        );
    }

    #[test]
    fn upgrade_output() {
        assert_eq!(
            upgrade_text(&report(
                apply(),
                false,
                Some(install(true, DaemonRestart::Signalled { pid: 7 }))
            )),
            "upgraded loom and loomd from 0.1.0 to 0.2.0 in /bin\n\
             saved 0.1.0 in /h/versions/0.1.0\n\
             removed saved versions 0.0.1\n\
             sent SIGTERM to loomd (PID 7) so that it restarts on the new version\n"
        );
        assert_eq!(
            upgrade_text(&report(
                apply(),
                false,
                Some(install(false, DaemonRestart::Skipped))
            )),
            "would upgrade loom and loomd from 0.1.0 to 0.2.0 in /bin\n"
        );
    }

    #[test]
    fn local_source_is_flagged() {
        let mut local = report(apply(), false, Some(install(false, DaemonRestart::Skipped)));
        local.source = UpgradeSource::Local {
            path: "/tmp/build".into(),
        };
        assert!(upgrade_text(&local).ends_with("source:   /tmp/build (not verified)\n"));
    }

    #[test]
    fn rollback_output() {
        let rollback = RollbackReport {
            current_version: "0.3.0".to_owned(),
            dry_run: false,
            install: install(true, DaemonRestart::NotRunning),
        };
        assert!(
            rollback_text(&rollback)
                .starts_with("rolled back loom and loomd from 0.3.0 to 0.2.0 in /bin\n"),
        );
        assert!(rollback_text(&rollback).ends_with("loomd is not running\n"));
    }

    #[test]
    fn local_builds_need_a_channel_or_version() {
        let args = |channel, version: Option<&str>| UpgradeArgs {
            channel,
            version: version.map(|v| Version::parse(v).unwrap()),
            from: None,
            check: false,
            dry_run: false,
            json: false,
        };
        assert!(lookup(BuildChannel::Local, &args(None, None)).is_err());
        assert_eq!(
            lookup(BuildChannel::Local, &args(Some(ReleaseChannel::Dev), None)).unwrap(),
            (Lookup::Channel(Channel::Dev), Channel::Dev)
        );
        assert_eq!(
            lookup(BuildChannel::Dev, &args(None, None)).unwrap(),
            (Lookup::Channel(Channel::Dev), Channel::Dev)
        );
        assert_eq!(
            lookup(BuildChannel::Dev, &args(Some(ReleaseChannel::Stable), None)).unwrap(),
            (Lookup::Channel(Channel::Stable), Channel::Stable)
        );
        assert!(matches!(
            lookup(BuildChannel::Local, &args(None, Some("1.0.0"))).unwrap(),
            (Lookup::Version(_), _)
        ));
    }

    #[test]
    fn pending_upgrade_blocks_until_stale() {
        let started = loom_api::Timestamp::from_unix_secs(1_000).unwrap();
        let state = UpdateState {
            pending: Some(loom_api::PendingUpgrade {
                from: "0.1.0".to_owned(),
                to: "0.2.0".to_owned(),
                started_at: started,
            }),
            ..UpdateState::default()
        };
        let error = refuse_while_pending(&state, 1_060).unwrap_err().to_string();
        assert!(
            error.starts_with(
                "loomd is upgrading from 0.1.0 to 0.2.0 (started 1970-01-01T00:16:40Z)"
            ),
            "{error}"
        );
        assert!(refuse_while_pending(&state, 1_000 + 3_601).is_ok());
        assert!(refuse_while_pending(&UpdateState::default(), 1_060).is_ok());
    }

    #[test]
    fn skip_messages_are_distinct() {
        let current = Version::new(0, 2, 0);
        let pinned = Version::new(0, 1, 0);
        let messages = [
            skip_message(&SkipReason::UpdatesOff, &current),
            skip_message(
                &SkipReason::PinnedNotReleased {
                    pinned: pinned.clone(),
                },
                &current,
            ),
            skip_message(
                &SkipReason::PinnedBelowCurrent {
                    pinned: pinned.clone(),
                },
                &current,
            ),
            skip_message(&SkipReason::Failed { version: pinned }, &current),
        ];
        assert!(messages[2].contains("loom rollback"));
        for (i, a) in messages.iter().enumerate() {
            for b in &messages[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
