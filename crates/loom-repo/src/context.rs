//! Context of a repository: override, `match.hosts`, `match.paths`, `default_context`,
//! otherwise unassigned (ADR-0012).

use std::path::{Path, PathBuf};

use loom_api::{ContextAssignment, ContextName, RemoteUrl};
use loom_config::config::Config;
use loom_config::repos::HostPattern;
use thiserror::Error;

/// Two entries that name the same directory only once `~` is expanded, such as `~/work` and
/// `/home/me/work`. Equal spellings are rejected when the configuration is read.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextRulesError {
    /// A `match.paths` entry of two contexts.
    #[error(
        "match.paths `{first}` of context `{first_context}` and `{second}` of context `{second_context}` are the same directory {path}: an entry can belong to one context only"
    )]
    PathTie {
        /// The directory, expanded.
        path: PathBuf,
        /// First entry, as written.
        first: String,
        /// Its context.
        first_context: ContextName,
        /// Second entry, as written.
        second: String,
        /// Its context.
        second_context: ContextName,
    },
    /// Two `[repos."<path>"]` tables for the same repository.
    #[error("[repos.\"{first}\"] and [repos.\"{second}\"] are the same repository {path}")]
    OverrideTie {
        /// The repository path, expanded.
        path: PathBuf,
        /// First key, as written.
        first: String,
        /// Second key, as written.
        second: String,
    },
}

/// A `match.paths` entry, expanded.
#[derive(Debug, Clone)]
struct PathRule {
    path: PathBuf,
    written: String,
    context: ContextName,
}

/// The context rules of a configuration, with `~` expanded, ready to assign repositories.
#[derive(Debug, Clone)]
pub struct ContextRules {
    /// `[repos."<path>"] context`, expanded.
    overrides: Vec<(PathBuf, ContextName)>,
    hosts: Vec<(HostPattern, ContextName)>,
    paths: Vec<PathRule>,
    default: Option<ContextName>,
}

impl ContextRules {
    /// The rules of `config`, with `~` expanded to `home`. Pure.
    ///
    /// # Errors
    ///
    /// [`ContextRulesError`] when two `match.paths` entries of different contexts, or two
    /// `[repos."<path>"]` keys, name the same directory once expanded.
    pub fn new(config: &Config, home: &Path) -> Result<Self, ContextRulesError> {
        let mut paths: Vec<PathRule> = Vec::new();
        let mut hosts = Vec::new();
        for (name, context) in &config.contexts {
            for entry in &context.matching.paths {
                let rule = PathRule {
                    path: entry.get_ref().expand(home),
                    written: entry.get_ref().as_str().to_owned(),
                    context: name.clone(),
                };
                if let Some(first) = paths
                    .iter()
                    .find(|first| first.path == rule.path && first.context != rule.context)
                {
                    return Err(ContextRulesError::PathTie {
                        path: rule.path,
                        first: first.written.clone(),
                        first_context: first.context.clone(),
                        second: rule.written,
                        second_context: rule.context,
                    });
                }
                paths.push(rule);
            }
            for entry in &context.matching.hosts {
                hosts.push((entry.get_ref().clone(), name.clone()));
            }
        }

        let mut overrides: Vec<(PathBuf, ContextName)> = Vec::new();
        let mut seen: Vec<(PathBuf, &str)> = Vec::new();
        for repo in &config.repos.overrides {
            let path = repo.path.get_ref().expand(home);
            let written = repo.path.get_ref().as_str();
            if let Some((_, first)) = seen.iter().find(|(seen, _)| *seen == path) {
                return Err(ContextRulesError::OverrideTie {
                    path,
                    first: (*first).to_owned(),
                    second: written.to_owned(),
                });
            }
            seen.push((path.clone(), written));
            if let Some(context) = &repo.context {
                overrides.push((path, context.get_ref().clone()));
            }
        }

        Ok(Self {
            overrides,
            hosts,
            paths,
            default: config
                .default_context
                .as_ref()
                .map(|name| name.get_ref().clone()),
        })
    }

    /// Context of the repository at `repo_path` (absolute, as found) whose primary remote is
    /// at `primary`, first match wins:
    ///
    /// 1. `[repos."<path>"] context` for exactly this path;
    /// 2. the `match.hosts` entry with the host of `primary` and the longest path prefix
    ///    (whole segments, ASCII case ignored);
    /// 3. the `match.paths` entry that is the longest prefix of `repo_path` (whole components);
    /// 4. `default_context`;
    /// 5. unassigned.
    ///
    /// Pure.
    #[must_use]
    pub fn assign(&self, repo_path: &Path, primary: Option<&RemoteUrl>) -> ContextAssignment {
        if let Some((_, context)) = self.overrides.iter().find(|(path, _)| path == repo_path) {
            return ContextAssignment::Override {
                context: context.clone(),
            };
        }
        if let Some(remote) = primary {
            let best = longest(
                self.hosts
                    .iter()
                    .filter(|(pattern, _)| host_matches(pattern, remote))
                    .map(|rule| (rule.0.segments().len(), rule)),
            );
            if let Some((pattern, context)) = best {
                return ContextAssignment::Host {
                    context: context.clone(),
                    pattern: pattern.to_string(),
                };
            }
        }
        let best = longest(
            self.paths
                .iter()
                .filter(|rule| repo_path.starts_with(&rule.path))
                .map(|rule| (rule.path.components().count(), rule)),
        );
        if let Some(rule) = best {
            return ContextAssignment::Path {
                context: rule.context.clone(),
                prefix: rule.written.clone(),
            };
        }
        self.default
            .as_ref()
            .map_or(ContextAssignment::Unassigned, |context| {
                ContextAssignment::Default {
                    context: context.clone(),
                }
            })
    }
}

