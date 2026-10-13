//! Changes between the known repositories and a scan: added, updated, removed (ADR-0012).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use loom_api::RepoIdentity;

use crate::assign_unique_slugs;
use crate::scan::ScanReport;

/// Changes a scan brings to the known repositories. Each list is sorted by path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanDiff {
    /// Repositories not known before, with their unique slug.
    pub added: Vec<RepoIdentity>,
    /// Known repositories whose identity changed (remotes, context, forge or slug), new value.
    pub updated: Vec<RepoIdentity>,
    /// Known repositories under a scanned root that the scan no longer finds, previous value.
    pub removed: Vec<RepoIdentity>,
}

/// Changes from `previous` (the known repositories, with their slugs) after `report`, a scan
/// of `scanned_roots`. Pure.
///
/// Only a repository under one of `scanned_roots` (whole path components, as configured) can
/// be removed, and only when no error of the report hides it (an unreadable directory
/// above it, or its own unreadable `.git` or config). Slugs are made unique over every
/// repository kept, scanned or not, with [`assign_unique_slugs`] and the previous slugs, so
/// that a slug never changes while its repository's context and name do not.
///
/// Registered repositories are scanned in place by [`crate::scan_with_registered`], which
/// returns their paths among `scanned_roots`: kept while on disk, removed once gone.
#[must_use]
pub fn diff(previous: &[RepoIdentity], scanned_roots: &[PathBuf], report: &ScanReport) -> ScanDiff {
    let scanned = |path: &Path| scanned_roots.iter().any(|root| path.starts_with(root));
    let hidden = |path: &Path| {
        report
            .errors
            .iter()
            .any(|error| error.hides_repos() && path.starts_with(error.path()))
    };

    let mut current: Vec<RepoIdentity> = report
        .repos
        .iter()
        .map(|repo| repo.identity.clone())
        .collect();
    let found: BTreeSet<PathBuf> = current.iter().map(|repo| repo.path.clone()).collect();
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for repo in previous {
        if found.contains(&repo.path) {
            continue;
        }
        if scanned(&repo.path) && !hidden(&repo.path) {
            removed.push(repo.clone());
        } else {
            kept.push(repo.clone());
        }
    }
    current.extend(kept);

    let existing: BTreeMap<PathBuf, String> = previous
        .iter()
        .map(|repo| (repo.path.clone(), repo.slug.clone()))
        .collect();
    assign_unique_slugs(&existing, &mut current);

    let before: BTreeMap<&Path, &RepoIdentity> = previous
        .iter()
        .map(|repo| (repo.path.as_path(), repo))
        .collect();
    let mut added = Vec::new();
    let mut updated = Vec::new();
    for repo in current {
        match before.get(repo.path.as_path()) {
            None => added.push(repo),
            Some(old) if **old != repo => updated.push(repo),
            Some(_) => {}
        }
    }
    for list in [&mut added, &mut updated, &mut removed] {
        list.sort_by(|a, b| a.path.cmp(&b.path));
    }
    ScanDiff {
        added,
        updated,
        removed,
    }
}

#[cfg(test)]
mod tests {
    use super::{ScanDiff, diff};
    use crate::scan::{ScanError, ScanReport, ScannedRepo};
    use loom_api::{ContextAssignment, Remote, RepoIdentity};
    use std::io;
    use std::path::{Path, PathBuf};

    /// A repository at `path` in context `p`, slug `slug`, with `remotes` remote names.
    fn repo(path: &str, slug: &str, remotes: &[&str]) -> RepoIdentity {
        RepoIdentity {
            path: PathBuf::from(path),
            name: path.rsplit('/').next().unwrap().to_owned(),
            slug: slug.to_owned(),
            context: ContextAssignment::Default {
                context: "p".parse().unwrap(),
            },
            remotes: remotes
                .iter()
                .map(|name| Remote {
                    name: (*name).to_owned(),
                    url: format!("/srv/{name}"),
                    location: None,
                })
                .collect(),
            primary_remote: remotes.first().map(|name| (*name).to_owned()),
            forge: None,
        }
    }

    /// A scanned repository: base slug, as `identify` gives it.
    fn scanned(path: &str, remotes: &[&str]) -> ScannedRepo {
        let name = path.rsplit('/').next().unwrap();
        ScannedRepo {
            identity: repo(path, &format!("p/{name}"), remotes),
            git_dir: Path::new(path).join(".git"),
            common_dir: Path::new(path).join(".git"),
            head: None,
        }
    }

    fn report(repos: Vec<ScannedRepo>, errors: Vec<ScanError>) -> ScanReport {
        ScanReport { repos, errors }
    }

