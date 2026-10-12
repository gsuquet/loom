//! `<loom home>/state/update.json`: read, and changed under a lock with an atomic write.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use loom_api::UpdateState;
use loom_config::layout::{Layout, STATE_DIR_MODE, STATE_FILE_MODE};
use tempfile::NamedTempFile;

/// Why the state could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Filesystem error.
    #[error("{action} {}: {source}", path.display())]
    Io {
        /// What failed.
        action: &'static str,
        /// Path involved.
        path: PathBuf,
        /// Cause.
        #[source]
        source: io::Error,
    },
    /// The file is not a valid state.
    #[error("{} is not a valid update state: {source}", path.display())]
    Invalid {
        /// Path of the file.
        path: PathBuf,
        /// Cause.
        #[source]
        source: serde_json::Error,
    },
}

fn io_error(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> StateError + use<> {
    let path = path.to_path_buf();
    move |source| StateError::Io {
        action,
        path,
        source,
    }
}

/// Reads the state; a missing file is an empty state.
///
/// # Errors
///
/// [`StateError`] when the file cannot be read or is not a valid state.
pub fn read(path: &Path) -> Result<UpdateState, StateError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| StateError::Invalid {
            path: path.to_path_buf(),
            source,
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(UpdateState::default()),
        Err(error) => Err(io_error("cannot read", path)(error)),
    }
}

/// Changes the state with `change` under a lock, and writes it atomically.
///
/// The lock is an exclusive `flock` on `state/update.lock`. An invalid file is replaced:
/// `on_invalid` receives the error, and `change` an empty state. Returns the state written.
///
/// # Errors
///
/// [`StateError::Io`] when the directory, the lock or the file cannot be written.
pub fn modify(
    layout: &Layout,
    on_invalid: impl FnOnce(&StateError),
    change: impl FnOnce(UpdateState) -> UpdateState,
) -> Result<UpdateState, StateError> {
    create_state_dir(&layout.state_dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(STATE_FILE_MODE)
        .open(&layout.update_lock)
        .map_err(io_error("cannot open", &layout.update_lock))?;
    // Released when `lock` is dropped.
    lock.lock()
        .map_err(io_error("cannot lock", &layout.update_lock))?;
    let current = match read(&layout.update_state) {
        Ok(state) => state,
        Err(error @ StateError::Invalid { .. }) => {
            on_invalid(&error);
            UpdateState::default()
        }
        Err(error) => return Err(error),
    };
    let next = change(current);
    write(&layout.state_dir, &layout.update_state, &next)?;
    Ok(next)
}

/// Writes `state` to `path` through a temporary file in `dir`, synced, then renamed.
fn write(dir: &Path, path: &Path, state: &UpdateState) -> Result<(), StateError> {
    let mut text = serde_json::to_vec_pretty(state).map_err(|source| StateError::Invalid {
        path: path.to_path_buf(),
        source,
    })?;
    text.push(b'\n');
    let mut temp = tempfile::Builder::new()
        .prefix(".update.")
        .permissions(Permissions::from_mode(STATE_FILE_MODE))
        .tempfile_in(dir)
        .map_err(io_error("cannot write to", dir))?;
    temp.write_all(&text)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(io_error("cannot write", temp.path()))?;
    persist(temp, path)?;
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(io_error("cannot sync", dir))
}

fn persist(temp: NamedTempFile, path: &Path) -> Result<(), StateError> {
    temp.persist(path)
        .map(drop)
        .map_err(|error| io_error("cannot replace", path)(error.error))
}

/// Creates `state/` (mode 0700) and restricts an existing one to 0700.
fn create_state_dir(dir: &Path) -> Result<(), StateError> {
    DirBuilder::new()
        .recursive(true)
        .mode(STATE_DIR_MODE)
        .create(dir)
        .and_then(|()| fs::set_permissions(dir, Permissions::from_mode(STATE_DIR_MODE)))
        .map_err(io_error("cannot create", dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().join("home"));
        (dir, layout)
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn missing_file_is_an_empty_state() {
        let (_dir, layout) = layout();
        assert_eq!(read(&layout.update_state).unwrap(), UpdateState::default());
    }

    #[test]
    fn modify_writes_atomically_with_private_modes() {
        let (_dir, layout) = layout();
        let written = modify(
            &layout,
            |_| panic!("valid"),
            |mut state| {
                state.failed_versions.push("0.2.0".to_owned());
                state
            },
        )
        .unwrap();
        assert_eq!(read(&layout.update_state).unwrap(), written);
        assert_eq!(mode(&layout.state_dir), 0o700);
        assert_eq!(mode(&layout.update_state), 0o600);
        assert_eq!(mode(&layout.update_lock), 0o600);
        // Read-modify-write keeps what was there.
        let again = modify(&layout, |_| panic!("valid"), |state| state).unwrap();
        assert_eq!(again.failed_versions, ["0.2.0"]);
        let leftovers = fs::read_dir(&layout.state_dir).unwrap().count();
        assert_eq!(leftovers, 2, "update.json and update.lock only");
    }

    #[test]
    fn invalid_file_is_reported_then_replaced() {
        let (_dir, layout) = layout();
        fs::create_dir_all(&layout.state_dir).unwrap();
        fs::write(&layout.update_state, "not json").unwrap();
        assert!(matches!(
            read(&layout.update_state),
            Err(StateError::Invalid { .. })
        ));
        let mut reported = false;
        let state = modify(&layout, |_| reported = true, |state| state).unwrap();
        assert!(reported);
        assert_eq!(state, UpdateState::default());
        assert_eq!(read(&layout.update_state).unwrap(), state);
    }
}