/// The item with the greatest length; the first one on a tie (excluded by validation).
fn longest<T>(items: impl Iterator<Item = (usize, T)>) -> Option<T> {
    items
        .fold(
            None,
            |best: Option<(usize, T)>, (length, item)| match best {
                Some((best_length, _)) if best_length >= length => best,
                _ => Some((length, item)),
            },
        )
        .map(|(_, item)| item)
}

/// Whether `pattern` matches `remote`: same host, and its segments are the first segments of
/// the remote path, ASCII case ignored (forges ignore the case of paths).
fn host_matches(pattern: &HostPattern, remote: &RemoteUrl) -> bool {
    if pattern.host() != remote.host {
        return false;
    }
    let mut path = remote.path.split('/');
    pattern.segments().iter().all(|segment| {
        path.next()
            .is_some_and(|part| part.eq_ignore_ascii_case(segment))
    })
}

#[cfg(test)]
mod tests {
    use super::{ContextRules, ContextRulesError};
    use loom_api::{ContextAssignment, ContextName, RemoteUrl};
    use loom_config::config::parse_config;
    use std::path::{Path, PathBuf};

    const HOME: &str = "/home/me";

    fn rules(text: &str) -> ContextRules {
        let config = parse_config(text, Path::new("config.toml")).unwrap();
        ContextRules::new(&config, Path::new(HOME)).unwrap()
    }

    fn name(name: &str) -> ContextName {
        name.parse().unwrap()
    }

    fn remote(host: &str, path: &str) -> RemoteUrl {
        RemoteUrl {
            host: host.to_owned(),
            port: None,
            path: path.to_owned(),
        }
    }

    fn host(context: &str, pattern: &str) -> ContextAssignment {
        ContextAssignment::Host {
            context: name(context),
            pattern: pattern.to_owned(),
        }
    }

    fn path(context: &str, prefix: &str) -> ContextAssignment {
        ContextAssignment::Path {
            context: name(context),
            prefix: prefix.to_owned(),
        }
    }

    const CONFIG: &str = r#"
default_context = "personal"

[contexts.personal]
match.paths = ["~/perso"]
match.hosts = ["github.com/gsuquet"]

[contexts.company-a]
match.paths = ["~/work/company-a", "/srv/company-a"]
match.hosts = ["gitlab.com/company-a", "gitlab.company-a.example"]

[contexts.company-a-infra]
match.paths = ["~/work/company-a/infra"]
match.hosts = ["gitlab.com/company-a/infra"]

[contexts.oss]
match.hosts = ["github.com"]

[repos."~/perso/client-fork"]
context = "company-a"

[repos."~/work/company-a/shared"]
update = "off"
"#;

    #[test]
    fn assignment_follows_the_rule_order() {
        let rules = rules(CONFIG);
        let gitlab_a = remote("gitlab.com", "company-a/app");
        let github_me = remote("github.com", "gsuquet/loom");
        let cases: Vec<(&str, Option<RemoteUrl>, ContextAssignment)> = vec![
            // 1. Override beats hosts and paths.
            (
                "/home/me/perso/client-fork",
                Some(github_me.clone()),
                ContextAssignment::Override {
                    context: name("company-a"),
                },
            ),
            // An override table without `context` does not assign.
            (
                "/home/me/work/company-a/shared",
                None,
                path("company-a", "~/work/company-a"),
            ),
            // 2. Hosts beat paths.
            (
                "/home/me/perso/loom",
                Some(gitlab_a),
                host("company-a", "gitlab.com/company-a"),
            ),
            (
                "/tmp/x",
                Some(github_me),
                host("personal", "github.com/gsuquet"),
            ),
            // Longest host prefix wins, whatever the order of contexts.
            (
                "/tmp/x",
                Some(remote("gitlab.com", "company-a/infra/terraform")),
                host("company-a-infra", "gitlab.com/company-a/infra"),
            ),
            (
                "/tmp/x",
                Some(remote("github.com", "rust-lang/rust")),
                host("oss", "github.com"),
            ),
            // A host alone matches every path on it.
            (
                "/tmp/x",
                Some(remote("gitlab.company-a.example", "any/thing")),
                host("company-a", "gitlab.company-a.example"),
            ),
            // Path case ignored for hosts.
            (
                "/tmp/x",
                Some(remote("github.com", "GSuquet/Loom")),
                host("personal", "github.com/gsuquet"),
            ),
            // 3. No host matches: paths, longest prefix.
            (
                "/home/me/work/company-a/app",
                Some(remote("bitbucket.org", "a/b")),
                path("company-a", "~/work/company-a"),
            ),
            (
                "/home/me/work/company-a/infra/terraform",
                None,
                path("company-a-infra", "~/work/company-a/infra"),
            ),
            (
                "/srv/company-a/mirror",
                None,
                path("company-a", "/srv/company-a"),
            ),
            // The prefix itself is a repository.
            ("/home/me/perso", None, path("personal", "~/perso")),
            // 4. Nothing matches: default context.
            (
                "/home/me/work/other/app",
                Some(remote("gitlab.com", "company-b/app")),
                ContextAssignment::Default {
                    context: name("personal"),
                },
            ),
        ];
        for (repo, primary, expected) in cases {
            assert_eq!(
                rules.assign(Path::new(repo), primary.as_ref()),
                expected,
                "{repo} {primary:?}"
            );
        }
    }

