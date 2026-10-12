//! I/O of `loom service`: the service file and the service manager commands.
//!
//! Every command goes through a [`CommandRunner`] and every file under the paths of a
//! [`Target`], so tests run against a fake runner and a temporary directory.

use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use loom::service::{
    Manager, ManagerState, Retry, ServicePaths, Step, bootstrap_retry, display_argv, id_u,
    launchctl_print, launchd_state, parse_uid, plan_install, plan_uninstall, recover_commands,
    systemctl_is_active, systemctl_is_enabled, unit_serves_home,
};
use loom_api::{ServicePlatform, ServiceStatus};
use miette::{Context as _, IntoDiagnostic as _, Result, miette};

/// Mode of a directory created for the service (the log directory, and the loom home if it is
/// missing).
const DIR_MODE: u32 = 0o700;

/// Result of a finished command.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Output {
    /// Exit code; `None` when killed by a signal.
    pub code: Option<i32>,
    /// Standard output, lossily decoded.
    pub stdout: String,
    /// Standard error, lossily decoded.
    pub stderr: String,
}

impl Output {
    const fn success(&self) -> bool {
        matches!(self.code, Some(0))
    }
}

/// Runs external commands.
pub trait CommandRunner {
    /// Runs `argv` (program, then arguments) to completion, stdin closed.
    ///
    /// # Errors
    ///
    /// When the program cannot be started.
    fn run(&mut self, argv: &[String]) -> io::Result<Output>;

    /// Waits `duration` before a retry.
    fn sleep(&mut self, duration: Duration);
}

/// Runs commands on the host.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&mut self, argv: &[String]) -> io::Result<Output> {
        let (program, arguments) = argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
        let output = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .output()?;
        Ok(Output {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

/// What a `loom service` command acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Platform, chosen by the caller.
    pub platform: ServicePlatform,
    /// Service file and log directory.
    pub paths: ServicePaths,
}

/// `loom service install`: the stdout text.
pub fn install(
    target: &Target,
    desired: &str,
    runner: &mut impl CommandRunner,
    dry_run: bool,
) -> Result<String> {
    let manager = manager(target.platform, runner)?;
    let state = query_state(manager, runner)?;
    let current = read_current(&target.paths.unit)?;
    let steps = plan_install(manager, &target.paths, current.as_deref(), desired, state);
    let unit = target.paths.unit.display();
    if steps.is_empty() {
        return Ok(format!("loomd service already installed: {unit}\n"));
    }
    if dry_run {
        return Ok(describe(&steps));
    }
    execute(&steps, runner)?;
    Ok(format!("loomd service installed: {unit}\n"))
}

/// `loom service uninstall`: the stdout text.
pub fn uninstall(
    target: &Target,
    runner: &mut impl CommandRunner,
    dry_run: bool,
) -> Result<String> {
    let manager = manager(target.platform, runner)?;
    let state = query_state(manager, runner)?;
    let installed = exists(&target.paths.unit)?;
    let steps = plan_uninstall(manager, &target.paths, installed, state);
    let unit = target.paths.unit.display();
    if steps.is_empty() {
        return Ok(format!("loomd service not installed: {unit}\n"));
    }
    if dry_run {
        return Ok(describe(&steps));
    }
    execute(&steps, runner)?;
    Ok(format!("loomd service uninstalled: {unit}\n"))
}

/// `loom service status`.
pub fn status(target: &Target, runner: &mut impl CommandRunner) -> Result<ServiceStatus> {
    let manager = manager(target.platform, runner)?;
    let state = query_state(manager, runner)?;
    Ok(ServiceStatus {
        platform: target.platform,
        unit_path: target.paths.unit.clone(),
        installed: exists(&target.paths.unit)?,
        loaded: state.loaded,
        running: state.running,
    })
}

/// Who brings loomd back after the watchdog's rollback (ADR-0011).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervisor {
    /// The user service of this loom home is installed and loaded: its manager restarts loomd,
    /// and the watchdog never starts one itself.
    Service(Manager),
    /// No service for this loom home: the watchdog starts loomd detached.
    Unsupervised,
}

