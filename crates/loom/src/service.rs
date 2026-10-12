//! `loom service`: the launchd property list and the systemd unit of `loomd`, the commands
//! that load and unload them, and the plans that install and uninstall the service.
//!
//! No I/O: `main.rs` reads the current state, runs the plan and the commands.

use loom_config::layout::Layout;
use std::ffi::OsStr;
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use loom_api::{ServicePlatform, ServiceStatus};
use thiserror::Error;

/// launchd label of the service.
pub const LAUNCHD_LABEL: &str = "io.github.gsuquet.loomd";
/// systemd unit name of the service.
pub const SYSTEMD_UNIT: &str = "loomd.service";

/// Path of the service file: `~/Library/LaunchAgents/io.github.gsuquet.loomd.plist` on macOS,
/// `$XDG_CONFIG_HOME/systemd/user/loomd.service` on Linux.
///
/// `XDG_CONFIG_HOME` is used when it is an absolute path (XDG Base Directory rules), otherwise
/// `~/.config`: systemd looks for user units there.
#[must_use]
pub fn unit_path(
    platform: ServicePlatform,
    user_home: &Path,
    xdg_config_home: Option<&OsStr>,
) -> PathBuf {
    match platform {
        ServicePlatform::Macos => user_home
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist")),
        ServicePlatform::Linux => xdg_config_home
            .map(Path::new)
            .filter(|dir| dir.is_absolute())
            .map_or_else(|| user_home.join(".config"), Path::to_path_buf)
            .join("systemd/user")
            .join(SYSTEMD_UNIT),
    }
}

/// `<loom home>/log` ([`Layout::log_dir`]): directory of the service log on macOS.
#[must_use]
pub fn log_dir(home: &Path) -> PathBuf {
    Layout::new(home.to_path_buf()).log_dir
}

/// `<loom home>/log/loomd.log` ([`Layout::daemon_log`]): stdout and stderr of the service on
/// macOS, the same file `loom status` appends to when it starts loomd.
#[must_use]
pub fn log_file(home: &Path) -> PathBuf {
    Layout::new(home.to_path_buf()).daemon_log
}

/// What the service runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Absolute path of the `loomd` executable.
    pub loomd: PathBuf,
    /// Absolute path of the loom home, given to `loomd` as `LOOM_HOME`.
    pub home: PathBuf,
}

/// A path that cannot be written in a service file.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RenderError {
    /// The path is not valid UTF-8.
    #[error("{} is not valid UTF-8: it cannot be written in a service file", .0.display())]
    NotUtf8(PathBuf),
    /// The path contains a control character, such as a newline.
    #[error("{0:?} contains a control character: it cannot be written in a service file")]
    ControlCharacter(PathBuf),
}

/// Content of the service file for `platform`.
///
/// # Errors
///
/// [`RenderError`] when a path of `spec` is not UTF-8 or contains a control character.
pub fn render_unit(platform: ServicePlatform, spec: &ServiceSpec) -> Result<String, RenderError> {
    match platform {
        ServicePlatform::Macos => render_plist(spec),
        ServicePlatform::Linux => render_systemd_unit(spec),
    }
}

/// launchd property list of the service. Every interpolated value is XML-escaped.
///
/// # Errors
///
/// [`RenderError`] when a path of `spec` is not UTF-8 or contains a control character.
pub fn render_plist(spec: &ServiceSpec) -> Result<String, RenderError> {
    let loomd = xml_escape(text(&spec.loomd)?);
    let home = xml_escape(text(&spec.home)?);
    let log = xml_escape(text(&log_file(&spec.home))?);
    let label = xml_escape(LAUNCHD_LABEL);
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{loomd}</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>LOOM_HOME</key>
		<string>{home}</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ProcessType</key>
	<string>Background</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    ))
}

/// systemd user unit of the service.
///
/// Values are double-quoted and escaped (systemd.syntax(7), systemd.service(5) "Command
/// lines"). `KillMode=process`: when loomd exits after an auto-upgrade, systemd stops only
/// loomd, not the watchdog it started (ADR-0011).
///
/// # Errors
///
/// [`RenderError`] when a path of `spec` is not UTF-8 or contains a control character.
pub fn render_systemd_unit(spec: &ServiceSpec) -> Result<String, RenderError> {
    let exec_start = systemd_quote(text(&spec.loomd)?, true);
    let environment = systemd_quote(&format!("LOOM_HOME={}", text(&spec.home)?), false);
    Ok(format!(
        "[Unit]
Description=Loom daemon

[Service]
ExecStart={exec_start}
Environment={environment}
Restart=always
KillMode=process

[Install]
WantedBy=default.target
"
    ))
}

