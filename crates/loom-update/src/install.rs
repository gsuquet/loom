//! Installation of `loom` and `loomd` into the install directory, with same-directory
//! renames, and bookkeeping of the previous versions.

use std::fs::{self, DirBuilder, File, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use semver::Version;
use tempfile::NamedTempFile;

/// Binaries installed and kept, in the order they are replaced.
pub const BINARIES: [&str; 2] = ["loomd", "loom"];
/// Mode of the installed binaries.
const BINARY_MODE: u32 = 0o755;
/// Mode of the directories loom creates in its home.
pub const DIR_MODE: u32 = 0o700;

/// An I/O error with what was being done.
#[derive(Debug, thiserror::Error)]
#[error("{action} {}: {source}", path.display())]
pub struct IoError {
    /// What failed, e.g. "cannot write".
    pub action: &'static str,
    /// Path involved.
    pub path: PathBuf,
    /// Cause.
    #[source]
    pub source: io::Error,
}

fn io_error(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> IoError + use<> {
    let path = path.to_path_buf();
    move |source| IoError {
        action,
        path,
        source,
    }
}

/// Creates `dir` and its missing parents with mode 0700.
///
/// # Errors
///
/// [`IoError`].
pub fn create_private_dir(dir: &Path) -> Result<(), IoError> {
    DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(dir)
        .map_err(io_error("cannot create", dir))
}

/// Checks that files can be created in `dir`, by creating and removing a temporary file.
///
/// # Errors
///
/// [`IoError`] when `dir` is not writable.
pub fn check_writable(dir: &Path) -> Result<(), IoError> {
    NamedTempFile::new_in(dir)
        .map(drop)
        .map_err(io_error("cannot write to", dir))
}

/// Replaces `loomd`, then `loom`, in `install_dir` with the files of `source_dir`.
///
/// Both new files are first copied next to their targets (same directory, so same
/// filesystem), synced and made executable; only then is each renamed over its target. A
/// failure before the renames leaves the installed binaries untouched. A missing `loomd` in
/// `install_dir` is created.
///
/// # Errors
///
/// [`IoError`].
pub fn replace_binaries(install_dir: &Path, source_dir: &Path) -> Result<(), IoError> {
    let mut staged = Vec::new();
    for name in BINARIES {
        let source = source_dir.join(name);
        let mut temp = NamedTempFile::with_prefix_in(format!(".{name}.upgrade."), install_dir)
            .map_err(io_error("cannot write to", install_dir))?;
        let mut input = File::open(&source).map_err(io_error("cannot read", &source))?;
        io::copy(&mut input, temp.as_file_mut()).map_err(io_error("cannot write", temp.path()))?;
        temp.as_file()
            .set_permissions(Permissions::from_mode(BINARY_MODE))
            .map_err(io_error("cannot set the mode of", temp.path()))?;
        temp.as_file()
            .sync_all()
            .map_err(io_error("cannot sync", temp.path()))?;
        staged.push((name, temp));
    }
    for (name, temp) in staged {
        let target = install_dir.join(name);
        temp.persist(&target)
            .map_err(|error| io_error("cannot replace", &target)(error.error))?;
    }
    sync_dir(install_dir)
}

/// Copies the installed `loom` and `loomd` into `versions_dir/<version>/`, replacing an older
/// save of the same version. Returns the directory. A missing installed `loomd` is skipped.
///
/// The copy is made in a temporary directory inside `versions_dir`, then renamed into place.
///
/// # Errors
///
/// [`IoError`].
pub fn save_installed(
    install_dir: &Path,
    versions_dir: &Path,
    version: &Version,
) -> Result<PathBuf, IoError> {
    create_private_dir(versions_dir)?;
    let temp = tempfile::Builder::new()
        .prefix(".saving.")
        .permissions(Permissions::from_mode(DIR_MODE))
        .tempdir_in(versions_dir)
        .map_err(io_error("cannot write to", versions_dir))?;
    for name in BINARIES {
        let source = install_dir.join(name);
        if name == "loomd" && !source.exists() {
            continue;
        }
        let target = temp.path().join(name);
        fs::copy(&source, &target).map_err(io_error("cannot copy", &source))?;
        File::open(&target)
            .and_then(|file| file.sync_all())
            .map_err(io_error("cannot sync", &target))?;
    }
    let dest = versions_dir.join(version.to_string());
    if dest.exists() {
        fs::remove_dir_all(&dest).map_err(io_error("cannot remove", &dest))?;
    }
    let saved = temp.keep();
    fs::rename(&saved, &dest).map_err(io_error("cannot rename", &saved))?;
    sync_dir(versions_dir)?;
    Ok(dest)
}

/// Deletes `versions_dir/<version>/` for each of `versions`. Returns the versions deleted and
/// the errors of the others.
#[must_use]
pub fn prune(versions_dir: &Path, versions: &[Version]) -> (Vec<Version>, Vec<IoError>) {
    let mut deleted = Vec::new();
    let mut errors = Vec::new();
    for version in versions {
        let dir = versions_dir.join(version.to_string());
        match fs::remove_dir_all(&dir).map_err(io_error("cannot remove", &dir)) {
            Ok(()) => deleted.push(version.clone()),
            Err(error) => errors.push(error),
        }
    }
    (deleted, errors)
}

/// Makes renames in `dir` durable.
fn sync_dir(dir: &Path) -> Result<(), IoError> {
    File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(io_error("cannot sync", dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
    }

    fn read(dir: &Path, name: &str) -> String {
        fs::read_to_string(dir.join(name)).unwrap()
    }

    #[test]
    fn replaces_both_binaries_executable() {
        let install = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        write(install.path(), "loom", "old loom");
        write(source.path(), "loom", "new loom");
        write(source.path(), "loomd", "new loomd");
        replace_binaries(install.path(), source.path()).unwrap();
        assert_eq!(read(install.path(), "loom"), "new loom");
        // Created although it was missing.
        assert_eq!(read(install.path(), "loomd"), "new loomd");
        let mode = fs::metadata(install.path().join("loom"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        // No temporary file left behind.
        assert_eq!(fs::read_dir(install.path()).unwrap().count(), 2);
    }

    #[test]
    fn a_missing_source_leaves_the_install_untouched() {
        let install = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        write(install.path(), "loom", "old loom");
        write(install.path(), "loomd", "old loomd");
        write(source.path(), "loomd", "new loomd");
        assert!(replace_binaries(install.path(), source.path()).is_err());
        assert_eq!(read(install.path(), "loom"), "old loom");
        assert_eq!(read(install.path(), "loomd"), "old loomd");
        assert_eq!(fs::read_dir(install.path()).unwrap().count(), 2);
    }

    #[test]
    fn saves_and_overwrites_a_version() {
        let install = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let versions = home.path().join("versions");
        write(install.path(), "loom", "loom 1");
        write(install.path(), "loomd", "loomd 1");
        let version = Version::new(0, 1, 0);
        let saved = save_installed(install.path(), &versions, &version).unwrap();
        assert_eq!(saved, versions.join("0.1.0"));
        assert_eq!(read(&saved, "loomd"), "loomd 1");
        let mode = fs::metadata(&versions).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);

        write(install.path(), "loom", "loom 1 rebuilt");
        save_installed(install.path(), &versions, &version).unwrap();
        assert_eq!(read(&saved, "loom"), "loom 1 rebuilt");
        // Only the version directory, no leftover temporary directory.
        assert_eq!(fs::read_dir(&versions).unwrap().count(), 1);

        let (deleted, errors) = prune(&versions, &[version.clone(), Version::new(9, 9, 9)]);
        assert_eq!(deleted, vec![version]);
        assert_eq!(errors.len(), 1, "9.9.9 does not exist");
        assert!(!saved.exists());
    }

    #[test]
    fn unwritable_directory_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_writable(dir.path()).is_ok());
        assert!(check_writable(&dir.path().join("missing")).is_err());
    }
}