    #[test]
    fn prefixes_match_whole_segments_only() {
        let rules = rules(
            r#"
[contexts.a]
match.paths = ["~/work/company-a"]
match.hosts = ["gitlab.com/company-a"]
"#,
        );
        for (repo, primary) in [
            ("/home/me/work/company-ab/x", None),
            ("/home/me/work/company", None),
            ("/tmp/x", Some(remote("gitlab.com", "company-ab/x"))),
            ("/tmp/x", Some(remote("gitlab.com", "company"))),
            (
                "/tmp/x",
                Some(remote("gitlab.company-a.example", "company-a/x")),
            ),
            ("/tmp/x", Some(remote("github.com", "company-a/x"))),
        ] {
            assert_eq!(
                rules.assign(Path::new(repo), primary.as_ref()),
                ContextAssignment::Unassigned,
                "{repo} {primary:?}"
            );
        }
        // The exact prefix matches.
        assert_eq!(
            rules.assign(
                Path::new("/tmp/x"),
                Some(&remote("gitlab.com", "company-a"))
            ),
            host("a", "gitlab.com/company-a")
        );
    }

    #[test]
    fn unassigned_without_default_context() {
        let some = rules("[contexts.a]\nmatch.paths = [\"~/a\"]\n");
        assert_eq!(
            some.assign(Path::new("/home/me/b/x"), None),
            ContextAssignment::Unassigned
        );
        let empty = rules("");
        assert_eq!(
            empty.assign(Path::new("/home/me/x"), Some(&remote("github.com", "a/b"))),
            ContextAssignment::Unassigned
        );
    }

    #[test]
    fn overrides_match_the_exact_expanded_path() {
        let rules = rules(
            "[contexts.a]\n[repos.\"~/x/\"]\ncontext = \"a\"\n[repos.\"/srv/y\"]\ncontext = \"a\"\n",
        );
        let over = ContextAssignment::Override { context: name("a") };
        assert_eq!(rules.assign(Path::new("/home/me/x"), None), over);
        assert_eq!(rules.assign(Path::new("/home/me/x/"), None), over);
        assert_eq!(rules.assign(Path::new("/srv/y"), None), over);
        assert_eq!(
            rules.assign(Path::new("/home/me/x/sub"), None),
            ContextAssignment::Unassigned
        );
    }

    #[test]
    fn ties_revealed_by_expansion_are_errors() {
        let config = parse_config(
            "[contexts.a]\nmatch.paths = [\"~/work\"]\n[contexts.b]\nmatch.paths = [\"/home/me/work/\"]\n",
            Path::new("config.toml"),
        )
        .unwrap();
        assert_eq!(
            ContextRules::new(&config, Path::new(HOME)).unwrap_err(),
            ContextRulesError::PathTie {
                path: PathBuf::from("/home/me/work"),
                first: "~/work".to_owned(),
                first_context: name("a"),
                second: "/home/me/work/".to_owned(),
                second_context: name("b"),
            }
        );
        // Another home: no tie.
        assert!(ContextRules::new(&config, Path::new("/Users/me")).is_ok());

        let config = parse_config(
            "[repos.\"~/x\"]\nupdate = \"off\"\n[repos.\"/home/me/x\"]\nupdate = \"ff\"\n",
            Path::new("config.toml"),
        )
        .unwrap();
        let error = ContextRules::new(&config, Path::new(HOME)).unwrap_err();
        assert_eq!(
            error,
            ContextRulesError::OverrideTie {
                path: PathBuf::from("/home/me/x"),
                first: "/home/me/x".to_owned(),
                second: "~/x".to_owned(),
            }
        );
        assert!(error.to_string().contains("same repository /home/me/x"));
    }

    #[test]
    fn the_same_path_twice_in_one_context_is_not_a_tie() {
        let config = parse_config(
            "[contexts.a]\nmatch.paths = [\"~/work\", \"/home/me/work\"]\n",
            Path::new("config.toml"),
        )
        .unwrap();
        let rules = ContextRules::new(&config, Path::new(HOME)).unwrap();
        assert_eq!(
            rules.assign(Path::new("/home/me/work/x"), None),
            path("a", "~/work")
        );
    }
}