/// `path` as UTF-8 without control characters.
fn text(path: &Path) -> Result<&str, RenderError> {
    let text = path
        .to_str()
        .ok_or_else(|| RenderError::NotUtf8(path.to_path_buf()))?;
    if text.chars().any(char::is_control) {
        return Err(RenderError::ControlCharacter(path.to_path_buf()));
    }
    Ok(text)
}

fn xml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// `text` as one double-quoted systemd word. `\` and `"` are backslash-escaped (C-style
/// unescaping), `%` is doubled (specifiers), and in a command line `$` is doubled (variable
/// expansion; `Environment=` does not expand it).
fn systemd_quote(text: &str, command_line: bool) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '%' => quoted.push_str("%%"),
            '$' if command_line => quoted.push_str("$$"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// Service manager of the platform, with what its commands need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    /// launchd, in the GUI domain of the user `uid`.
    Launchd {
        /// User ID.
        uid: u32,
    },
    /// systemd, user instance.
    Systemd,
}

impl Manager {
    /// Platform this manager runs on.
    #[must_use]
    pub const fn platform(self) -> ServicePlatform {
        match self {
            Self::Launchd { .. } => ServicePlatform::Macos,
            Self::Systemd => ServicePlatform::Linux,
        }
    }
}

/// What the service manager reports about the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ManagerState {
    /// Loaded in launchd, or enabled in systemd.
    pub loaded: bool,
    /// The service process is running.
    pub running: bool,
}

/// One action of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Create a directory and its missing parents (mode `0700`).
    CreateDir(PathBuf),
    /// Write a file, creating its missing parent directories.
    Write {
        /// File to write.
        path: PathBuf,
        /// New content.
        content: String,
    },
    /// Remove a file.
    Remove(PathBuf),
    /// Run a service manager command; a non-zero exit fails the plan.
    Run(Vec<String>),
    /// Run a service manager command, retried while it exits non-zero as
    /// [`bootstrap_retry`] decides; the last failure fails the plan. Used for the
    /// `launchctl bootstrap` that follows a `bootout`: launchd may still be tearing the
    /// service down and answer `Bootstrap failed: 5: Input/output error`.
    RunRetrying(Vec<String>),
}

/// Attempts of a [`Step::RunRetrying`] command, the first included.
pub const BOOTSTRAP_ATTEMPTS: u32 = 5;
/// Wait between two attempts of a [`Step::RunRetrying`] command.
pub const BOOTSTRAP_RETRY_DELAY: Duration = Duration::from_millis(200);

/// What to do after an attempt of a [`Step::RunRetrying`] command exited non-zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Wait this long, then try again.
    After(Duration),
    /// Stop and report the last failure.
    GiveUp,
}

/// Retry policy after the failed attempt number `attempt` (1-based): retry after
/// [`BOOTSTRAP_RETRY_DELAY`] until [`BOOTSTRAP_ATTEMPTS`] attempts have failed.
#[must_use]
pub const fn bootstrap_retry(attempt: u32) -> Retry {
    if attempt < BOOTSTRAP_ATTEMPTS {
        Retry::After(BOOTSTRAP_RETRY_DELAY)
    } else {
        Retry::GiveUp
    }
}

/// One line per step, the content of a written file after its line.
impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateDir(path) => writeln!(f, "create directory {}", path.display()),
            Self::Write { path, content } => {
                writeln!(f, "write {}:", path.display())?;
                f.write_str(content)
            }
            Self::Remove(path) => writeln!(f, "remove {}", path.display()),
            Self::Run(argv) | Self::RunRetrying(argv) => {
                writeln!(f, "run {}", display_argv(argv))
            }
        }
    }
}

