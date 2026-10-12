//! Release discovery, update decision, download, verification, install and rollback.
//!
//! Shared by `loom` and `loomd`. Decisions are pure functions ([`decide`],
//! [`release::published`], [`verify`], [`versions::to_prune`], [`versions::rollback_target`],
//! [`archive::check_entry`], [`github::resolve_token`]); the functions of this module do the
//! I/O around them, in this order:
//!
//! 1. [`discover`] the release of a channel or version;
//! 2. [`decide`] whether to move to it;
//! 3. [`download`] and verify it into a staging directory, or stage a local build with
//!    [`stage::from_local`], or pick a saved version with [`rollback_source`];
//! 4. [`install`] it: save the installed version, replace the binaries, prune, restart loomd.
//!
//! Nothing installed changes before step 4.

pub mod archive;
mod daemon;
mod decision;
pub mod github;
pub mod install;
pub mod release;
pub mod stage;
pub mod tls;
pub mod verify;
pub mod versions;

use std::path::PathBuf;

use loom_api::DaemonRestart;
use semver::Version;

pub use decision::{Channel, SkipReason, UpdateDecision, UpdateMode, UpdatePolicy, decide};
pub use stage::Staged;

use crate::github::{Client, HttpError};
use crate::install::IoError;
use crate::release::{AssetError, Release};
use crate::verify::VerifyError;

/// Environment variable overriding the release public key (base64 key line). Test hook,
/// honoured by debug builds only.
pub const PUBLIC_KEY_ENV: &str = "LOOM_UPDATE_PUBKEY";

/// Public key verifying `SHA256SUMS`: the key embedded from `dist/minisign.pub`, or
/// `override_key` (`LOOM_UPDATE_PUBKEY`) in a debug build. Pure.
#[must_use]
pub fn public_key(override_key: Option<&str>) -> &str {
    match override_key.filter(|key| !key.is_empty()) {
        Some(key) if cfg!(debug_assertions) => key,
        _ => verify::embedded_public_key(),
    }
}

/// Why an update or a rollback failed.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// HTTP request failed.
    #[error(transparent)]
    Http(#[from] HttpError),
    /// Release asset missing.
    #[error(transparent)]
    Asset(#[from] AssetError),
    /// Signature or checksum verification failed.
    #[error(transparent)]
    Verify(#[from] VerifyError),
    /// Archive rejected.
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    /// Filesystem error.
    #[error(transparent)]
    Io(#[from] IoError),
    /// The new binaries did not report a usable version.
    #[error("the binaries in {} are not a usable loom build: {message}", dir.display())]
    Probe {
        /// Directory of the binaries.
        dir: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// A `--from` directory without `loom` or `loomd`.
    #[error("{} has no {name} file", dir.display())]
    LocalBinaryMissing {
        /// Directory given.
        dir: PathBuf,
        /// Missing binary.
        name: &'static str,
    },
    /// A release tag that is not `vX.Y.Z` (or `vX.Y.Z-dev.N` where expected).
    #[error("unexpected release tag `{0}`")]
    BadTag(String),
    /// The binaries of a release report another version than its tag.
    #[error("release {expected} contains binaries reporting version {actual}")]
    VersionMismatch {
        /// Version of the release or saved directory.
        expected: Version,
        /// Version the binaries report.
        actual: Version,
    },
    /// No saved version to roll back to.
    #[error(
        "nothing to roll back to: no saved version other than {current} in {}",
        dir.display()
    )]
    NothingToRollBack {
        /// Current version.
        current: Version,
        /// Versions directory.
        dir: PathBuf,
    },
}

/// Paths the updater reads and writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Directory of the running `loom`, where `loom` and `loomd` are replaced.
    pub install_dir: PathBuf,
    /// `<loom home>/versions/`.
    pub versions_dir: PathBuf,
    /// `<loom home>/staging/`.
    pub staging_dir: PathBuf,
    /// `<loom home>/run/loomd.lock`.
    pub lock: PathBuf,
}

/// What to look up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The latest release of a channel.
    Channel(Channel),
    /// The release of this version.
    Version(Version),
}

