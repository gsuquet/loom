//! Identity of a repository: names, remotes, forge and context, and unique slugs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use loom_api::RepoIdentity;
use loom_config::repos::{ForgeConfig, ForgeHost};

use crate::{ContextRules, detect_forge, primary_remote, remote};

/// Identity of the repository at `path` (absolute, as found) with `remotes` (name and URL of
/// each `[remote "<name>"]`, in configuration order). Pure.
///
/// The slug is `<context>/<name>`, or `unassigned/<name>`; [`assign_unique_slugs`] then
/// suffixes duplicates. SSH aliases of the remotes go through `resolve_alias` (see
/// [`crate::parse_remote_url`]).
#[must_use]
pub fn identify(
    path: &Path,
    remotes: &[(String, String)],
    rules: &ContextRules,
    forges: &BTreeMap<ForgeHost, ForgeConfig>,
    resolve_alias: impl Fn(&str) -> Option<String>,
) -> RepoIdentity {
    let remotes: Vec<_> = remotes
        .iter()
        .map(|(name, url)| remote(name, url, &resolve_alias))
        .collect();
    let primary = primary_remote(&remotes);
    let location = primary.and_then(|remote| remote.location.as_ref());
    let context = rules.assign(path, location);
    let name = path.file_name().map_or_else(
        || path.to_string_lossy().into_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    RepoIdentity {
        path: path.to_path_buf(),
        slug: format!("{}/{name}", context.context_name()),
        name,
        context,
        primary_remote: primary.map(|remote| remote.name.clone()),
        forge: location.and_then(|location| detect_forge(&location.host, forges)),
        remotes,
    }
}

/// Sets a unique, stable slug on every repository of `repos`. Pure.
///
/// `existing` maps the path of each repository known before to its slug. A repository keeps
/// its existing slug while its context and name are unchanged, so a new repository never
/// renumbers another one. The others (new, or whose context or name changed) get
/// `<context>/<name>` when free, otherwise the lowest free `<context>/<name>-N` (N ≥ 2), in
/// path order. Slugs of repositories absent from `repos` are free again.
pub fn assign_unique_slugs(existing: &BTreeMap<PathBuf, String>, repos: &mut [RepoIdentity]) {
    let base = |repo: &RepoIdentity| format!("{}/{}", repo.context.context_name(), repo.name);
    let mut order: Vec<usize> = (0..repos.len()).collect();
    order.sort_by(|a, b| repos[*a].path.cmp(&repos[*b].path));

    let mut taken = BTreeSet::new();
    let mut pending = Vec::new();
    for index in order {
        let kept = existing
            .get(&repos[index].path)
            .filter(|slug| belongs_to(slug, &base(&repos[index])))
            .filter(|slug| taken.insert((*slug).clone()));
        match kept {
            Some(slug) => repos[index].slug.clone_from(slug),
            None => pending.push(index),
        }
    }
    // Unsuffixed slugs first, so that a repository named `x-2` gets `<context>/x-2` even when
    // a duplicate `x` comes before it.
    let mut suffixed = Vec::new();
    for index in pending {
        let base = base(&repos[index]);
        if taken.insert(base.clone()) {
            repos[index].slug = base;
        } else {
            suffixed.push(index);
        }
    }
    for index in suffixed {
        let base = base(&repos[index]);
        // At most `taken.len()` slugs are taken: one of the first `taken.len() + 1` is free.
        let slug = (2..=taken.len() + 2)
            .map(|n| format!("{base}-{n}"))
            .find(|candidate| !taken.contains(candidate))
            .unwrap_or(base);
        taken.insert(slug.clone());
        repos[index].slug = slug;
    }
}

/// Whether `slug` is `base` or `base-N` with N ≥ 2.
fn belongs_to(slug: &str, base: &str) -> bool {
    if slug == base {
        return true;
    }
    // Canonical decimal only: digits, no leading zero, at least 2.
    slug.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|n| {
            n.bytes().all(|byte| byte.is_ascii_digit())
                && !n.starts_with('0')
                && n.parse::<u64>().is_ok_and(|n| n >= 2)
        })
}

#[cfg(test)]
mod tests {
    use super::{assign_unique_slugs, identify};
    use crate::ContextRules;
    use loom_api::{ContextAssignment, ForgeKind, Remote, RemoteUrl, RepoIdentity};
    use loom_config::config::parse_config;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    const CONFIG: &str = r#"
[contexts.personal]
match.hosts = ["github.com/gsuquet"]

[contexts.company-a]
match.paths = ["~/work/company-a"]

[forges."git.company-a.example"]
kind = "gitlab"
"#;

    fn setup() -> (ContextRules, loom_config::config::Config) {
        let config = parse_config(CONFIG, Path::new("config.toml")).unwrap();
        let rules = ContextRules::new(&config, Path::new("/home/me")).unwrap();
        (rules, config)
    }