/// `argv` joined with spaces, an argument with spaces or quotes single-quoted as in a shell.
#[must_use]
pub fn display_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            let plain = !arg.is_empty()
                && arg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
            if plain {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Paths a plan touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePaths {
    /// Service file ([`unit_path`]). Must be valid UTF-8: it is passed to `launchctl`.
    pub unit: PathBuf,
    /// Log directory ([`log_dir`]), created before launchd opens the log file.
    pub log_dir: PathBuf,
}

/// Steps that install the service with the file content `desired`, given the `current` file
/// content (`None`: no file) and the manager state.
///
/// - Identical file, already loaded: nothing.
/// - Identical file, not loaded: load it.
/// - Missing or different file: write it, then (re)load. launchd: `bootout` if loaded, then
///   `bootstrap` (retried after a `bootout`, see [`Step::RunRetrying`]). systemd: `daemon-reload`, `enable --now`, and `restart` if it was running.
#[must_use]
pub fn plan_install(
    manager: Manager,
    paths: &ServicePaths,
    current: Option<&str>,
    desired: &str,
    state: ManagerState,
) -> Vec<Step> {
    let up_to_date = current == Some(desired);
    if up_to_date && state.loaded {
        return Vec::new();
    }
    let mut steps = Vec::new();
    if !up_to_date {
        steps.push(Step::Write {
            path: paths.unit.clone(),
            content: desired.to_owned(),
        });
    }
    match manager {
        Manager::Launchd { uid } => {
            if state.loaded {
                steps.push(Step::Run(launchctl_bootout(uid)));
            }
            steps.push(Step::CreateDir(paths.log_dir.clone()));
            let bootstrap = launchctl_bootstrap(uid, &paths.unit);
            // Only right after a `bootout` can launchd still be unloading the service.
            steps.push(if state.loaded {
                Step::RunRetrying(bootstrap)
            } else {
                Step::Run(bootstrap)
            });
        }
        Manager::Systemd => {
            steps.push(Step::Run(systemctl(&["daemon-reload"])));
            steps.push(Step::Run(systemctl(&["enable", "--now", SYSTEMD_UNIT])));
            if !up_to_date && state.running {
                steps.push(Step::Run(systemctl(&["restart", SYSTEMD_UNIT])));
            }
        }
    }
    steps
}

/// Steps that uninstall the service: stop and unload it if the manager knows it, then remove
/// the file if `installed`. systemd reloads its units after the removal. Nothing installed or
/// loaded: nothing.
#[must_use]
pub fn plan_uninstall(
    manager: Manager,
    paths: &ServicePaths,
    installed: bool,
    state: ManagerState,
) -> Vec<Step> {
    let mut steps = Vec::new();
    match manager {
        Manager::Launchd { uid } => {
            if state.loaded {
                steps.push(Step::Run(launchctl_bootout(uid)));
            }
            if installed {
                steps.push(Step::Remove(paths.unit.clone()));
            }
        }
        Manager::Systemd => {
            if state.loaded || state.running {
                steps.push(Step::Run(systemctl(&["disable", "--now", SYSTEMD_UNIT])));
            }
            if installed {
                steps.push(Step::Remove(paths.unit.clone()));
                steps.push(Step::Run(systemctl(&["daemon-reload"])));
            }
        }
    }
    steps
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|&arg| arg.to_owned()).collect()
}

fn systemctl(args: &[&str]) -> Vec<String> {
    let mut command = argv(&["systemctl", "--user"]);
    command.extend(argv(args));
    command
}

fn launchd_service(uid: u32) -> String {
    format!("gui/{uid}/{LAUNCHD_LABEL}")
}

/// `id -u`: user ID, for the launchd domain.
#[must_use]
pub fn id_u() -> Vec<String> {
    argv(&["id", "-u"])
}

/// User ID printed by `id -u`.
#[must_use]
pub fn parse_uid(stdout: &str) -> Option<u32> {
    stdout.trim().parse().ok()
}

/// `launchctl print gui/<uid>/<label>`: succeeds when the service is loaded.
#[must_use]
pub fn launchctl_print(uid: u32) -> Vec<String> {
    vec![
        "launchctl".to_owned(),
        "print".to_owned(),
        launchd_service(uid),
    ]
}

fn launchctl_bootstrap(uid: u32, plist: &Path) -> Vec<String> {
    vec![
        "launchctl".to_owned(),
        "bootstrap".to_owned(),
        format!("gui/{uid}"),
        plist.display().to_string(),
    ]
}

fn launchctl_bootout(uid: u32) -> Vec<String> {
    vec![
        "launchctl".to_owned(),
        "bootout".to_owned(),
        launchd_service(uid),
    ]
}

