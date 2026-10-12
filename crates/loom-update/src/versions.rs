//! Previous versions kept in `<loom home>/versions/<version>/`: which to keep, which to
//! restore (pure), and the directory listing (I/O).

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::Path;

use semver::Version;

/// Number of previous versions kept.
pub const KEEP: usize = 2;

fn same(a: &Version, b: &Version) -> bool {
    a.cmp_precedence(b) == Ordering::Equal
}

/// Saved versions to delete after `just_saved` was saved and `installed` installed.
///
/// The installed version is not a previous version: it is never counted nor deleted. Among the
/// others, `just_saved` is always kept, then the highest versions (semver precedence) up to
/// `keep` in all. Pure.
#[must_use]
pub fn to_prune(
    saved: &[Version],
    installed: &Version,
    just_saved: &Version,
    keep: usize,
) -> Vec<Version> {
    let mut previous: Vec<&Version> = saved
        .iter()
        .filter(|version| !same(version, installed) && !same(version, just_saved))
        .collect();
    previous.sort_by(|a, b| b.cmp_precedence(a));
    let room = keep.saturating_sub(usize::from(!same(just_saved, installed)));
    previous.into_iter().skip(room).cloned().collect()
}

/// Version `loom rollback` restores: the highest saved version (semver precedence) that
/// differs from `current`. Pure.
#[must_use]
pub fn rollback_target(saved: &[Version], current: &Version) -> Option<Version> {
    saved
        .iter()
        .filter(|version| !same(version, current))
        .max_by(|a, b| a.cmp_precedence(b))
        .cloned()
}

/// Versions saved in `dir`: subdirectories named after a version that hold both `loom` and
/// `loomd`. A missing `dir` holds none.
///
/// # Errors
///
/// Any I/O error other than `dir` not existing.
pub fn list(dir: &Path) -> io::Result<Vec<Version>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut versions = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(version) = entry
            .file_name()
            .to_str()
            .and_then(|name| Version::parse(name).ok())
        else {
            continue;
        };
        let path = entry.path();
        if path.join("loom").is_file() && path.join("loomd").is_file() {
            versions.push(version);
        }
    }
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn vs(list: &[&str]) -> Vec<Version> {
        list.iter().map(|s| v(s)).collect()
    }

    #[test]
    fn keeps_the_two_highest_previous_versions() {
        // Upgrade 0.3 -> 0.4: 0.3 just saved, 0.1 is the oldest.
        assert_eq!(
            to_prune(
                &vs(&["0.1.0", "0.2.0", "0.3.0"]),
                &v("0.4.0"),
                &v("0.3.0"),
                KEEP
            ),
            vs(&["0.1.0"])
        );
        assert_eq!(
            to_prune(&vs(&["0.2.0", "0.3.0"]), &v("0.4.0"), &v("0.3.0"), KEEP),
            Vec::<Version>::new()
        );
    }

    #[test]
    fn never_prunes_the_one_just_saved() {
        // A rollback from 0.3 to 0.2 saved 0.3; a `--from` build may save a lower version.
        assert_eq!(
            to_prune(
                &vs(&["0.1.0", "0.5.0", "0.6.0"]),
                &v("0.7.0"),
                &v("0.1.0"),
                KEEP
            ),
            vs(&["0.5.0"])
        );
    }

    #[test]
    fn the_installed_version_is_not_a_previous_one() {
        // Rollback 0.3 -> 0.2: versions/0.2.0 still exists, it is the installed version.
        assert_eq!(
            to_prune(
                &vs(&["0.1.0", "0.2.0", "0.3.0"]),
                &v("0.2.0"),
                &v("0.3.0"),
                KEEP
            ),
            Vec::<Version>::new()
        );
        // `--from` a build with the installed version: the save holds the same version.
        assert_eq!(
            to_prune(
                &vs(&["0.1.0", "0.2.0", "0.3.0"]),
                &v("0.3.0"),
                &v("0.3.0"),
                KEEP
            ),
            Vec::<Version>::new()
        );
        assert_eq!(
            to_prune(
                &vs(&["0.0.9", "0.1.0", "0.2.0", "0.3.0"]),
                &v("0.3.0"),
                &v("0.3.0"),
                KEEP
            ),
            vs(&["0.0.9"])
        );
    }

    #[test]
    fn prereleases_order_before_their_release() {
        assert_eq!(
            to_prune(
                &vs(&["0.2.0-dev.9", "0.2.0", "0.3.0-dev.1"]),
                &v("0.3.0-dev.2"),
                &v("0.3.0-dev.1"),
                KEEP
            ),
            vs(&["0.2.0-dev.9"])
        );
    }

    #[test]
    fn rollback_picks_the_highest_other_version() {
        assert_eq!(
            rollback_target(&vs(&["0.1.0", "0.2.0", "0.3.0"]), &v("0.3.0")),
            Some(v("0.2.0"))
        );
        assert_eq!(
            rollback_target(&vs(&["0.1.0", "0.3.0"]), &v("0.2.0")),
            Some(v("0.3.0"))
        );
        assert_eq!(rollback_target(&vs(&["0.2.0"]), &v("0.2.0")), None);
        assert_eq!(rollback_target(&[], &v("0.2.0")), None);
    }

    #[test]
    fn lists_complete_version_directories_only() {
        let dir = tempfile::tempdir().unwrap();
        for (name, files) in [
            ("0.1.0", &["loom", "loomd"][..]),
            ("0.2.0", &["loom"][..]),
            ("not-a-version", &["loom", "loomd"][..]),
        ] {
            fs::create_dir(dir.path().join(name)).unwrap();
            for file in files {
                fs::write(dir.path().join(name).join(file), "x").unwrap();
            }
        }
        assert_eq!(list(dir.path()).unwrap(), vs(&["0.1.0"]));
        assert_eq!(list(&dir.path().join("missing")).unwrap(), Vec::new());
    }
}