/// The supervisor of the loomd of the loom home `home`. No service file, or one that runs
/// loomd for another loom home: [`Supervisor::Unsupervised`], without running any command.
pub fn supervisor(
    target: &Target,
    home: &Path,
    runner: &mut impl CommandRunner,
) -> Result<Supervisor> {
    let Some(content) = read_current(&target.paths.unit)? else {
        return Ok(Supervisor::Unsupervised);
    };
    if !unit_serves_home(target.platform, &content, home).into_diagnostic()? {
        return Ok(Supervisor::Unsupervised);
    }
    let manager = manager(target.platform, runner)?;
    let state = query_state(manager, runner)?;
    Ok(if state.loaded {
        Supervisor::Service(manager)
    } else {
        Supervisor::Unsupervised
    })
}

/// Whether the watchdog starts loomd itself when nobody holds the lock: only without a
/// service manager. Under one, a loomd the watchdog started would hold the lock and make
/// every start of the service fail (launchd `KeepAlive` retrying forever, systemd failing
/// the unit), and loomd would end up outside its supervisor.
pub const fn starts_loomd_itself(supervisor: Supervisor) -> bool {
    match supervisor {
        Supervisor::Service(_) => false,
        Supervisor::Unsupervised => true,
    }
}

/// Makes `manager` run loomd again on the installed binaries ([`recover_commands`]); stops at
/// the first command that fails.
pub fn recover(manager: Manager, runner: &mut impl CommandRunner) -> Result<()> {
    for argv in recover_commands(manager) {
        run_checked(runner, &argv)?;
    }
    Ok(())
}

/// The manager of `platform`; on macOS, asks `id -u` for the launchd domain.
fn manager(platform: ServicePlatform, runner: &mut impl CommandRunner) -> Result<Manager> {
    match platform {
        ServicePlatform::Linux => Ok(Manager::Systemd),
        ServicePlatform::Macos => {
            let argv = id_u();
            let output = run_checked(runner, &argv)?;
            let uid = parse_uid(&output.stdout).ok_or_else(|| {
                miette!(
                    "`{}` printed {:?}, not a user ID",
                    display_argv(&argv),
                    output.stdout
                )
            })?;
            Ok(Manager::Launchd { uid })
        }
    }
}

/// Asks the service manager about the service. A query that exits non-zero means "no".
fn query_state(manager: Manager, runner: &mut impl CommandRunner) -> Result<ManagerState> {
    match manager {
        Manager::Launchd { uid } => {
            let output = spawn(runner, &launchctl_print(uid))?;
            Ok(launchd_state(output.success(), &output.stdout))
        }
        Manager::Systemd => Ok(ManagerState {
            loaded: spawn(runner, &systemctl_is_enabled())?.success(),
            running: spawn(runner, &systemctl_is_active())?.success(),
        }),
    }
}

/// Content of the service file, `None` when there is none. Invalid UTF-8 is replaced, so it
/// differs from any generated file.
fn read_current(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot read {}", path.display())),
    }
}

fn exists(path: &Path) -> Result<bool> {
    path.try_exists()
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot check {}", path.display()))
}

/// Dry-run output: the steps, in order.
fn describe(steps: &[Step]) -> String {
    steps.iter().map(ToString::to_string).collect()
}

/// Runs the steps in order and stops at the first failure.
fn execute(steps: &[Step], runner: &mut impl CommandRunner) -> Result<()> {
    for step in steps {
        match step {
            Step::CreateDir(path) => DirBuilder::new()
                .recursive(true)
                .mode(DIR_MODE)
                .create(path)
                .into_diagnostic()
                .wrap_err_with(|| format!("cannot create {}", path.display()))?,
            Step::Write { path, content } => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .into_diagnostic()
                        .wrap_err_with(|| format!("cannot create {}", parent.display()))?;
                }
                fs::write(path, content)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("cannot write {}", path.display()))?;
            }
            Step::Remove(path) => fs::remove_file(path)
                .into_diagnostic()
                .wrap_err_with(|| format!("cannot remove {}", path.display()))?,
            Step::Run(argv) => {
                run_checked(runner, argv)?;
            }
            Step::RunRetrying(argv) => run_retrying(runner, argv)?,
        }
    }
    Ok(())
}

