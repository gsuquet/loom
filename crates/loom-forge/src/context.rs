//! Context of an MR (ADR-0016 decision 1): the context of an indexed clone of its target
//! project, then `match.hosts`, then the context whose credential found it. Never
//! `default_context`.

use loom_api::{ContextAssignment, ContextName, RemoteUrl};
use loom_repo::ContextRules;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The context of an MR and the rule that chose it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum MrContext {
    /// An indexed clone of the target project is in this context, by a repository override,
    /// `match.hosts` or `match.paths`.
    Clone {
        /// The context.
        context: ContextName,
    },
    /// An entry of `match.hosts` matches `<host>/<project path>` of the MR.
    Host {
        /// The context.
        context: ContextName,
        /// The entry that matches, `host` or `host/path-prefix`, as normalized.
        pattern: String,
    },
    /// No clone and no `match.hosts` entry: the context whose credential found the MR.
    Finder {
        /// The context.
        context: ContextName,
    },
}

impl MrContext {
    /// The context.
    #[must_use]
    pub const fn context(&self) -> &ContextName {
        match self {
            Self::Clone { context } | Self::Host { context, .. } | Self::Finder { context } => {
                context
            }
        }
    }
}

/// What decides the context of an MR, other than its host and project path.
#[derive(Debug, Clone, Copy)]
pub struct MrContexts<'a> {
    /// The context assignment of the indexed clone of the target project, when there is one.
    /// The caller chooses the clone (the clone whose remote names the project; with several,
    /// the one that the refs use).
    pub clone: Option<&'a ContextAssignment>,
    /// The context rules of the configuration.
    pub rules: &'a ContextRules,
    /// The context whose credential found the MR.
    pub finder: &'a ContextName,
}

/// The context of the MR of project `path` on `host` (a host name without port, as in
/// `match.hosts`), first match wins:
///
/// 1. the context of the indexed clone, when the clone has one by a repository override,
///    `match.hosts` or `match.paths`; a clone in `default_context` or unassigned does not count;
/// 2. the `match.hosts` entry with the longest prefix of `<host>/<path>`;
/// 3. the context whose credential found the MR.
///
/// `default_context` is never used: a client MR found by a client token must not go to the
/// personal store. Pure.
#[must_use]
pub fn mr_context(host: &str, path: &str, contexts: &MrContexts<'_>) -> MrContext {
    if let Some(context) = contexts.clone.and_then(clone_context) {
        return MrContext::Clone {
            context: context.clone(),
        };
    }
    let project = RemoteUrl {
        host: host.to_ascii_lowercase(),
        port: None,
        path: path.to_owned(),
    };
    match contexts.rules.assign_host(&project) {
        Some(ContextAssignment::Host { context, pattern }) => MrContext::Host { context, pattern },
        Some(
            ContextAssignment::Override { .. }
            | ContextAssignment::Path { .. }
            | ContextAssignment::Default { .. }
            | ContextAssignment::Unassigned,
        )
        | None => MrContext::Finder {
            context: contexts.finder.clone(),
        },
    }
}