/// `systemctl --user is-enabled loomd.service`: succeeds when the unit is enabled.
#[must_use]
pub fn systemctl_is_enabled() -> Vec<String> {
    systemctl(&["is-enabled", SYSTEMD_UNIT])
}

/// `systemctl --user is-active loomd.service`: succeeds when the unit is running.
#[must_use]
pub fn systemctl_is_active() -> Vec<String> {
    systemctl(&["is-active", SYSTEMD_UNIT])
}

/// launchd state from `launchctl print`: loaded when it succeeded, running when its output
/// has `state = running`.
#[must_use]
pub fn launchd_state(print_succeeded: bool, print_stdout: &str) -> ManagerState {
    ManagerState {
        loaded: print_succeeded,
        running: print_succeeded
            && print_stdout
                .lines()
                .any(|line| line.trim() == "state = running"),
    }
}

/// Human output of `loom service status`.
#[must_use]
pub fn format_status(status: &ServiceStatus) -> String {
    let yes_no = |value: bool| if value { "yes" } else { "no" };
    let mut out = String::new();
    let _ = writeln!(out, "platform:  {}", status.platform.as_str());
    let _ = writeln!(out, "unit:      {}", status.unit_path.display());
    let _ = writeln!(out, "installed: {}", yes_no(status.installed));
    let _ = writeln!(out, "loaded:    {}", yes_no(status.loaded));
    let _ = writeln!(out, "running:   {}", yes_no(status.running));
    out
}

/// Message of `loom service install` when `[daemon] service = false`.
#[must_use]
pub fn opt_out_message(config: &Path) -> String {
    format!(
        "the loomd service is disabled by `[daemon] service = false` in {}: set it to true or remove it to install the service",
        config.display()
    )
}

#[cfg(test)]
mod tests {
    use super::{
        BOOTSTRAP_RETRY_DELAY, Manager, ManagerState, RenderError, Retry, ServicePaths,
        ServiceSpec, Step, bootstrap_retry, display_argv, format_status, launchctl_print,
        launchd_state, log_file, opt_out_message, parse_uid, plan_install, plan_uninstall,
        render_plist, render_systemd_unit, render_unit, systemctl_is_active, systemctl_is_enabled,
        unit_path,
    };
    use loom_api::{ServicePlatform, ServiceStatus};
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    const MACOS_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>io.github.gsuquet.loomd</string>
	<key>ProgramArguments</key>
	<array>
		<string>/usr/local/bin/loomd</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>LOOM_HOME</key>
		<string>/Users/me/.loom</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ProcessType</key>
	<string>Background</string>
	<key>StandardOutPath</key>
	<string>/Users/me/.loom/log/loomd.log</string>
	<key>StandardErrorPath</key>
	<string>/Users/me/.loom/log/loomd.log</string>
</dict>
</plist>
"#;

    const LINUX_UNIT: &str = r#"[Unit]
Description=Loom daemon

[Service]
ExecStart="/usr/local/bin/loomd"
Environment="LOOM_HOME=/home/me/.loom"
Restart=always
KillMode=process

[Install]
WantedBy=default.target
"#;

    fn spec(loomd: &str, home: &str) -> ServiceSpec {
        ServiceSpec {
            loomd: loomd.into(),
            home: home.into(),
        }
    }

    #[test]
    fn plist_snapshot() {
        assert_eq!(
            render_plist(&spec("/usr/local/bin/loomd", "/Users/me/.loom")).unwrap(),
            MACOS_PLIST
        );
    }

    #[test]
    fn plist_escapes_xml_specials() {
        let plist = render_plist(&spec("/opt/a&b <c>/loomd", "/Users/me/it's \"x\"")).unwrap();
        assert!(
            plist.contains("<string>/opt/a&amp;b &lt;c&gt;/loomd</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>/Users/me/it&apos;s &quot;x&quot;</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>/Users/me/it&apos;s &quot;x&quot;/log/loomd.log</string>"),
            "{plist}"
        );
    }

    #[test]
    fn systemd_unit_snapshot() {
        assert_eq!(
            render_systemd_unit(&spec("/usr/local/bin/loomd", "/home/me/.loom")).unwrap(),
            LINUX_UNIT
        );
    }