/// Runs `argv` again while it exits non-zero, as [`bootstrap_retry`] decides. A command that
/// cannot be started is not retried. Fails with the last attempt's error.
fn run_retrying(runner: &mut impl CommandRunner, argv: &[String]) -> Result<()> {
    let mut attempt = 1;
    loop {
        let output = spawn(runner, argv)?;
        if output.success() {
            return Ok(());
        }
        match bootstrap_retry(attempt) {
            Retry::After(delay) => runner.sleep(delay),
            Retry::GiveUp => return Err(failure(argv, &output)),
        }
        attempt += 1;
    }
}

/// Runs `argv`; fails only when it cannot be started.
fn spawn(runner: &mut impl CommandRunner, argv: &[String]) -> Result<Output> {
    runner
        .run(argv)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot run `{}`", display_argv(argv)))
}

/// Runs `argv`; fails when it cannot be started or exits non-zero, with its stderr.
fn run_checked(runner: &mut impl CommandRunner, argv: &[String]) -> Result<Output> {
    let output = spawn(runner, argv)?;
    if output.success() {
        return Ok(output);
    }
    Err(failure(argv, &output))
}

/// Error for `argv` exiting non-zero: its command line, exit status and stderr.
fn failure(argv: &[String], output: &Output) -> miette::Report {
    let status = output.code.map_or_else(
        || "was killed by a signal".to_owned(),
        |code| format!("exited with code {code}"),
    );
    let stderr = output.stderr.trim();
    let detail = if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    };
    miette!("`{}` {status}{detail}", display_argv(argv))
}

#[cfg(test)]
mod tests {
    use super::{
        CommandRunner, Output, Supervisor, Target, install, recover, starts_loomd_itself, status,
        supervisor, uninstall,
    };
    use loom::service::{Manager, ServicePaths, ServiceSpec, render_unit};
    use loom_api::{ServicePlatform, ServiceStatus};
    use std::collections::HashMap;
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tempfile::TempDir;

    /// Records every command; answers from a table keyed by the command line, success with
    /// empty output otherwise.
    #[derive(Default)]
    struct FakeRunner {
        answers: HashMap<String, io::Result<Output>>,
        /// Answers used once each, in order, before `answers`.
        sequences: HashMap<String, Vec<Output>>,
        calls: Vec<String>,
        sleeps: Vec<Duration>,
    }

    impl FakeRunner {
        fn answer(mut self, command: &str, code: i32, stdout: &str, stderr: &str) -> Self {
            let output = Output {
                code: Some(code),
                stdout: stdout.to_owned(),
                stderr: stderr.to_owned(),
            };
            self.answers.insert(command.to_owned(), Ok(output));
            self
        }

        /// `command` exits `code` with `stderr` the next `times` times it runs.
        fn fail_times(mut self, command: &str, times: usize, code: i32, stderr: &str) -> Self {
            let output = Output {
                code: Some(code),
                stdout: String::new(),
                stderr: stderr.to_owned(),
            };
            self.sequences
                .insert(command.to_owned(), vec![output; times]);
            self
        }

        fn missing(mut self, command: &str) -> Self {
            self.answers
                .insert(command.to_owned(), Err(io::ErrorKind::NotFound.into()));
            self
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&mut self, argv: &[String]) -> io::Result<Output> {
            let command = argv.join(" ");
            self.calls.push(command.clone());
            if let Some(sequence) = self.sequences.get_mut(&command)
                && !sequence.is_empty()
            {
                return Ok(sequence.remove(0));
            }
            match self.answers.get(&command) {
                Some(Ok(output)) => Ok(output.clone()),
                Some(Err(error)) => Err(error.kind().into()),
                None => Ok(Output {
                    code: Some(0),
                    ..Output::default()
                }),
            }
        }

        fn sleep(&mut self, duration: Duration) {
            self.sleeps.push(duration);
        }
    }

    const ID: &str = "id -u";
    const PRINT: &str = "launchctl print gui/501/io.github.gsuquet.loomd";
    const BOOTOUT: &str = "launchctl bootout gui/501/io.github.gsuquet.loomd";
    const IS_ENABLED: &str = "systemctl --user is-enabled loomd.service";
    const IS_ACTIVE: &str = "systemctl --user is-active loomd.service";
    const RELOAD: &str = "systemctl --user daemon-reload";
    const ENABLE: &str = "systemctl --user enable --now loomd.service";
    const DISABLE: &str = "systemctl --user disable --now loomd.service";
    const RESTART: &str = "systemctl --user restart loomd.service";
    const RUNNING: &str = "gui/501/io.github.gsuquet.loomd = {\n\tstate = running\n}\n";