const fn clone_context(assignment: &ContextAssignment) -> Option<&ContextName> {
    match assignment {
        ContextAssignment::Override { context }
        | ContextAssignment::Host { context, .. }
        | ContextAssignment::Path { context, .. } => Some(context),
        ContextAssignment::Default { .. } | ContextAssignment::Unassigned => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{MrContext, MrContexts, mr_context};
    use loom_api::{ContextAssignment, ContextName};
    use loom_config::config::parse_config;
    use loom_repo::ContextRules;
    use std::path::Path;

    const CONFIG: &str = r#"
default_context = "personal"

[contexts.personal]
match.paths = ["~/perso"]
match.hosts = ["github.com/gsuquet"]

[contexts.company-a]
match.paths = ["~/work/company-a"]
match.hosts = ["gitlab.com/company-a", "gitlab.company-a.example"]

[contexts.infra]
match.hosts = ["gitlab.com/company-a/infra"]

[contexts.company-b]
match.paths = ["~/work/company-b"]
"#;

    fn name(name: &str) -> ContextName {
        name.parse().unwrap()
    }

    fn clone(rule: &str, context: &str) -> ContextAssignment {
        if rule == "unassigned" {
            return ContextAssignment::Unassigned;
        }
        let context = name(context);
        match rule {
            "override" => ContextAssignment::Override { context },
            "host" => ContextAssignment::Host {
                context,
                pattern: "gitlab.com/company-a".to_owned(),
            },
            "path" => ContextAssignment::Path {
                context,
                prefix: "~/work/company-b".to_owned(),
            },
            _ => ContextAssignment::Default { context },
        }
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "one row per rule")]
    fn context_rule_table() {
        let config = parse_config(CONFIG, Path::new("config.toml")).unwrap();
        let rules = ContextRules::new(&config, Path::new("/home/me")).unwrap();
        let host = |context: &str, pattern: &str| MrContext::Host {
            context: name(context),
            pattern: pattern.to_owned(),
        };
        let finder = |context: &str| MrContext::Finder {
            context: name(context),
        };
        let by_clone = |context: &str| MrContext::Clone {
            context: name(context),
        };
        // (host, project path, clone assignment, finding context, expected)
        let table: Vec<(&str, &str, Option<ContextAssignment>, &str, MrContext)> = vec![
            // 1. The clone context wins over `match.hosts`, whatever rule placed the clone.
            (
                "gitlab.com",
                "company-a/checkout",
                Some(clone("path", "company-b")),
                "company-a",
                by_clone("company-b"),
            ),
            (
                "gitlab.com",
                "company-a/checkout",
                Some(clone("override", "personal")),
                "company-a",
                by_clone("personal"),
            ),
            (
                "gitlab.com",
                "company-a/checkout",
                Some(clone("host", "company-a")),
                "personal",
                by_clone("company-a"),
            ),
            // A clone in `default_context` or unassigned does not count: `match.hosts` decides.
            (
                "gitlab.com",
                "company-a/checkout",
                Some(clone("default", "personal")),
                "personal",
                host("company-a", "gitlab.com/company-a"),
            ),
            (
                "gitlab.com",
                "company-a/checkout",
                Some(clone("unassigned", "")),
                "personal",
                host("company-a", "gitlab.com/company-a"),
            ),
            // 2. `match.hosts`: longest prefix, whole segments, case of the path and host ignored.
            (
                "gitlab.com",
                "company-a/infra/terraform",
                None,
                "personal",
                host("infra", "gitlab.com/company-a/infra"),
            ),
            (
                "GitLab.com",
                "Company-A/checkout",
                None,
                "personal",
                host("company-a", "gitlab.com/company-a"),
            ),
            (
                "gitlab.company-a.example",
                "team/app",
                None,
                "personal",
                host("company-a", "gitlab.company-a.example"),
            ),
            (
                "github.com",
                "gsuquet/loom",
                None,
                "company-a",
                host("personal", "github.com/gsuquet"),
            ),
            // 3. No match: the finding context, never `default_context`.
            (
                "gitlab.com",
                "company-ab/checkout",
                None,
                "company-a",
                finder("company-a"),
            ),
            (
                "github.com",
                "acme/shop",
                None,
                "company-b",
                finder("company-b"),
            ),
            (
                "github.com",
                "acme/shop",
                Some(clone("default", "personal")),
                "company-b",
                finder("company-b"),
            ),
        ];
        for (host_name, path, clone, found_by, expected) in table {
            let finder = name(found_by);
            let contexts = MrContexts {
                clone: clone.as_ref(),
                rules: &rules,
                finder: &finder,
            };
            let context = mr_context(host_name, path, &contexts);
            assert_eq!(context, expected, "{host_name}/{path} {clone:?} {found_by}");
        }
    }

    #[test]
    fn serializes_with_its_rule() {
        let context = MrContext::Host {
            context: name("company-a"),
            pattern: "gitlab.com/company-a".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&context).unwrap(),
            serde_json::json!({"rule": "host", "context": "company-a", "pattern": "gitlab.com/company-a"})
        );
        assert_eq!(context.context(), &name("company-a"));
    }
}