    fn remotes(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, url)| ((*name).to_owned(), (*url).to_owned()))
            .collect()
    }

    fn resolve(alias: &str) -> Option<String> {
        (alias == "work").then(|| "git.company-a.example".to_owned())
    }

    #[test]
    fn identifies_a_repository() {
        let (rules, config) = setup();
        let identity = identify(
            Path::new("/home/me/perso/loom"),
            &remotes(&[
                ("upstream", "https://github.com/other/loom.git"),
                ("origin", "git@github.com:gsuquet/loom.git"),
            ]),
            &rules,
            &config.forges,
            resolve,
        );
        assert_eq!(
            identity,
            RepoIdentity {
                path: PathBuf::from("/home/me/perso/loom"),
                name: "loom".to_owned(),
                slug: "personal/loom".to_owned(),
                context: ContextAssignment::Host {
                    context: "personal".parse().unwrap(),
                    pattern: "github.com/gsuquet".to_owned(),
                },
                remotes: vec![
                    Remote {
                        name: "upstream".to_owned(),
                        url: "https://github.com/other/loom.git".to_owned(),
                        location: Some(RemoteUrl {
                            host: "github.com".to_owned(),
                            port: None,
                            path: "other/loom".to_owned(),
                        }),
                    },
                    Remote {
                        name: "origin".to_owned(),
                        url: "git@github.com:gsuquet/loom.git".to_owned(),
                        location: Some(RemoteUrl {
                            host: "github.com".to_owned(),
                            port: None,
                            path: "gsuquet/loom".to_owned(),
                        }),
                    },
                ],
                primary_remote: Some("origin".to_owned()),
                forge: Some(ForgeKind::Github),
            }
        );
    }

    #[test]
    fn forge_comes_from_the_resolved_alias_of_the_primary_remote() {
        let (rules, config) = setup();
        let identity = identify(
            Path::new("/home/me/work/company-a/app"),
            &remotes(&[("origin", "work:team/app.git")]),
            &rules,
            &config.forges,
            resolve,
        );
        assert_eq!(identity.forge, Some(ForgeKind::Gitlab));
        assert_eq!(identity.slug, "company-a/app");
        assert_eq!(
            identity.remotes[0]
                .location
                .as_ref()
                .map(|url| url.host.as_str()),
            Some("git.company-a.example")
        );
    }

    #[test]
    fn repository_without_remote_is_identified_by_its_path() {
        let (rules, config) = setup();
        let identity = identify(
            Path::new("/tmp/scratch"),
            &[],
            &rules,
            &config.forges,
            resolve,
        );
        assert_eq!(identity.slug, "unassigned/scratch");
        assert_eq!(identity.context, ContextAssignment::Unassigned);
        assert_eq!(identity.primary_remote, None);
        assert_eq!(identity.forge, None);
        assert_eq!(identity.remotes, []);

        // A local primary remote: no location, so no forge nor host match.
        let identity = identify(
            Path::new("/home/me/work/company-a/copy"),
            &remotes(&[("origin", "/srv/git/app.git")]),
            &rules,
            &config.forges,
            resolve,
        );
        assert_eq!(identity.primary_remote.as_deref(), Some("origin"));
        assert_eq!(identity.forge, None);
        assert_eq!(identity.slug, "company-a/copy");
    }

    /// A repository at `path` assigned to `context` by default, with a stale slug.
    fn repo(path: &str, context: &str) -> RepoIdentity {
        let name = path.rsplit('/').next().unwrap_or_default().to_owned();
        RepoIdentity {
            path: PathBuf::from(path),
            slug: "stale".to_owned(),
            name,
            context: ContextAssignment::Default {
                context: context.parse().unwrap(),
            },
            remotes: vec![],
            primary_remote: None,
            forge: None,
        }
    }

    fn slugs(repos: &[RepoIdentity]) -> Vec<(&str, &str)> {
        repos
            .iter()
            .map(|repo| (repo.path.to_str().unwrap(), repo.slug.as_str()))
            .collect()
    }

    /// The path-to-slug map the index would persist after `repos`.
    fn persisted(repos: &[RepoIdentity]) -> BTreeMap<PathBuf, String> {
        repos
            .iter()
            .map(|repo| (repo.path.clone(), repo.slug.clone()))
            .collect()
    }

    #[test]
    fn first_assignment_suffixes_duplicates_in_path_order() {
        let mut repos = vec![
            repo("/c/x", "p"),
            repo("/a/x", "p"),
            repo("/b/x", "p"),
            repo("/d/y", "p"),
            repo("/e/y", "w"),
        ];
        assign_unique_slugs(&BTreeMap::new(), &mut repos);
        assert_eq!(
            slugs(&repos),
            [
                ("/c/x", "p/x-3"),
                ("/a/x", "p/x"),
                ("/b/x", "p/x-2"),
                ("/d/y", "p/y"),
                ("/e/y", "w/y"),
            ]
        );
    }

    #[test]
    fn unsuffixed_slugs_go_first() {
        let mut repos = vec![repo("/b/x", "p"), repo("/a/x", "p"), repo("/a/x-2", "p")];
        assign_unique_slugs(&BTreeMap::new(), &mut repos);
        assert_eq!(
            slugs(&repos),
            [("/b/x", "p/x-3"), ("/a/x", "p/x"), ("/a/x-2", "p/x-2")]
        );
    }

    #[test]
    fn stable_slug_cases() {
        // (repos before, repos now, expected slugs now)
        type Case = (
            Vec<(&'static str, &'static str)>,
            Vec<(&'static str, &'static str)>,
            Vec<(&'static str, &'static str)>,
        );
        let cases: Vec<Case> = vec![
            // A new repository with a smaller path does not renumber the others.
            (
                vec![("/b/x", "p"), ("/c/x", "p")],
                vec![("/a/x", "p"), ("/b/x", "p"), ("/c/x", "p")],
                vec![("/a/x", "p/x-3"), ("/b/x", "p/x"), ("/c/x", "p/x-2")],
            ),
            // A context change re-slugs only that repository.
            (
                vec![("/a/x", "p"), ("/b/x", "p"), ("/c/x", "p")],
                vec![("/a/x", "w"), ("/b/x", "p"), ("/c/x", "p")],
                vec![("/a/x", "w/x"), ("/b/x", "p/x-2"), ("/c/x", "p/x-3")],
            ),
            // A removed repository frees its slug for the next new one.
            (
                vec![("/a/x", "p"), ("/b/x", "p"), ("/c/x", "p")],
                vec![("/a/x", "p"), ("/c/x", "p"), ("/d/x", "p")],
                vec![("/a/x", "p/x"), ("/c/x", "p/x-3"), ("/d/x", "p/x-2")],
            ),
            // The freed unsuffixed slug is reused before any suffix.
            (
                vec![("/a/x", "p"), ("/b/x", "p")],
                vec![("/b/x", "p"), ("/z/x", "p")],
                vec![("/b/x", "p/x-2"), ("/z/x", "p/x")],
            ),
            // Nothing changed: nothing moves.
            (
                vec![("/a/x", "p"), ("/b/x", "p"), ("/c/y", "w")],
                vec![("/a/x", "p"), ("/b/x", "p"), ("/c/y", "w")],
                vec![("/a/x", "p/x"), ("/b/x", "p/x-2"), ("/c/y", "w/y")],
            ),
        ];
        for (before, now, expected) in cases {
            let mut old: Vec<_> = before.iter().map(|(p, c)| repo(p, c)).collect();
            assign_unique_slugs(&BTreeMap::new(), &mut old);
            let mut repos: Vec<_> = now.iter().map(|(p, c)| repo(p, c)).collect();
            assign_unique_slugs(&persisted(&old), &mut repos);
            assert_eq!(slugs(&repos), expected, "{before:?} -> {now:?}");
        }
    }

    #[test]
    fn existing_slugs_are_checked_against_context_and_name() {
        let existing = BTreeMap::from([
            (PathBuf::from("/a/x"), "p/x-7".to_owned()),
            (PathBuf::from("/b/x"), "p/x-07".to_owned()),
            (PathBuf::from("/c/x"), "p/xy".to_owned()),
            (PathBuf::from("/d/x"), "p/x-1".to_owned()),
            (PathBuf::from("/e/x"), "w/x".to_owned()),
            // Two paths claiming the same slug: the first path keeps it.
            (PathBuf::from("/f/y"), "p/y".to_owned()),
            (PathBuf::from("/g/y"), "p/y".to_owned()),
        ]);
        let mut repos: Vec<_> = ["/a/x", "/b/x", "/c/x", "/d/x", "/e/x", "/f/y", "/g/y"]
            .iter()
            .map(|path| repo(path, "p"))
            .collect();
        assign_unique_slugs(&existing, &mut repos);
        assert_eq!(
            slugs(&repos),
            [
                ("/a/x", "p/x-7"),
                ("/b/x", "p/x"),
                ("/c/x", "p/x-2"),
                ("/d/x", "p/x-3"),
                ("/e/x", "p/x-4"),
                ("/f/y", "p/y"),
                ("/g/y", "p/y-2"),
            ]
        );
    }

    #[test]
    fn slugs_do_not_depend_on_input_order() {
        let existing = BTreeMap::from([(PathBuf::from("/c/x"), "p/x".to_owned())]);
        let base: Vec<_> = ["/a/x", "/b/x", "/c/x", "/d/x-2"]
            .iter()
            .map(|path| repo(path, "p"))
            .collect();
        let mut forward = base.clone();
        assign_unique_slugs(&existing, &mut forward);
        let mut backward: Vec<_> = base.into_iter().rev().collect();
        assign_unique_slugs(&existing, &mut backward);
        backward.reverse();
        assert_eq!(forward, backward);
        let mut empty: Vec<RepoIdentity> = vec![];
        assign_unique_slugs(&existing, &mut empty);
    }
}