    struct Fixture {
        dir: TempDir,
        target: Target,
        desired: String,
    }

    impl Fixture {
        fn new(platform: ServicePlatform) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("loom home");
            let unit = match platform {
                ServicePlatform::Macos => dir
                    .path()
                    .join("Library/LaunchAgents/io.github.gsuquet.loomd.plist"),
                ServicePlatform::Linux => dir.path().join(".config/systemd/user/loomd.service"),
            };
            let spec = ServiceSpec {
                loomd: "/usr/local/bin/loomd".into(),
                home: home.clone(),
            };
            let desired = render_unit(platform, &spec).unwrap();
            let target = Target {
                platform,
                paths: ServicePaths {
                    unit,
                    log_dir: home.join("log"),
                },
            };
            Self {
                dir,
                target,
                desired,
            }
        }

        fn unit(&self) -> &Path {
            &self.target.paths.unit
        }

        fn log_dir(&self) -> PathBuf {
            self.target.paths.log_dir.clone()
        }

        fn bootstrap(&self) -> String {
            format!("launchctl bootstrap gui/501 {}", self.unit().display())
        }

        fn put_unit(&self, content: &str) {
            fs::create_dir_all(self.unit().parent().unwrap()).unwrap();
            fs::write(self.unit(), content).unwrap();
        }

