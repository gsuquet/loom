//! Staging: new binaries unpacked or copied into a directory of their own under the loom
//! home, then asked for their version before anything installed is touched.

use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use loom_api::VersionInfo;
use semver::Version;
use tempfile::TempDir;

use crate::UpdateError;
use crate::archive;
use crate::install::{BINARIES, IoError, create_private_dir};

/// New binaries ready to be installed.
#[derive(Debug)]
pub struct Staged {
    /// Directory holding `loom` and `loomd`.
    pub dir: PathBuf,
    /// Version they report.
    pub version: Version,
    /// Removed when dropped; `None` when `dir` is not ours to remove (a saved version).
    _temp: Option<TempDir>,
}

impl Staged {
    /// Binaries already on disk, such as a saved version, of a known `version`.
    #[must_use]
    pub const fn existing(dir: PathBuf, version: Version) -> Self {
        Self {
            dir,
            version,
            _temp: None,
        }
    }
}

/// A fresh, empty staging directory under `staging_root` (created 0700), removed on drop.
///
/// # Errors
///
/// [`IoError`].
pub fn staging_dir(staging_root: &Path) -> Result<TempDir, IoError> {
    create_private_dir(staging_root)?;
    tempfile::Builder::new()
        .prefix("upgrade-")
        .tempdir_in(staging_root)
        .map_err(|source| IoError {
            action: "cannot write to",
            path: staging_root.to_path_buf(),
            source,
        })
}

/// Extracts the release archive `data` into a new staging directory and reads the version of
/// the binaries.
///
/// # Errors
///
/// [`UpdateError`].
pub fn from_archive(data: impl std::io::Read, staging_root: &Path) -> Result<Staged, UpdateError> {
    let temp = staging_dir(staging_root)?;
    archive::extract(data, temp.path())?;
    staged(temp)
}

/// Stages a local build given with `--from`: a `.tar.gz` archive with the release layout, or
/// a directory holding `loom` and `loomd`.
///
/// # Errors
///
/// [`UpdateError`].
pub fn from_local(path: &Path, staging_root: &Path) -> Result<Staged, UpdateError> {
    let metadata = fs::metadata(path).map_err(|source| IoError {
        action: "cannot read",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.is_file() {
        let file = File::open(path).map_err(|source| IoError {
            action: "cannot read",
            path: path.to_path_buf(),
            source,
        })?;
        return from_archive(BufReader::new(file), staging_root);
    }
    let temp = staging_dir(staging_root)?;
    for name in BINARIES {
        let source = path.join(name);
        if !source.is_file() {
            return Err(UpdateError::LocalBinaryMissing {
                dir: path.to_path_buf(),
                name,
            });
        }
        fs::copy(&source, temp.path().join(name)).map_err(|source_error| IoError {
            action: "cannot copy",
            path: source.clone(),
            source: source_error,
        })?;
    }
    staged(temp)
}

fn staged(temp: TempDir) -> Result<Staged, UpdateError> {
    let version = probe(temp.path())?;
    Ok(Staged {
        dir: temp.path().to_path_buf(),
        version,
        _temp: Some(temp),
    })
}

/// Version of the binaries in `dir`: `loom version --json` must print a `VersionInfo` with a
/// semver version, and `loomd -V` must print `loomd <that version>`.
///
/// # Errors
///
/// [`UpdateError::Probe`].
pub fn probe(dir: &Path) -> Result<Version, UpdateError> {
    let probe_error = |message: String| UpdateError::Probe {
        dir: dir.to_path_buf(),
        message,
    };
    let loom = run(&dir.join("loom"), &["version", "--json"]).map_err(probe_error)?;
    let info: VersionInfo = serde_json::from_str(&loom).map_err(|error| {
        probe_error(format!(
            "`loom version --json` printed no VersionInfo: {error}"
        ))
    })?;
    let version = Version::parse(&info.version).map_err(|error| {
        probe_error(format!(
            "`loom version --json` reports an invalid version `{}`: {error}",
            info.version
        ))
    })?;
    let loomd = run(&dir.join("loomd"), &["-V"]).map_err(probe_error)?;
    let expected = format!("loomd {version}");
    if loomd.trim() != expected {
        return Err(probe_error(format!(
            "`loomd -V` printed `{}`, expected `{expected}`",
            loomd.trim()
        )));
    }
    Ok(version)
}

fn run(program: &Path, args: &[&str]) -> Result<String, String> {
    let shown = format!("{} {}", program.display(), args.join(" "));
    let output = crate::exec::retry_busy(|| Command::new(program).args(args).output())
        .map_err(|error| format!("cannot run `{shown}`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`{shown}` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| format!("`{shown}` printed invalid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn script(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn version_json(version: &str) -> String {
        format!(
            r#"{{"version":"{version}","commit":"unknown","commit_date":"unknown","target":"x","channel":"dev","profile":"release"}}"#
        )
    }

    fn stubs(dir: &Path, cli: &str, daemon: &str) {
        script(dir, "loom", &format!("echo '{}'", version_json(cli)));
        script(dir, "loomd", &format!("echo 'loomd {daemon}'"));
    }

    #[test]
    fn probe_reads_matching_versions() {
        let dir = tempfile::tempdir().unwrap();
        stubs(dir.path(), "1.2.3-dev.4", "1.2.3-dev.4");
        assert_eq!(probe(dir.path()).unwrap().to_string(), "1.2.3-dev.4");
    }

    #[test]
    fn probe_rejects_mismatches_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        stubs(dir.path(), "1.2.3", "1.2.4");
        assert!(matches!(probe(dir.path()), Err(UpdateError::Probe { .. })));

        script(dir.path(), "loom", "echo not json");
        assert!(matches!(probe(dir.path()), Err(UpdateError::Probe { .. })));

        script(dir.path(), "loom", "exit 3");
        assert!(matches!(probe(dir.path()), Err(UpdateError::Probe { .. })));
    }

    #[test]
    fn local_directory_needs_both_binaries() {
        let staging = tempfile::tempdir().unwrap();
        let build = tempfile::tempdir().unwrap();
        script(build.path(), "loom", "true");
        assert!(matches!(
            from_local(build.path(), staging.path()),
            Err(UpdateError::LocalBinaryMissing { name: "loomd", .. })
        ));

        stubs(build.path(), "2.0.0", "2.0.0");
        let staged = from_local(build.path(), staging.path()).unwrap();
        assert_eq!(staged.version, Version::new(2, 0, 0));
        assert!(staged.dir.starts_with(staging.path()));
        let dir = staged.dir.clone();
        drop(staged);
        assert!(!dir.exists(), "staging directory removed on drop");
    }
}
