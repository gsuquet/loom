//! Atomic file writes.
//!
//! The new content goes to a temporary file beside the target, synced, then renamed over it,
//! and the directory is synced so that the rename survives a crash. A reader sees the old file
//! or the new one, never a part of either.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// Writes `contents` to `path` atomically, with `mode`.
///
/// # Errors
///
/// The I/O error of the write, the sync or the rename; `path` is unchanged then.
pub fn write_file(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    Staged::write(path, contents, mode)?.commit()
}

/// New content of a file, written and synced to its temporary file, not yet in place.
///
/// Dropped without [`Staged::commit`] (a crash), the target keeps its previous content and the
/// temporary file stays until the next write of the same target replaces it.
#[derive(Debug)]
#[must_use = "the content is in place only after `commit`"]
pub struct Staged {
    temporary: PathBuf,
    target: PathBuf,
}

impl Staged {
    /// Writes `contents` to the temporary file of `path` (`.<name>.tmp` in the same directory),
    /// with `mode`, and syncs it.
    ///
    /// # Errors
    ///
    /// The I/O error of the write or the sync.
    pub fn write(path: &Path, contents: &[u8], mode: u32) -> io::Result<Self> {
        let temporary = temporary_path(path)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&temporary)?;
        // A leftover temporary file keeps the mode it was created with.
        file.set_permissions(Permissions::from_mode(mode))?;
        file.write_all(contents)?;
        file.sync_all()?;
        Ok(Self {
            temporary,
            target: path.to_path_buf(),
        })
    }

    /// Renames the temporary file over the target and syncs their directory.
    ///
    /// # Errors
    ///
    /// The I/O error of the rename or the sync.
    pub fn commit(self) -> io::Result<()> {
        fs::rename(&self.temporary, &self.target)?;
        sync_parent(&self.target)
    }
}

/// Syncs the directory holding `path`, so that a rename or a creation in it is durable.
///
/// # Errors
///
/// The I/O error of the sync.
pub fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| no_name(path))?;
    sync_dir(parent)
}

/// Syncs the directory `dir`.
///
/// # Errors
///
/// The I/O error of the open or the sync.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// `.<name>.tmp` beside `path`: hidden, and never a task ID or a file loom reads.
fn temporary_path(path: &Path) -> io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| no_name(path))?;
    let mut temporary = OsString::from(".");
    temporary.push(name);
    temporary.push(".tmp");
    Ok(path.with_file_name(temporary))
}

fn no_name(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{} has no file name", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::{Staged, temporary_path, write_file};

    #[test]
    fn the_temporary_file_is_hidden_beside_the_target() {
        assert_eq!(
            temporary_path(Path::new("/s/tasks/k3q7zm2a/task.md")).unwrap(),
            Path::new("/s/tasks/k3q7zm2a/.task.md.tmp")
        );
        assert!(temporary_path(Path::new("/")).is_err());
    }

    #[test]
    fn a_write_replaces_the_file_and_leaves_no_temporary_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("task.md");
        write_file(&path, b"one", 0o600).unwrap();
        write_file(&path, b"two", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_staged_write_not_committed_leaves_the_file_as_it_was() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("task.md");
        write_file(&path, b"before", 0o600).unwrap();
        drop(Staged::write(&path, b"after", 0o600).unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"before");
        write_file(&path, b"later", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"later");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
