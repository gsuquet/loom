//! How a repository is assigned to a context.

use loom_core::{ContextName, UNASSIGNED};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The context of a repository and the rule that chose it, first match wins:
/// override, host, path, default context, otherwise unassigned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum ContextAssignment {
    /// `[repos."<path>"] context` names the context of this repository.
    Override {
        /// Assigned context.
        context: ContextName,
    },
    /// An entry of `match.hosts` matches the host and path of the primary remote.
    Host {
        /// Assigned context.
        context: ContextName,
        /// The matching entry, `host` or `host/path-prefix`, as normalized.
        pattern: String,
    },
    /// An entry of `match.paths` is the longest prefix of the repository path.
    Path {
        /// Assigned context.
        context: ContextName,
        /// The matching entry, as written in the configuration (before `~` expansion).
        prefix: String,
    },
    /// No rule matches; `default_context` is set.
    Default {
        /// Assigned context.
        context: ContextName,
    },
    /// No rule matches and no `default_context` is set.
    Unassigned,
}

impl ContextAssignment {
    /// The assigned context; `None` when unassigned.
    #[must_use]
    pub const fn context(&self) -> Option<&ContextName> {
        match self {
            Self::Override { context }
            | Self::Host { context, .. }
            | Self::Path { context, .. }
            | Self::Default { context } => Some(context),
            Self::Unassigned => None,
        }
    }

    /// Name of the assigned context, or [`UNASSIGNED`].
    #[must_use]
    pub fn context_name(&self) -> &str {
        self.context().map_or(UNASSIGNED, ContextName::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextAssignment, ContextName, UNASSIGNED};

    #[test]
    fn assignment_names_its_context() {
        let personal: ContextName = "personal".parse().unwrap();
        for assignment in [
            ContextAssignment::Override {
                context: personal.clone(),
            },
            ContextAssignment::Host {
                context: personal.clone(),
                pattern: "github.com/me".to_owned(),
            },
            ContextAssignment::Path {
                context: personal.clone(),
                prefix: "~/perso".to_owned(),
            },
            ContextAssignment::Default {
                context: personal.clone(),
            },
        ] {
            assert_eq!(assignment.context(), Some(&personal));
            assert_eq!(assignment.context_name(), "personal");
        }
        assert_eq!(ContextAssignment::Unassigned.context(), None);
        assert_eq!(ContextAssignment::Unassigned.context_name(), UNASSIGNED);
    }

    #[test]
    fn assignment_is_tagged_by_rule() {
        let assignment = ContextAssignment::Host {
            context: "company-a".parse().unwrap(),
            pattern: "gitlab.com/company-a".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&assignment).unwrap(),
            serde_json::json!({
                "rule": "host",
                "context": "company-a",
                "pattern": "gitlab.com/company-a"
            })
        );
        assert_eq!(
            serde_json::to_value(ContextAssignment::Unassigned).unwrap(),
            serde_json::json!({ "rule": "unassigned" })
        );
    }
}
