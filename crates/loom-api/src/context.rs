//! Contexts: their names and how a repository is assigned to one.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Maximum length of a context name.
pub const CONTEXT_NAME_MAX_LEN: usize = 64;

/// Name shown for a repository that no rule assigns to a context. Reserved: no context can be
/// defined with it.
pub const UNASSIGNED: &str = "unassigned";

/// Name of a context, as given to `-c`/`--context` and as defined by `[contexts.<name>]`.
///
/// 1 to 64 characters: lowercase ASCII letters, digits and `-`, starting with a
/// letter and not ending with `-`. Paths and `key=value` pairs are rejected.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContextName(String);

/// Why a context name is invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextNameError {
    /// The name is empty.
    #[error("a context name is expected, got an empty string")]
    Empty,
    /// The name is longer than 64 characters.
    #[error("a context name has at most {CONTEXT_NAME_MAX_LEN} characters")]
    TooLong,
    /// The name looks like a path.
    #[error("a context name is expected, not a path: `{0}`")]
    Path(String),
    /// The name looks like a `key=value` pair.
    #[error("a context name is expected, not a `key=value` pair: `{0}`")]
    KeyValue(String),
    /// The name has a character outside the allowed set, or a bad first or last character.
    #[error(
        "a context name is expected (lowercase letters, digits and `-`, starting with a letter, not ending with `-`): `{0}`"
    )]
    Invalid(String),
}

impl FromStr for ContextName {
    type Err = ContextNameError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        if name.is_empty() {
            return Err(ContextNameError::Empty);
        }
        if name.contains('/') || name.starts_with('.') || name.starts_with('~') {
            return Err(ContextNameError::Path(name.to_owned()));
        }
        if name.contains('=') {
            return Err(ContextNameError::KeyValue(name.to_owned()));
        }
        if name.len() > CONTEXT_NAME_MAX_LEN {
            return Err(ContextNameError::TooLong);
        }
        let starts_with_letter = name.starts_with(|c: char| c.is_ascii_lowercase());
        let allowed = name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !starts_with_letter || !allowed || name.ends_with('-') {
            return Err(ContextNameError::Invalid(name.to_owned()));
        }
        Ok(Self(name.to_owned()))
    }
}

impl ContextName {
    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is the reserved name [`UNASSIGNED`].
    #[must_use]
    pub fn is_unassigned(&self) -> bool {
        self.0 == UNASSIGNED
    }
}

impl fmt::Display for ContextName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ContextName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ContextName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

impl JsonSchema for ContextName {
    fn schema_name() -> Cow<'static, str> {
        "ContextName".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Context name: 1 to 64 lowercase ASCII letters, digits and `-`, starting with a letter, not ending with `-`.",
            "type": "string",
            "minLength": 1,
            "maxLength": CONTEXT_NAME_MAX_LEN,
            "pattern": "^[a-z]([a-z0-9-]*[a-z0-9])?$"
        })
    }
}

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
    use super::{ContextAssignment, ContextName, ContextNameError, UNASSIGNED};

    #[test]
    fn valid_context_names() {
        for name in [
            "a",
            "personal",
            "company-a",
            "client-2",
            "unassigned",
            &"a".repeat(64),
        ] {
            assert_eq!(name.parse::<ContextName>().unwrap().as_str(), name);
        }
    }

    #[test]
    fn invalid_context_names() {
        let cases: [(&str, ContextNameError); 11] = [
            ("", ContextNameError::Empty),
            (&"a".repeat(65), ContextNameError::TooLong),
            ("./foo", ContextNameError::Path("./foo".into())),
            ("~/src", ContextNameError::Path("~/src".into())),
            ("src/foo", ContextNameError::Path("src/foo".into())),
            ("..", ContextNameError::Path("..".into())),
            ("a=b", ContextNameError::KeyValue("a=b".into())),
            ("Personal", ContextNameError::Invalid("Personal".into())),
            ("2fa", ContextNameError::Invalid("2fa".into())),
            ("a_b", ContextNameError::Invalid("a_b".into())),
            ("a-", ContextNameError::Invalid("a-".into())),
        ];
        for (name, expected) in cases {
            assert_eq!(name.parse::<ContextName>(), Err(expected), "{name:?}");
        }
    }

    #[test]
    fn context_name_round_trips_as_a_json_string() {
        let name: ContextName = "company-a".parse().unwrap();
        assert_eq!(serde_json::to_string(&name).unwrap(), "\"company-a\"");
        assert_eq!(
            serde_json::from_str::<ContextName>("\"company-a\"").unwrap(),
            name
        );
        let error = serde_json::from_str::<ContextName>("\"Company\"").unwrap_err();
        assert!(error.to_string().contains("`Company`"), "{error}");
    }

    #[test]
    fn unassigned_is_recognized() {
        assert!(UNASSIGNED.parse::<ContextName>().unwrap().is_unassigned());
        assert!(!"personal".parse::<ContextName>().unwrap().is_unassigned());
    }

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
