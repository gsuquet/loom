//! The forge token of a credential.
//!
//! `token_cmd` runs through `sh -c`, `token_env` is read from loomd's environment. The token is
//! kept in memory until a 401 asks for it again. Never written, never logged, never given to
//! git or to an agent.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::TokenProblem;
use loom_config::forges::TokenSource;
use thiserror::Error;
use tracing::{debug, info_span, warn};

use super::Token;
use crate::repo_update::{STDERR_TAIL_BYTES, stop_group};

/// Longest run of a `token_cmd`.
pub const TOKEN_CMD_TIMEOUT: Duration = Duration::from_secs(10);
/// Wait between `SIGTERM` and `SIGKILL` of a `token_cmd` that timed out.
const KILL_GRACE: Duration = Duration::from_secs(1);
/// Exit status of `sh` when it cannot find the program.
const COMMAND_NOT_FOUND: i32 = 127;

/// Why a token cannot be resolved. Never contains the token.
#[derive(Debug, Error)]
pub enum TokenError {
    /// `sh` cannot find the program of `token_cmd`.
    #[error("`{program}` is not found in the PATH of loomd: {stderr}")]
    CommandNotFound {
        /// The first word of `token_cmd`.
        program: String,
        /// The end of its standard error.
        stderr: String,
    },
    /// `token_cmd` exited with a non-zero status.
    #[error("token_cmd failed ({status}): {stderr}")]
    CommandFailed {
        /// Its exit status.
        status: String,
        /// The end of its standard error.
        stderr: String,
    },
    /// `token_cmd` could not start.
    #[error("cannot run token_cmd: {0}")]
    Spawn(std::io::Error),
    /// `token_cmd` did not end in time.
    #[error(
        "token_cmd did not end within {}s (does it wait for an unlock, such as 1Password?)",
        .0.as_secs()
    )]
    Timeout(Duration),
    /// `token_cmd` wrote nothing on its standard output.
    #[error("token_cmd wrote no token on its standard output")]
    EmptyCommand,
    /// The variable of `token_env` is empty.
    #[error("the environment variable `{0}` of loomd is empty")]
    EmptyEnv(String),
    /// The variable of `token_env` is not set.
    #[error(
        "the environment variable `{0}` is not set for loomd (a service does not see the variables of your shell: use token_cmd)"
    )]
    EnvUnset(String),
}

impl TokenError {
    /// Its kind, as the API reports it.
    #[must_use]
    pub const fn problem(&self) -> TokenProblem {
        match self {
            Self::CommandNotFound { .. } => TokenProblem::CommandNotFound,
            Self::CommandFailed { .. } | Self::Spawn(_) => TokenProblem::CommandFailed,
            Self::Timeout(_) => TokenProblem::Timeout,
            Self::EmptyCommand | Self::EmptyEnv(_) => TokenProblem::Empty,
            Self::EnvUnset(_) => TokenProblem::EnvUnset,
        }
    }

    /// The program that was not found, for [`TokenError::CommandNotFound`].
    #[must_use]
    pub fn program(&self) -> Option<&str> {
        match self {
            Self::CommandNotFound { program, .. } => Some(program),
            _ => None,
        }
    }
}

/// The first word of a `token_cmd`, the program `sh` looks for. Pure.
#[must_use]
pub fn program(command: &str) -> String {
    let mut rest = command.trim_start();
    // Variable assignments before the program, such as `GH_HOST=x gh`.
    loop {
        let word = rest.split_whitespace().next().unwrap_or_default();
        if word.contains('=') && !word.starts_with(['"', '\'']) {
            rest = rest[word.len()..].trim_start();
        } else {
            break;
        }
    }
    match rest.chars().next() {
        Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next().unwrap_or_default().to_owned(),
        _ => rest
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned(),
    }
}

/// Reads an environment variable of loomd.
type EnvLookup = dyn Fn(&str) -> Option<String> + Send + Sync;

/// Resolves tokens and keeps them in memory, by source: two contexts with the same source run
/// its command once, even at the same time.
pub struct TokenResolver {
    timeout: Duration,
    env: Box<EnvLookup>,
    cache: Mutex<HashMap<TokenSource, Arc<Mutex<Option<Token>>>>>,
}

impl std::fmt::Debug for TokenResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResolver")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Default for TokenResolver {
    /// loomd's environment, [`TOKEN_CMD_TIMEOUT`].
    fn default() -> Self {
        Self::new(TOKEN_CMD_TIMEOUT, Box::new(|name| std::env::var(name).ok()))
    }
}

