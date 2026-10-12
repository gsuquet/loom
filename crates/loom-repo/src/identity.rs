//! Identity of a repository: names, remotes, forge and context, and unique slugs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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

/// Makes the slugs of `repos` unique. Pure.
///
/// Repositories are taken in path order: the first one with a slug keeps it, the next ones
/// get `-2`, `-3`… skipping any slug already taken, so the result does not depend on the
/// order of `repos`. Expects the `<context>/<name>` slugs of [`identify`].
pub fn assign_unique_slugs(repos: &mut [RepoIdentity]) {
    let mut taken: BTreeSet<String> = repos.iter().map(|repo| repo.slug.clone()).collect();
    let mut order: Vec<usize> = (0..repos.len()).collect();
    order.sort_by(|a, b| repos[*a].path.cmp(&repos[*b].path));
    let mut kept = BTreeSet::new();
    for index in order {
        let slug = &repos[index].slug;
        if kept.insert(slug.clone()) {
            continue;
        }
        // At most `taken.len()` candidates are taken: one of the first `taken.len() + 1` is free.
        let unique = (2..=taken.len() + 2)
            .map(|n| format!("{slug}-{n}"))
            .find(|candidate| !taken.contains(candidate))
            .unwrap_or_else(|| slug.clone());
        taken.insert(unique.clone());
        kept.insert(unique.clone());
        repos[index].slug = unique;
    }
}

#[cfg(test)]
mod tests {
    use super::{assign_unique_slugs, identify};
    use crate::ContextRules;
    use loom_api::{ContextAssignment, ForgeKind, Remote, RemoteUrl, RepoIdentity};
    use loom_config::config::parse_config;
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

    fn repo(path: &str, slug: &str) -> RepoIdentity {
        RepoIdentity {
            path: PathBuf::from(path),
            name: slug.rsplit('/').next().unwrap_or_default().to_owned(),
            slug: slug.to_owned(),
            context: ContextAssignment::Unassigned,
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

    #[test]
    fn duplicate_slugs_get_a_numeric_suffix_in_path_order() {
        let mut repos = vec![
            repo("/c/x", "p/x"),
            repo("/a/x", "p/x"),
            repo("/b/x", "p/x"),
            repo("/d/y", "p/y"),
            repo("/e/y", "w/y"),
        ];
        assign_unique_slugs(&mut repos);
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
    fn suffixes_skip_slugs_already_taken() {
        let mut repos = vec![
            repo("/b/x", "p/x"),
            repo("/a/x", "p/x"),
            repo("/z/x-2", "p/x-2"),
        ];
        assign_unique_slugs(&mut repos);
        assert_eq!(
            slugs(&repos),
            [("/b/x", "p/x-3"), ("/a/x", "p/x"), ("/z/x-2", "p/x-2")]
        );
    }

    #[test]
    fn unique_slugs_do_not_depend_on_input_order() {
        let base = vec![
            repo("/a/x", "p/x"),
            repo("/b/x", "p/x"),
            repo("/c/x", "p/x"),
            repo("/d/x-2", "p/x-2"),
        ];
        let mut forward = base.clone();
        assign_unique_slugs(&mut forward);
        let mut backward: Vec<_> = base.into_iter().rev().collect();
        assign_unique_slugs(&mut backward);
        backward.reverse();
        assert_eq!(forward, backward);
        let mut empty: Vec<RepoIdentity> = vec![];
        assign_unique_slugs(&mut empty);
    }
}