    fn roots(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn paths(repos: &[RepoIdentity]) -> Vec<(&str, &str)> {
        repos
            .iter()
            .map(|repo| (repo.path.to_str().unwrap(), repo.slug.as_str()))
            .collect()
    }

    #[test]
    fn first_scan_adds_everything_with_unique_slugs() {
        let result = diff(
            &[],
            &roots(&["/w"]),
            &report(vec![scanned("/w/b/x", &[]), scanned("/w/a/x", &[])], vec![]),
        );
        assert_eq!(
            paths(&result.added),
            [("/w/a/x", "p/x"), ("/w/b/x", "p/x-2")]
        );
        assert_eq!(result.updated, []);
        assert_eq!(result.removed, []);
    }

    #[test]
    fn unchanged_scan_changes_nothing() {
        let previous = [
            repo("/w/a/x", "p/x", &["origin"]),
            repo("/w/b/x", "p/x-2", &[]),
        ];
        let result = diff(
            &previous,
            &roots(&["/w"]),
            &report(
                vec![scanned("/w/a/x", &["origin"]), scanned("/w/b/x", &[])],
                vec![],
            ),
        );
        assert_eq!(result, ScanDiff::default());
    }

    #[test]
    fn changed_identity_is_updated() {
        let previous = [repo("/w/a", "p/a", &["origin"]), repo("/w/b", "p/b", &[])];
        let result = diff(
            &previous,
            &roots(&["/w"]),
            &report(
                vec![scanned("/w/a", &["origin", "fork"]), scanned("/w/b", &[])],
                vec![],
            ),
        );
        assert_eq!(result.updated, [repo("/w/a", "p/a", &["origin", "fork"])]);
        assert_eq!(result.added, []);
        assert_eq!(result.removed, []);
    }

    #[test]
    fn only_repos_under_scanned_roots_are_removed() {
        let previous = [
            repo("/w/gone", "p/gone", &[]),
            repo("/other/kept", "p/kept", &[]),
            // Same prefix as a scanned root, but another directory.
            repo("/workshop/kept", "p/kept-2", &[]),
        ];
        let result = diff(&previous, &roots(&["/w"]), &report(vec![], vec![]));
        assert_eq!(paths(&result.removed), [("/w/gone", "p/gone")]);
        assert_eq!(result.added, []);
        assert_eq!(result.updated, []);

        let result = diff(&previous, &roots(&["/elsewhere"]), &report(vec![], vec![]));
        assert_eq!(result, ScanDiff::default());
    }

    #[test]
    fn repos_hidden_by_an_error_are_kept() {
        let previous = [
            repo("/w/locked/a", "p/a", &[]),
            repo("/w/broken", "p/broken", &[]),
            repo("/w/no-head", "p/no-head", &[]),
            repo("/w/gone", "p/gone", &[]),
        ];
        let errors = vec![
            ScanError::ReadDir {
                path: PathBuf::from("/w/locked"),
                source: io::Error::from(io::ErrorKind::PermissionDenied),
            },
            ScanError::GitDir {
                path: PathBuf::from("/w/broken"),
                reason: "missing".to_owned(),
            },
            // HEAD errors do not hide the repository: it is in the report or gone.
            ScanError::Head {
                path: PathBuf::from("/w/no-head"),
                reason: "empty".to_owned(),
            },
        ];
        let result = diff(&previous, &roots(&["/w"]), &report(vec![], errors));
        assert_eq!(
            paths(&result.removed),
            [("/w/gone", "p/gone"), ("/w/no-head", "p/no-head")]
        );
    }

    #[test]
    fn slugs_stay_unique_across_unscanned_repos() {
        // `/other/x` holds `p/x` and is not rescanned: the new `/w/x` must not take it.
        let previous = [repo("/other/x", "p/x", &[]), repo("/w/old/x", "p/x-2", &[])];
        let result = diff(
            &previous,
            &roots(&["/w"]),
            &report(
                vec![scanned("/w/new/x", &[]), scanned("/w/old/x", &[])],
                vec![],
            ),
        );
        assert_eq!(paths(&result.added), [("/w/new/x", "p/x-3")]);
        assert_eq!(result.updated, []);
        assert_eq!(result.removed, []);
    }

    #[test]
    fn removal_frees_a_slug_without_renumbering() {
        let previous = [repo("/w/a/x", "p/x", &[]), repo("/w/b/x", "p/x-2", &[])];
        let result = diff(
            &previous,
            &roots(&["/w"]),
            &report(vec![scanned("/w/b/x", &[]), scanned("/w/c/x", &[])], vec![]),
        );
        assert_eq!(paths(&result.removed), [("/w/a/x", "p/x")]);
        assert_eq!(paths(&result.added), [("/w/c/x", "p/x")]);
        assert_eq!(result.updated, []);
    }
}