/// Releases found by [`discover`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Discovery {
    /// Published releases with their versions, to give to [`decide`] and then [`download`].
    pub releases: Vec<(Version, Release)>,
}

impl Discovery {
    /// Versions to give to [`decide`].
    #[must_use]
    pub fn versions(&self) -> Vec<Version> {
        self.releases
            .iter()
            .map(|(version, _)| version.clone())
            .collect()
    }

    /// The release of `version`, as [`decide`] chose it.
    #[must_use]
    pub fn release(&self, version: &Version) -> Option<&Release> {
        self.releases
            .iter()
            .find(|(found, _)| found == version)
            .map(|(_, release)| release)
    }
}

/// Looks up the releases to decide on:
///
/// - stable: `GET /releases/latest`, which must be a stable `vX.Y.Z`;
/// - dev: every non-draft release of `GET /releases?per_page=30` whose tag is `vX.Y.Z` or
///   `vX.Y.Z-dev.N` (others are skipped), so that [`decide`]'s dev rule can pick a stable
///   release above every dev build;
/// - a version: `GET /releases/tags/vX.Y.Z`; not found gives no release.
///
/// # Errors
///
/// [`UpdateError`] on HTTP errors or an unexpected tag.
pub fn discover(client: &Client, lookup: &Lookup) -> Result<Discovery, UpdateError> {
    let single = |release: Release, expected: &dyn Fn(&Version) -> bool| match release::tag_version(
        &release.tag_name,
    ) {
        Some(version) if expected(&version) => Ok(Discovery {
            releases: vec![(version, release)],
        }),
        _ => Err(UpdateError::BadTag(release.tag_name)),
    };
    match lookup {
        Lookup::Channel(Channel::Stable) => single(client.latest()?, &|v| v.pre.is_empty()),
        Lookup::Channel(Channel::Dev) => Ok(Discovery {
            releases: release::published(client.list()?),
        }),
        // Not found: no release, so `decide` reports the pin as not released.
        Lookup::Version(wanted) => client
            .by_tag(&format!("v{wanted}"))?
            .map_or_else(|| Ok(Discovery::default()), |r| single(r, &|v| v == wanted)),
    }
}

/// Downloads and verifies the archive of `release` for `target` into a staging directory.
///
/// `SHA256SUMS` and its signature first (checked against `public_key`), then the archive,
/// checked against its `SHA256SUMS` line, then extracted; its binaries must report
/// `version`. `progress` receives one line per step.
///
/// # Errors
///
/// [`UpdateError`]; nothing installed has changed.
pub fn download(
    client: &Client,
    release: &Release,
    version: &Version,
    target: &str,
    public_key: &str,
    staging_root: &std::path::Path,
    progress: &mut dyn FnMut(&str),
) -> Result<Staged, UpdateError> {
    let archive_name = release::archive_name(version, target);
    let sums_id = release::asset_id(release, "SHA256SUMS")?;
    let sig_id = release::asset_id(release, "SHA256SUMS.minisig")?;
    let archive_id = release::asset_id(release, &archive_name)?;

    progress(&format!("downloading SHA256SUMS of {}", release.tag_name));
    let sums = client.asset(sums_id, "SHA256SUMS")?;
    let sig = client.asset(sig_id, "SHA256SUMS.minisig")?;
    let sig = String::from_utf8(sig)
        .map_err(|_| VerifyError::SignatureFormat("SHA256SUMS.minisig is not UTF-8".to_owned()))?;
    verify::verify_signature(public_key, &sums, &sig)?;
    progress("verified the signature of SHA256SUMS");
    let sums = String::from_utf8_lossy(&sums).into_owned();
    // Fail before a long download when the archive is not listed.
    verify::listed_checksum(&sums, &archive_name)?;

    progress(&format!("downloading {archive_name}"));
    let data = client.asset(archive_id, &archive_name)?;
    verify::verify_checksum(&sums, &archive_name, &data)?;
    progress(&format!("verified the checksum of {archive_name}"));

    let staged = stage::from_archive(&data[..], staging_root)?;
    check_version(&staged, version)?;
    Ok(staged)
}

