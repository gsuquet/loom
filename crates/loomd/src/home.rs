//! Resolution of the loom home directory (`LOOM_HOME`).

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// Environment variable naming the loom home directory.
pub const LOOM_HOME_ENV: &str = "LOOM_HOME";
/// Name of the default loom home directory, inside the user's home directory.
pub const DEFAULT_HOME_DIR: &str = ".loom";

/// Why the loom home directory cannot be resolved.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HomeError {
    /// `--loom-home` was given an empty value.
    #[error("--loom-home is empty: pass a directory")]
    EmptyFlag,
    /// No source gave a directory.
    #[error(
        "cannot find the loom home directory: --loom-home is not given, LOOM_HOME is unset or empty, and the user's home directory is unknown"
    )]
    Unresolved,
}

/// Resolves the loom home directory.
///
/// Order: `flag` (`--loom-home`), then `env` (`LOOM_HOME`) when non-empty, then
/// `~/.loom` from `user_home`. A relative result is made absolute against `cwd`.
///
/// # Errors
///
/// [`HomeError::EmptyFlag`] when `flag` is empty, [`HomeError::Unresolved`] when no source
/// gives a directory.
pub fn resolve_home(
    flag: Option<&Path>,
    env: Option<&OsStr>,
    user_home: Option<&Path>,
    cwd: &Path,
) -> Result<PathBuf, HomeError> {
    let home = match (flag, env.filter(|value| !value.is_empty()), user_home) {
        (Some(flag), _, _) if flag.as_os_str().is_empty() => return Err(HomeError::EmptyFlag),
        (Some(flag), _, _) => flag.to_path_buf(),
        (None, Some(env), _) => PathBuf::from(env),
        (None, None, Some(user_home)) if !user_home.as_os_str().is_empty() => {
            user_home.join(DEFAULT_HOME_DIR)
        }
        (None, None, _) => return Err(HomeError::Unresolved),
    };
    Ok(absolute(&home, cwd))
}

/// `path` made absolute against `cwd`, without `.` components. No filesystem access, so
/// symbolic links and `..` are kept as given.
fn absolute(path: &Path, cwd: &Path) -> PathBuf {
    cwd.join(path)
        .components()
        .filter(|component| *component != Component::CurDir)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{HomeError, resolve_home};
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    const CWD: &str = "/work";

    fn resolve(
        flag: Option<&str>,
        env: Option<&str>,
        user_home: Option<&str>,
    ) -> Result<PathBuf, HomeError> {
        resolve_home(
            flag.map(Path::new),
            env.map(OsStr::new),
            user_home.map(Path::new),
            Path::new(CWD),
        )
    }

    #[test]
    fn flag_wins_over_env_and_user_home() {
        assert_eq!(
            resolve(Some("/flag"), Some("/env"), Some("/home/me")),
            Ok(PathBuf::from("/flag"))
        );
    }

    #[test]
    fn env_wins_over_user_home() {
        assert_eq!(
            resolve(None, Some("/env"), Some("/home/me")),
            Ok(PathBuf::from("/env"))
        );
    }

    #[test]
    fn empty_env_is_ignored() {
        assert_eq!(
            resolve(None, Some(""), Some("/home/me")),
            Ok(PathBuf::from("/home/me/.loom"))
        );
    }

    #[test]
    fn defaults_to_dot_loom_in_the_user_home() {
        assert_eq!(
            resolve(None, None, Some("/home/me")),
            Ok(PathBuf::from("/home/me/.loom"))
        );
    }

    #[test]
    fn relative_paths_are_made_absolute_against_cwd() {
        assert_eq!(
            resolve(Some("data/loom"), None, None),
            Ok(PathBuf::from("/work/data/loom"))
        );
        assert_eq!(
            resolve(None, Some("./loom"), None),
            Ok(PathBuf::from("/work/loom"))
        );
        assert_eq!(
            resolve(None, None, Some("me")),
            Ok(PathBuf::from("/work/me/.loom"))
        );
    }

    #[test]
    fn empty_flag_is_an_error() {
        assert_eq!(
            resolve(Some(""), Some("/env"), Some("/home/me")),
            Err(HomeError::EmptyFlag)
        );
    }

    #[test]
    fn fails_when_nothing_resolves() {
        assert_eq!(resolve(None, None, None), Err(HomeError::Unresolved));
        assert_eq!(
            resolve(None, Some(""), Some("")),
            Err(HomeError::Unresolved)
        );
        let message = HomeError::Unresolved.to_string();
        assert!(message.contains("LOOM_HOME"), "{message}");
        assert!(message.contains("--loom-home"), "{message}");
    }
}