    #[test]
    fn systemd_unit_quotes_and_escapes_specials() {
        let unit = render_systemd_unit(&spec(
            r#"/opt/my apps/$x%i\"q"/loomd"#,
            r"/home/me/50% $HOME\dir",
        ))
        .unwrap();
        assert!(
            unit.contains("ExecStart=\"/opt/my apps/$$x%%i\\\\\\\"q\\\"/loomd\"\n"),
            "{unit}"
        );
        assert!(
            unit.contains("Environment=\"LOOM_HOME=/home/me/50%% $HOME\\\\dir\"\n"),
            "{unit}"
        );
    }

    #[test]
    fn render_unit_picks_the_platform_format() {
        let spec = spec("/usr/local/bin/loomd", "/home/me/.loom");
        assert!(
            render_unit(ServicePlatform::Macos, &spec)
                .unwrap()
                .starts_with("<?xml")
        );
        assert!(
            render_unit(ServicePlatform::Linux, &spec)
                .unwrap()
                .starts_with("[Unit]")
        );
    }

    #[test]
    fn control_characters_are_rejected() {
        let bad = spec("/opt/a\nb/loomd", "/home/me/.loom");
        for platform in [ServicePlatform::Macos, ServicePlatform::Linux] {
            assert_eq!(
                render_unit(platform, &bad),
                Err(RenderError::ControlCharacter(PathBuf::from(
                    "/opt/a\nb/loomd"
                )))
            );
        }
    }

    #[test]
    fn non_utf8_paths_are_rejected() {
        use std::os::unix::ffi::OsStrExt as _;
        let home = PathBuf::from(OsStr::from_bytes(b"/home/\xff"));
        let bad = ServiceSpec {
            loomd: "/usr/bin/loomd".into(),
            home: home.clone(),
        };
        assert_eq!(render_plist(&bad), Err(RenderError::NotUtf8(home.clone())));
        assert_eq!(render_systemd_unit(&bad), Err(RenderError::NotUtf8(home)));
    }

    #[test]
    fn unit_paths() {
        let home = Path::new("/Users/me");
        assert_eq!(
            unit_path(ServicePlatform::Macos, home, Some(OsStr::new("/x"))),
            Path::new("/Users/me/Library/LaunchAgents/io.github.gsuquet.loomd.plist")
        );
        let home = Path::new("/home/me");
        assert_eq!(
            unit_path(ServicePlatform::Linux, home, None),
            Path::new("/home/me/.config/systemd/user/loomd.service")
        );
        assert_eq!(
            unit_path(ServicePlatform::Linux, home, Some(OsStr::new("/xdg"))),
            Path::new("/xdg/systemd/user/loomd.service")
        );
        // Relative or empty: ignored, as the XDG Base Directory specification requires.
        for ignored in ["", "rel/config"] {
            assert_eq!(
                unit_path(ServicePlatform::Linux, home, Some(OsStr::new(ignored))),
                Path::new("/home/me/.config/systemd/user/loomd.service"),
                "{ignored:?}"
            );
        }
    }

    #[test]
    fn log_file_is_under_the_loom_home() {
        assert_eq!(log_file(Path::new("/h")), Path::new("/h/log/loomd.log"));
    }

    // --- Plans ---------------------------------------------------------------------------

    const UID: u32 = 501;
    const LAUNCHD: Manager = Manager::Launchd { uid: UID };
    const DESIRED: &str = "desired\n";

    fn paths() -> ServicePaths {
        ServicePaths {
            unit: "/u/unit file".into(),
            log_dir: "/h/log".into(),
        }
    }

    fn run(argv: &str) -> Step {
        Step::Run(argv.split(' ').map(str::to_owned).collect())
    }

    fn write() -> Step {
        Step::Write {
            path: paths().unit,
            content: DESIRED.to_owned(),
        }
    }

    fn bootstrap() -> Step {
        Step::Run(vec![
            "launchctl".into(),
            "bootstrap".into(),
            "gui/501".into(),
            "/u/unit file".into(),
        ])
    }

    fn bootstrap_retrying() -> Step {
        match bootstrap() {
            Step::Run(argv) => Step::RunRetrying(argv),
            other => other,
        }
    }

    fn create_log_dir() -> Step {
        Step::CreateDir("/h/log".into())
    }

