//! The forge token of a credential.
//!
//! `token_cmd` runs through `sh -c`, `token_env` is read from loomd's environment. The token is
//! kept in memory until a 401 asks for it again. Never written, never logged, never given to
//! git or to an agent.

use std::collections::HashMap;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use loom_api::TokenProblem;
use loom_config::forges::TokenSource;
use thiserror::Error;
use tracing::{debug, info_span};

use super::Token;
use crate::repo_update::{Failure, wait};

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
    command
        .split_whitespace()
        .find(|word| !word.contains('='))
        .unwrap_or_default()
        .trim_matches(['"', '\''])
        .to_owned()
}

/// Reads an environment variable of loomd.
type EnvLookup = dyn Fn(&str) -> Option<String> + Send + Sync;

/// Resolves tokens and keeps them in memory, by source: two contexts with the same source run
/// its command once.
pub struct TokenResolver {
    timeout: Duration,
    env: Box<EnvLookup>,
    cache: Mutex<HashMap<TokenSource, Token>>,
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

    /// The token of `source`: from memory, otherwise resolved and kept.
    ///
    /// # Errors
    ///
    /// [`TokenError`] when the command fails, times out or writes nothing, or the variable is
    /// unset or empty. A failure is not kept: the next call tries again.
    pub fn resolve(&self, source: &TokenSource) -> Result<Token, TokenError> {
        if let Some(token) = self.cached(source) {
            return Ok(token);
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
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(source.clone(), token.clone());
        Ok(token)
    }

    /// Forgets the token of `source`, after the forge refused it (401).
    pub fn invalidate(&self, source: &TokenSource) {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(source);
    }

    fn cached(&self, source: &TokenSource) -> Option<Token> {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(source)
            .cloned()
    }

    /// Runs `sh -c <command>` with no input, in its own process group; its standard output,
    /// trimmed, is the token.
    fn run(&self, command: &str) -> Result<Token, TokenError> {
        let span = info_span!("forge.token_cmd", program = program(command).as_str());
        let _entered = span.enter();
        let child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(TokenError::Spawn)?;
        match wait(child, self.timeout, KILL_GRACE) {
            Ok(stdout) => {
                let token = stdout.trim();
                if token.is_empty() {
                    return Err(TokenError::EmptyCommand);
                }
                debug!("token_cmd gave a token");
                Ok(Token::new(token.to_owned()))
            }
            Err(Failure::Exit {
                status,
                stderr_tail,
            }) if status.code() == Some(COMMAND_NOT_FOUND) => Err(TokenError::CommandNotFound {
                program: program(command),
                stderr: stderr_tail.trim().to_owned(),
            }),
            Err(Failure::Exit {
                status,
                stderr_tail,
            }) => Err(TokenError::CommandFailed {
                status: status.to_string(),
                stderr: stderr_tail.trim().to_owned(),
            }),
            Err(Failure::Timeout { after, .. }) => Err(TokenError::Timeout(after)),
            Err(Failure::Run(error)) => Err(TokenError::Spawn(error)),
        }
    }
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
        assert_eq!(program("'my tool' x"), "my");
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
        let source = command(&format!("echo run >> '{}'; echo tok", runs.display()));
        let resolver = resolver(Duration::from_secs(10));
        let count = || fs::read_to_string(&runs).unwrap().lines().count();
        resolver.resolve(&source).unwrap();
        resolver.resolve(&source).unwrap();
        assert_eq!(count(), 1);
        resolver.invalidate(&source);
        resolver.resolve(&source).unwrap();
        assert_eq!(count(), 2);
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