fn check_version(staged: &Staged, expected: &Version) -> Result<(), UpdateError> {
    if staged.version == *expected {
        Ok(())
    } else {
        Err(UpdateError::VersionMismatch {
            expected: expected.clone(),
            actual: staged.version.clone(),
        })
    }
}

/// The saved version `loom rollback` restores: see [`versions::rollback_target`]. Its
/// binaries must report the version of their directory.
///
/// # Errors
///
/// [`UpdateError::NothingToRollBack`], or an I/O or probe error.
pub fn rollback_source(paths: &Paths, current: &Version) -> Result<Staged, UpdateError> {
    let saved = versions::list(&paths.versions_dir).map_err(|source| IoError {
        action: "cannot read",
        path: paths.versions_dir.clone(),
        source,
    })?;
    let target = versions::rollback_target(&saved, current).ok_or_else(|| {
        UpdateError::NothingToRollBack {
            current: current.clone(),
            dir: paths.versions_dir.clone(),
        }
    })?;
    let dir = paths.versions_dir.join(target.to_string());
    let reported = stage::probe(&dir)?;
    if reported != target {
        return Err(UpdateError::VersionMismatch {
            expected: target,
            actual: reported,
        });
    }
    Ok(Staged::existing(dir, target))
}

/// Result of [`install`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// Where the replaced version was saved.
    pub saved_to: PathBuf,
    /// Saved versions deleted.
    pub pruned: Vec<Version>,
    /// What happened to loomd.
    pub daemon: DaemonRestart,
    /// Problems after the binaries were replaced (pruning); the install itself succeeded.
    pub warnings: Vec<String>,
}

/// Installs `staged` in place of the running version `current`.
///
/// Saves the installed binaries into `versions/<current>/`, replaces `loomd` and `loom` in
/// the install directory, keeps the [`versions::KEEP`] most recent previous versions, then
/// sends `SIGTERM` to a running loomd.
///
/// # Errors
///
/// [`UpdateError::Io`] when saving or replacing fails; the binaries are then untouched,
/// unless the second rename of [`install::replace_binaries`] fails after the first succeeded.
pub fn install(
    staged: &Staged,
    paths: &Paths,
    current: &Version,
) -> Result<Installed, UpdateError> {
    let saved_to = install::save_installed(&paths.install_dir, &paths.versions_dir, current)?;
    install::replace_binaries(&paths.install_dir, &staged.dir)?;

    let mut warnings = Vec::new();
    let pruned = match versions::list(&paths.versions_dir) {
        Ok(saved) => {
            let doomed = versions::to_prune(&saved, &staged.version, current, versions::KEEP);
            let (pruned, errors) = install::prune(&paths.versions_dir, &doomed);
            warnings.extend(errors.iter().map(ToString::to_string));
            pruned
        }
        Err(error) => {
            warnings.push(format!(
                "cannot read {}: {error}",
                paths.versions_dir.display()
            ));
            Vec::new()
        }
    };
    let daemon = daemon::restart(&paths.lock);
    Ok(Installed {
        saved_to,
        pruned,
        daemon,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::public_key;
    use crate::verify::embedded_public_key;

    #[test]
    fn public_key_override_only_in_debug_builds() {
        assert_eq!(public_key(None), embedded_public_key());
        assert_eq!(public_key(Some("")), embedded_public_key());
        let expected = if cfg!(debug_assertions) {
            "TESTKEY"
        } else {
            embedded_public_key()
        };
        assert_eq!(public_key(Some("TESTKEY")), expected);
    }
}