impl TokenResolver {
    /// A resolver that stops a command after `timeout` and reads variables with `env`.
    #[must_use]
    pub fn new(timeout: Duration, env: Box<EnvLookup>) -> Self {
        Self {
            timeout,
            env,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The token of `source`: from memory, otherwise resolved and kept. Callers that ask for
    /// the same source at the same time wait for one run of its command.
    ///
    /// # Errors
    ///
    /// [`TokenError`] when the command fails, times out or writes nothing, or the variable is
    /// unset or empty. A failure is not kept: the next call tries again.
    pub fn resolve(&self, source: &TokenSource) -> Result<Token, TokenError> {
        let slot = self.slot(source);
        let mut kept = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(token) = kept.as_ref() {
            return Ok(token.clone());
        }
        let token = match source {
            TokenSource::Command(command) => self.run(command)?,
            TokenSource::Env(name) => {
                let value = (self.env)(name).ok_or_else(|| TokenError::EnvUnset(name.clone()))?;
                let value = value.trim();
                if value.is_empty() {
                    return Err(TokenError::EmptyEnv(name.clone()));
                }
                Token::new(value.to_owned())
            }
        };
        *kept = Some(token.clone());
        drop(kept);
        Ok(token)
    }

    /// Forgets the token of `source` after the forge refused `refused` (401). A token resolved
    /// since then by another caller is kept.
    pub fn invalidate(&self, source: &TokenSource, refused: &Token) {
        let slot = self.slot(source);
        let mut kept = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if kept.as_ref() == Some(refused) {
            *kept = None;
        }
    }

    fn slot(&self, source: &TokenSource) -> Arc<Mutex<Option<Token>>> {
        Arc::clone(
            self.cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(source.clone())
                .or_default(),
        )
    }

    /// Runs `sh -c <command>` with no input, in its own process group; its standard output,
    /// trimmed, is the token.
    fn run(&self, command: &str) -> Result<Token, TokenError> {
        let span = info_span!("forge.token_cmd", program = program(command).as_str());
        let _entered = span.enter();
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(TokenError::Spawn)?;
        let stdout = child.stdout.take().map(chunks);
        let stderr = child.stderr.take().map(chunks);
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(None) => {
                    stop_group(&mut child, KILL_GRACE);
                    return Err(TokenError::Timeout(self.timeout));
                }
                Err(error) => {
                    stop_group(&mut child, KILL_GRACE);
                    return Err(TokenError::Spawn(error));
                }
            }
        };
        // A process the command left behind may keep the pipes open: take what arrived.
        let stdout = stdout
            .map(|received| collect(&received))
            .unwrap_or_default();
        let stderr = stderr
            .map(|received| collect(&received))
            .unwrap_or_default();
        let stderr = tail(&stderr);
        if status.code() == Some(COMMAND_NOT_FOUND) {
            return Err(TokenError::CommandNotFound {
                program: program(command),
                stderr,
            });
        }
        if !status.success() {
            return Err(TokenError::CommandFailed {
                status: status.to_string(),
                stderr,
            });
        }
        let token = String::from_utf8_lossy(&stdout).trim().to_owned();
        if token.is_empty() {
            return Err(TokenError::EmptyCommand);
        }
        debug!("token_cmd gave a token");
        Ok(Token::new(token))
    }
}

/// Wait for more output after the command exited.
const OUTPUT_GRACE: Duration = Duration::from_millis(200);

/// Reads `pipe` on a thread; each chunk arrives on the channel as soon as it is read.
fn chunks(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (send, received) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("token-cmd-output".to_owned())
        .spawn(move || {
            let mut buffer = [0_u8; 4096];
            while let Ok(read @ 1..) = pipe.read(&mut buffer) {
                if send.send(buffer[..read].to_vec()).is_err() {
                    break;
                }
            }
        });
    if let Err(error) = spawned {
        warn!(%error, "cannot start a thread to read the output of token_cmd");
    }
    received
}

/// The chunks received until the pipe closes, or until no chunk came for [`OUTPUT_GRACE`].
fn collect(received: &mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Ok(chunk) = received.recv_timeout(OUTPUT_GRACE) {
        bytes.extend(chunk);
    }
    bytes
}