    const BOOTOUT: &str = "launchctl bootout gui/501/io.github.gsuquet.loomd";
    const RELOAD: &str = "systemctl --user daemon-reload";
    const ENABLE: &str = "systemctl --user enable --now loomd.service";
    const DISABLE: &str = "systemctl --user disable --now loomd.service";
    const RESTART: &str = "systemctl --user restart loomd.service";

    /// Current file content: none, identical to `DESIRED`, different.
    #[derive(Debug, Clone, Copy)]
    enum Current {
        Missing,
        Same,
        Different,
    }

    impl Current {
        const fn content(self) -> Option<&'static str> {
            match self {
                Self::Missing => None,
                Self::Same => Some(DESIRED),
                Self::Different => Some("old\n"),
            }
        }
    }

    const fn state(loaded: bool, running: bool) -> ManagerState {
        ManagerState { loaded, running }
    }

    #[test]
    fn launchd_install_plans() {
        use Current::{Different, Missing, Same};
        let fresh = || vec![write(), create_log_dir(), bootstrap()];
        let reload = || {
            vec![
                write(),
                run(BOOTOUT),
                create_log_dir(),
                bootstrap_retrying(),
            ]
        };
        let cases: Vec<(Current, ManagerState, Vec<Step>)> = vec![
            (Missing, state(false, false), fresh()),
            (Missing, state(false, true), fresh()),
            (Missing, state(true, false), reload()),
            (Missing, state(true, true), reload()),
            (
                Same,
                state(false, false),
                vec![create_log_dir(), bootstrap()],
            ),
            (
                Same,
                state(false, true),
                vec![create_log_dir(), bootstrap()],
            ),
            (Same, state(true, false), vec![]),
            (Same, state(true, true), vec![]),
            (Different, state(false, false), fresh()),
            (Different, state(false, true), fresh()),
            (Different, state(true, false), reload()),
            (Different, state(true, true), reload()),
        ];
        for (current, state, expected) in cases {
            assert_eq!(
                plan_install(LAUNCHD, &paths(), current.content(), DESIRED, state),
                expected,
                "{current:?} {state:?}"
            );
        }
    }

    #[test]
    fn systemd_install_plans() {
        use Current::{Different, Missing, Same};
        let load = || vec![run(RELOAD), run(ENABLE)];
        let fresh = || vec![write(), run(RELOAD), run(ENABLE)];
        let restart = || vec![write(), run(RELOAD), run(ENABLE), run(RESTART)];
        let cases: Vec<(Current, ManagerState, Vec<Step>)> = vec![
            (Missing, state(false, false), fresh()),
            (Missing, state(false, true), restart()),
            (Missing, state(true, false), fresh()),
            (Missing, state(true, true), restart()),
            (Same, state(false, false), load()),
            (Same, state(false, true), load()),
            (Same, state(true, false), vec![]),
            (Same, state(true, true), vec![]),
            (Different, state(false, false), fresh()),
            (Different, state(false, true), restart()),
            (Different, state(true, false), fresh()),
            (Different, state(true, true), restart()),
        ];
        for (current, state, expected) in cases {
            assert_eq!(
                plan_install(
                    Manager::Systemd,
                    &paths(),
                    current.content(),
                    DESIRED,
                    state
                ),
                expected,
                "{current:?} {state:?}"
            );
        }
    }

    #[test]
    fn launchd_uninstall_plans() {
        let remove = || Step::Remove(paths().unit);
        let cases: Vec<(bool, ManagerState, Vec<Step>)> = vec![
            (false, state(false, false), vec![]),
            (false, state(false, true), vec![]),
            (false, state(true, false), vec![run(BOOTOUT)]),
            (false, state(true, true), vec![run(BOOTOUT)]),
            (true, state(false, false), vec![remove()]),
            (true, state(false, true), vec![remove()]),
            (true, state(true, false), vec![run(BOOTOUT), remove()]),
            (true, state(true, true), vec![run(BOOTOUT), remove()]),
        ];
        for (installed, state, expected) in cases {
            assert_eq!(
                plan_uninstall(LAUNCHD, &paths(), installed, state),
                expected,
                "installed={installed} {state:?}"
            );
        }
    }

    #[test]
    fn systemd_uninstall_plans() {
        let remove = || Step::Remove(paths().unit);
        let cases: Vec<(bool, ManagerState, Vec<Step>)> = vec![
            (false, state(false, false), vec![]),
            (false, state(false, true), vec![run(DISABLE)]),
            (false, state(true, false), vec![run(DISABLE)]),
            (false, state(true, true), vec![run(DISABLE)]),
            (true, state(false, false), vec![remove(), run(RELOAD)]),
            (
                true,
                state(false, true),
                vec![run(DISABLE), remove(), run(RELOAD)],
            ),
            (
                true,
                state(true, false),
                vec![run(DISABLE), remove(), run(RELOAD)],
            ),
            (
                true,
                state(true, true),
                vec![run(DISABLE), remove(), run(RELOAD)],
            ),
        ];
        for (installed, state, expected) in cases {
            assert_eq!(
                plan_uninstall(Manager::Systemd, &paths(), installed, state),
                expected,
                "installed={installed} {state:?}"
            );
        }
    }

    // --- Commands and their output -------------------------------------------------------

    #[test]
    fn query_commands() {
        assert_eq!(
            launchctl_print(UID),
            ["launchctl", "print", "gui/501/io.github.gsuquet.loomd"]
        );
        assert_eq!(
            systemctl_is_enabled(),
            ["systemctl", "--user", "is-enabled", "loomd.service"]
        );
        assert_eq!(
            systemctl_is_active(),
            ["systemctl", "--user", "is-active", "loomd.service"]
        );
    }

    #[test]
    fn uid_parsing() {
        assert_eq!(parse_uid("501\n"), Some(501));
        assert_eq!(parse_uid(" 0 "), Some(0));
        assert_eq!(parse_uid(""), None);
        assert_eq!(parse_uid("me"), None);
    }

    #[test]
    fn launchd_state_from_print() {
        let running = "gui/501/io.github.gsuquet.loomd = {\n\tactive count = 1\n\tstate = running\n\tpid = 42\n}\n";
        let waiting = "gui/501/io.github.gsuquet.loomd = {\n\tstate = not running\n}\n";
        assert_eq!(launchd_state(true, running), state(true, true));
        assert_eq!(launchd_state(true, waiting), state(true, false));
        assert_eq!(launchd_state(false, running), state(false, false));
        assert_eq!(launchd_state(false, ""), state(false, false));
    }

    #[test]
    fn steps_display() {
        assert_eq!(create_log_dir().to_string(), "create directory /h/log\n");
        assert_eq!(write().to_string(), "write /u/unit file:\ndesired\n");
        assert_eq!(Step::Remove("/u/x".into()).to_string(), "remove /u/x\n");
        assert_eq!(
            bootstrap().to_string(),
            "run launchctl bootstrap gui/501 '/u/unit file'\n"
        );
    }

    #[test]
    fn bootstrap_is_retried_four_times_after_the_first_failure() {
        for attempt in 1..=4 {
            assert_eq!(
                bootstrap_retry(attempt),
                Retry::After(BOOTSTRAP_RETRY_DELAY),
                "{attempt}"
            );
        }
        assert_eq!(BOOTSTRAP_RETRY_DELAY.as_millis(), 200);
        assert_eq!(bootstrap_retry(5), Retry::GiveUp);
        assert_eq!(bootstrap_retry(6), Retry::GiveUp);
    }

    #[test]
    fn retrying_step_displays_as_a_run() {
        assert_eq!(bootstrap_retrying().to_string(), bootstrap().to_string());
    }

    #[test]
    fn argv_display_quotes_like_a_shell() {
        let argv = ["a", "b c", "it's", "", "x=1"].map(str::to_owned);
        assert_eq!(display_argv(&argv), r"a 'b c' 'it'\''s' '' x=1");
    }

    #[test]
    fn status_human_output() {
        let status = ServiceStatus {
            platform: ServicePlatform::Linux,
            unit_path: "/home/me/.config/systemd/user/loomd.service".into(),
            installed: true,
            loaded: true,
            running: false,
        };
        assert_eq!(
            format_status(&status),
            "platform:  linux\n\
             unit:      /home/me/.config/systemd/user/loomd.service\n\
             installed: yes\n\
             loaded:    yes\n\
             running:   no\n"
        );
    }

    #[test]
    fn opt_out_message_names_the_key_and_the_file() {
        let message = opt_out_message(Path::new("/home/me/.loom/config.toml"));
        assert!(message.contains("[daemon] service = false"), "{message}");
        assert!(message.contains("/home/me/.loom/config.toml"), "{message}");
    }
}