        fn unit_content(&self) -> Option<String> {
            fs::read_to_string(self.unit()).ok()
        }
    }

    fn macos_runner(loaded: bool) -> FakeRunner {
        let runner = FakeRunner::default().answer(ID, 0, "501\n", "");
        if loaded {
            runner.answer(PRINT, 0, RUNNING, "")
        } else {
            runner.answer(PRINT, 113, "", "Could not find service")
        }
    }

    fn linux_runner(enabled: bool, active: bool) -> FakeRunner {
        FakeRunner::default()
            .answer(IS_ENABLED, i32::from(!enabled), "", "")
            .answer(IS_ACTIVE, if active { 0 } else { 3 }, "", "")
    }

    #[test]
    fn macos_fresh_install_writes_the_plist_and_bootstraps() {
        let fixture = Fixture::new(ServicePlatform::Macos);
        let mut runner = macos_runner(false);
        let out = install(&fixture.target, &fixture.desired, &mut runner, false).unwrap();
        assert_eq!(runner.calls, [ID, PRINT, &fixture.bootstrap()]);
        assert_eq!(fixture.unit_content().as_deref(), Some(&*fixture.desired));
        let log_dir = fs::metadata(fixture.log_dir()).unwrap();
        assert!(log_dir.is_dir());
        assert_eq!(log_dir.permissions().mode() & 0o777, 0o700);
        assert_eq!(
            out,
            format!("loomd service installed: {}\n", fixture.unit().display())
        );
    }

    #[test]
    fn macos_install_twice_is_a_no_op() {
        let fixture = Fixture::new(ServicePlatform::Macos);
        fixture.put_unit(&fixture.desired);
        let mut runner = macos_runner(true);
        let out = install(&fixture.target, &fixture.desired, &mut runner, false).unwrap();
        assert_eq!(runner.calls, [ID, PRINT]);
        assert_eq!(
            out,
            format!(
                "loomd service already installed: {}\n",
                fixture.unit().display()
            )
        );
    }

    #[test]
    fn macos_changed_plist_is_rewritten_and_reloaded() {
        let fixture = Fixture::new(ServicePlatform::Macos);
        fixture.put_unit("old");
        let mut runner = macos_runner(true);
        install(&fixture.target, &fixture.desired, &mut runner, false).unwrap();
        assert_eq!(runner.calls, [ID, PRINT, BOOTOUT, &fixture.bootstrap()]);
        assert_eq!(fixture.unit_content().as_deref(), Some(&*fixture.desired));
    }

    const IO_ERROR: &str = "Bootstrap failed: 5: Input/output error\n";

    #[test]
    fn bootstrap_after_bootout_succeeds_on_the_third_attempt() {
        let fixture = Fixture::new(ServicePlatform::Macos);
        fixture.put_unit("old");
        let bootstrap = fixture.bootstrap();
        let mut runner = macos_runner(true).fail_times(&bootstrap, 2, 5, IO_ERROR);
        install(&fixture.target, &fixture.desired, &mut runner, false).unwrap();
        assert_eq!(
            runner.calls,
            [ID, PRINT, BOOTOUT, &bootstrap, &bootstrap, &bootstrap]
        );
        assert_eq!(runner.sleeps, [Duration::from_millis(200); 2]);
    }

    #[test]
    fn bootstrap_after_bootout_gives_up_after_five_attempts() {
        let fixture = Fixture::new(ServicePlatform::Macos);
        fixture.put_unit("old");
        let bootstrap = fixture.bootstrap();
        let mut runner = macos_runner(true)
            .fail_times(&bootstrap, 4, 5, "first failures\n")
            .answer(&bootstrap, 5, "", IO_ERROR);
        let error = install(&fixture.target, &fixture.desired, &mut runner, false).unwrap_err();
        assert_eq!(runner.calls[3..], [&*bootstrap; 5]);
        assert_eq!(runner.calls.len(), 8);
        assert_eq!(runner.sleeps, [Duration::from_millis(200); 4]);
        let message = error.to_string();
        assert!(
            message.ends_with("exited with code 5: Bootstrap failed: 5: Input/output error"),
            "{message}"
        );
    }

    #[test]
    fn bootstrap_without_a_bootout_is_not_retried() {
        let fixture = Fixture::new(ServicePlatform::Macos);
        let bootstrap = fixture.bootstrap();
        let mut runner = macos_runner(false).fail_times(&bootstrap, 1, 5, IO_ERROR);
        install(&fixture.target, &fixture.desired, &mut runner, false).unwrap_err();
        assert_eq!(runner.calls, [ID, PRINT, &bootstrap]);
        assert_eq!(runner.sleeps, []);
    }

    #[test]
    fn linux_fresh_install_writes_the_unit_and_enables_it() {
        let fixture = Fixture::new(ServicePlatform::Linux);
        let mut runner = linux_runner(false, false);
        install(&fixture.target, &fixture.desired, &mut runner, false).unwrap();
        assert_eq!(runner.calls, [IS_ENABLED, IS_ACTIVE, RELOAD, ENABLE]);
        assert_eq!(fixture.unit_content().as_deref(), Some(&*fixture.desired));
        // systemd logs to the journal: no log directory.
        assert!(!fixture.log_dir().exists());
    }

    #[test]
    fn linux_install_twice_is_a_no_op() {
        let fixture = Fixture::new(ServicePlatform::Linux);
        fixture.put_unit(&fixture.desired);
        let mut runner = linux_runner(true, true);
        let out = install(&fixture.target, &fixture.desired, &mut runner, false).unwrap();
        assert_eq!(runner.calls, [IS_ENABLED, IS_ACTIVE]);
        assert!(
            out.starts_with("loomd service already installed: "),
            "{out}"
        );
    }

    #[test]
    fn linux_changed_unit_is_rewritten_and_restarted() {
        let fixture = Fixture::new(ServicePlatform::Linux);
        fixture.put_unit("old");
        let mut runner = linux_runner(true, true);
        install(&fixture.target, &fixture.desired, &mut runner, false).unwrap();
        assert_eq!(
            runner.calls,
            [IS_ENABLED, IS_ACTIVE, RELOAD, ENABLE, RESTART]
        );
        assert_eq!(fixture.unit_content().as_deref(), Some(&*fixture.desired));
    }

    #[test]
    fn install_dry_run_prints_the_plan_and_changes_nothing() {
        let fixture = Fixture::new(ServicePlatform::Macos);
        let mut runner = macos_runner(false);
        let out = install(&fixture.target, &fixture.desired, &mut runner, true).unwrap();
        assert_eq!(runner.calls, [ID, PRINT]);
        assert_eq!(fixture.unit_content(), None);
        assert!(!fixture.log_dir().exists());
        assert_eq!(
            out,
            format!(
                "write {}:\n{}create directory {}\nrun launchctl bootstrap gui/501 {}\n",
                fixture.unit().display(),
                fixture.desired,
                fixture.log_dir().display(),
                fixture.unit().display()
            )
        );
    }

    #[test]
    fn manager_command_failure_reports_its_stderr() {
        let fixture = Fixture::new(ServicePlatform::Macos);
        let bootstrap = fixture.bootstrap();
        let mut runner = macos_runner(false).answer(
            &bootstrap,
            5,
            "",
            "Bootstrap failed: 5: Input/output error\n",
        );
        let error = install(&fixture.target, &fixture.desired, &mut runner, false).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("launchctl bootstrap gui/501"), "{message}");
        assert!(message.contains("exited with code 5"), "{message}");
        assert!(
            message.ends_with(": Bootstrap failed: 5: Input/output error"),
            "{message}"
        );
    }

    #[test]
    fn missing_manager_is_an_error() {
        let fixture = Fixture::new(ServicePlatform::Linux);
        let mut runner = FakeRunner::default().missing(IS_ENABLED);
        let error = status(&fixture.target, &mut runner).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot run `systemctl --user is-enabled"),
            "{error}"
        );
    }

    #[test]
    fn unparsable_uid_is_an_error() {
        let fixture = Fixture::new(ServicePlatform::Macos);
        let mut runner = FakeRunner::default().answer(ID, 0, "nobody\n", "");
        let error = status(&fixture.target, &mut runner).unwrap_err();
        assert!(error.to_string().contains("not a user ID"), "{error}");
        assert_eq!(runner.calls, [ID]);
    }

    #[test]
    fn macos_uninstall_boots_out_then_removes_the_plist() {
        let fixture = Fixture::new(ServicePlatform::Macos);
        fixture.put_unit(&fixture.desired);
        let mut runner = macos_runner(true);
        let out = uninstall(&fixture.target, &mut runner, false).unwrap();
        assert_eq!(runner.calls, [ID, PRINT, BOOTOUT]);
        assert_eq!(fixture.unit_content(), None);
        assert_eq!(
            out,
            format!("loomd service uninstalled: {}\n", fixture.unit().display())
        );
    }

    #[test]
    fn linux_uninstall_disables_removes_and_reloads() {
        let fixture = Fixture::new(ServicePlatform::Linux);
        fixture.put_unit(&fixture.desired);
        let mut runner = linux_runner(true, true);
        uninstall(&fixture.target, &mut runner, false).unwrap();
        assert_eq!(runner.calls, [IS_ENABLED, IS_ACTIVE, DISABLE, RELOAD]);
        assert_eq!(fixture.unit_content(), None);
    }

    #[test]
    fn uninstall_without_a_service_is_a_no_op() {
        for platform in [ServicePlatform::Macos, ServicePlatform::Linux] {
            let fixture = Fixture::new(platform);
            let mut runner = match platform {
                ServicePlatform::Macos => macos_runner(false),
                ServicePlatform::Linux => linux_runner(false, false),
            };
            let out = uninstall(&fixture.target, &mut runner, false).unwrap();
            assert_eq!(runner.calls.len(), 2, "{platform:?}: queries only");
            assert!(out.starts_with("loomd service not installed: "), "{out}");
        }
    }

    #[test]
    fn uninstall_dry_run_changes_nothing() {
        let fixture = Fixture::new(ServicePlatform::Linux);
        fixture.put_unit(&fixture.desired);
        let mut runner = linux_runner(true, true);
        let out = uninstall(&fixture.target, &mut runner, true).unwrap();
        assert_eq!(runner.calls, [IS_ENABLED, IS_ACTIVE]);
        assert_eq!(fixture.unit_content().as_deref(), Some(&*fixture.desired));
        assert_eq!(
            out,
            format!(
                "run {DISABLE}\nremove {}\nrun {RELOAD}\n",
                fixture.unit().display()
            )
        );
    }

    // --- Supervisor of the watchdog ------------------------------------------------------

    impl Fixture {
        fn home(&self) -> PathBuf {
            self.dir.path().join("loom home")
        }
    }

    const KICKSTART: &str = "launchctl kickstart -k gui/501/io.github.gsuquet.loomd";
    const RESET_FAILED: &str = "systemctl --user reset-failed loomd.service";

    #[test]
    fn installed_and_loaded_service_supervises_and_recovers_with_exact_commands() {
        let fixture = Fixture::new(ServicePlatform::Macos);
        fixture.put_unit(&fixture.desired);
        let mut runner = macos_runner(true);
        let found = supervisor(&fixture.target, &fixture.home(), &mut runner).unwrap();
        assert_eq!(found, Supervisor::Service(Manager::Launchd { uid: 501 }));
        assert!(
            !starts_loomd_itself(found),
            "no detached start under launchd"
        );
        recover(Manager::Launchd { uid: 501 }, &mut runner).unwrap();
        assert_eq!(runner.calls, [ID, PRINT, KICKSTART]);

        let fixture = Fixture::new(ServicePlatform::Linux);
        fixture.put_unit(&fixture.desired);
        // A failed unit (start limit hit) is still enabled.
        let mut runner = linux_runner(true, false);
        let found = supervisor(&fixture.target, &fixture.home(), &mut runner).unwrap();
        assert_eq!(found, Supervisor::Service(Manager::Systemd));
        assert!(
            !starts_loomd_itself(found),
            "no detached start under systemd"
        );
        recover(Manager::Systemd, &mut runner).unwrap();
        assert_eq!(runner.calls, [IS_ENABLED, IS_ACTIVE, RESET_FAILED, RESTART]);
    }

    #[test]
    fn without_a_service_of_this_home_the_watchdog_starts_loomd_itself() {
        for platform in [ServicePlatform::Macos, ServicePlatform::Linux] {
            // No service file: no command at all.
            let fixture = Fixture::new(platform);
            let mut runner = FakeRunner::default();
            let found = supervisor(&fixture.target, &fixture.home(), &mut runner).unwrap();
            assert_eq!(found, Supervisor::Unsupervised, "{platform:?}");
            assert!(starts_loomd_itself(found));
            assert_eq!(runner.calls, Vec::<String>::new(), "{platform:?}");

            // A service of another loom home does not supervise this one.
            fixture.put_unit(&fixture.desired);
            let other = fixture.dir.path().join("other home");
            let found = supervisor(&fixture.target, &other, &mut runner).unwrap();
            assert_eq!(found, Supervisor::Unsupervised, "{platform:?}");
            assert_eq!(runner.calls, Vec::<String>::new(), "{platform:?}");

            // Installed but not loaded.
            let mut runner = match platform {
                ServicePlatform::Macos => macos_runner(false),
                ServicePlatform::Linux => linux_runner(false, false),
            };
            let found = supervisor(&fixture.target, &fixture.home(), &mut runner).unwrap();
            assert_eq!(found, Supervisor::Unsupervised, "{platform:?}");
            assert_eq!(runner.calls.len(), 2, "{platform:?}: queries only");
        }
    }

    #[test]
    fn a_failing_recovery_command_stops_and_reports_it() {
        let mut runner = FakeRunner::default().answer(RESET_FAILED, 1, "", "Unit not loaded.\n");
        let error = recover(Manager::Systemd, &mut runner).unwrap_err();
        assert_eq!(runner.calls, [RESET_FAILED]);
        assert!(
            error
                .to_string()
                .ends_with("exited with code 1: Unit not loaded."),
            "{error}"
        );
    }

    #[test]
    fn status_reports_file_and_manager_state() {
        let fixture = Fixture::new(ServicePlatform::Macos);
        fixture.put_unit(&fixture.desired);
        let mut runner = macos_runner(true);
        assert_eq!(
            status(&fixture.target, &mut runner).unwrap(),
            ServiceStatus {
                platform: ServicePlatform::Macos,
                unit_path: fixture.unit().to_path_buf(),
                installed: true,
                loaded: true,
                running: true,
            }
        );

        let fixture = Fixture::new(ServicePlatform::Linux);
        let mut runner = linux_runner(true, false);
        let reported = status(&fixture.target, &mut runner).unwrap();
        assert_eq!(
            (reported.installed, reported.loaded, reported.running),
            (false, true, false)
        );
        assert_eq!(runner.calls, [IS_ENABLED, IS_ACTIVE]);
        drop(fixture.dir);
    }
}