/// The last [`STDERR_TAIL_BYTES`] of `bytes`, trimmed.
fn tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(STDERR_TAIL_BYTES);
    String::from_utf8_lossy(&bytes[start..]).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use loom_api::TokenProblem;
    use loom_config::forges::TokenSource;

    use super::{TokenError, TokenResolver, program};

    fn resolver(timeout: Duration) -> TokenResolver {
        TokenResolver::new(
            timeout,
            Box::new(|name| match name {
                "SET" => Some(" env-token\n".to_owned()),
                "BLANK" => Some("  ".to_owned()),
                _ => None,
            }),
        )
    }

    fn command(text: &str) -> TokenSource {
        TokenSource::Command(text.to_owned())
    }

    #[test]
    fn programs_of_commands() {
        assert_eq!(program("gh auth token --hostname github.com"), "gh");
        assert_eq!(
            program("/opt/homebrew/bin/op read op://x/y"),
            "/opt/homebrew/bin/op"
        );
        assert_eq!(program("  GH_HOST=x gh auth token"), "gh");
        assert_eq!(
            program("'/Applications/My Tool/op' read x"),
            "/Applications/My Tool/op"
        );
        assert_eq!(program("\"gh\" auth token"), "gh");
    }

    #[test]
    fn a_command_gives_its_trimmed_output() {
        let token = resolver(Duration::from_secs(10))
            .resolve(&command("printf '  tok-1\\n'"))
            .unwrap();
        assert_eq!(token.expose(), "tok-1");
    }

    #[test]
    fn a_variable_gives_its_trimmed_value() {
        let resolver = resolver(Duration::from_secs(10));
        let token = resolver
            .resolve(&TokenSource::Env("SET".to_owned()))
            .unwrap();
        assert_eq!(token.expose(), "env-token");
        let unset = resolver
            .resolve(&TokenSource::Env("UNSET".to_owned()))
            .unwrap_err();
        assert_eq!(unset.problem(), TokenProblem::EnvUnset);
        let blank = resolver
            .resolve(&TokenSource::Env("BLANK".to_owned()))
            .unwrap_err();
        assert_eq!(blank.problem(), TokenProblem::Empty);
    }

    #[test]
    fn a_failing_command_reports_its_status_and_error_output() {
        let error = resolver(Duration::from_secs(10))
            .resolve(&command("echo 'not logged in' >&2; exit 3"))
            .unwrap_err();
        assert_eq!(error.problem(), TokenProblem::CommandFailed);
        let message = error.to_string();
        assert!(message.contains("exit status: 3"), "{message}");
        assert!(message.contains("not logged in"), "{message}");
    }

    #[test]
    fn a_missing_program_is_named() {
        let error = resolver(Duration::from_secs(10))
            .resolve(&command("loom-no-such-program-xyz auth token"))
            .unwrap_err();
        assert_eq!(error.problem(), TokenProblem::CommandNotFound);
        assert_eq!(error.program(), Some("loom-no-such-program-xyz"));
    }

    #[test]
    fn an_empty_output_is_no_token() {
        let error = resolver(Duration::from_secs(10))
            .resolve(&command("printf '\\n  \\n'"))
            .unwrap_err();
        assert!(matches!(error, TokenError::EmptyCommand));
    }

    #[test]
    fn a_slow_command_is_stopped() {
        let started = Instant::now();
        let error = resolver(Duration::from_millis(200))
            .resolve(&command("sleep 30"))
            .unwrap_err();
        assert_eq!(error.problem(), TokenProblem::Timeout);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_token_is_kept_until_invalidated() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        // A new token at each run: tok-1, tok-2…
        let source = command(&format!(
            "echo run >> '{0}'; echo tok-$(wc -l < '{0}' | tr -d ' ')",
            runs.display()
        ));
        let resolver = resolver(Duration::from_secs(10));
        let count = || fs::read_to_string(&runs).unwrap().lines().count();
        let first = resolver.resolve(&source).unwrap();
        resolver.resolve(&source).unwrap();
        assert_eq!(count(), 1);
        resolver.invalidate(&source, &first);
        let second = resolver.resolve(&source).unwrap();
        assert_eq!(count(), 2);
        // A refusal of the old token does not drop the new one.
        resolver.invalidate(&source, &first);
        assert_eq!(resolver.resolve(&source).unwrap(), second);
        assert_eq!(count(), 2);
    }

    #[test]
    fn callers_at_the_same_time_wait_for_one_run() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        let source = command(&format!(
            "echo run >> '{}'; sleep 0.3; echo tok",
            runs.display()
        ));
        let resolver = resolver(Duration::from_secs(10));
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| assert_eq!(resolver.resolve(&source).unwrap().expose(), "tok"));
            }
        });
        assert_eq!(fs::read_to_string(&runs).unwrap().lines().count(), 1);
    }

    #[test]
    fn a_process_left_behind_does_not_lose_the_token() {
        let started = Instant::now();
        let token = resolver(Duration::from_secs(10))
            .resolve(&command("echo tok; sleep 5 &"))
            .unwrap();
        assert_eq!(token.expose(), "tok");
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn a_failure_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join("ready");
        let source = command(&format!("cat '{}'", flag.display()));
        let resolver = resolver(Duration::from_secs(10));
        assert!(resolver.resolve(&source).is_err());
        fs::write(&flag, "tok\n").unwrap();
        assert_eq!(resolver.resolve(&source).unwrap().expose(), "tok");
    }
}
