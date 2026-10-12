//! Repositories: identity, remotes, forge and update policy.

use std::fmt;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ContextAssignment;

/// Forge hosting a remote (`[forges."<host>"] kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    /// GitHub or GitHub Enterprise.
    Github,
    /// GitLab, hosted or self-managed.
    Gitlab,
}

impl ForgeKind {
    /// Lowercase name, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
        }
    }
}

impl fmt::Display for ForgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How `loomd` updates a repository (`update` of a context or of `[repos."<path>"]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RepoUpdatePolicy {
    /// Fetch, then fast-forward when it is safe.
    Ff,
    /// Fetch only.
    Fetch,
    /// Neither fetch nor fast-forward.
    Off,
}

impl RepoUpdatePolicy {
    /// Name used in the configuration and the outputs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ff => "ff",
            Self::Fetch => "fetch",
            Self::Off => "off",
        }
    }
}

impl fmt::Display for RepoUpdatePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Network location of a remote, normalized: host lowercased (SSH aliases resolved), user,
/// default port and `.git` suffix removed, path case kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoteUrl {
    /// Host name, lowercase, such as `github.com`.
    pub host: String,
    /// Port, when not the default of the transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Path without leading or trailing `/` and without `.git`, such as `gsuquet/loom`.
    pub path: String,
}

/// `host[:port]/path`.
impl fmt::Display for RemoteUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.host)?;
        if let Some(port) = self.port {
            write!(f, ":{port}")?;
        }
        write!(f, "/{}", self.path)
    }
}

/// A `[remote "<name>"]` of a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Remote {
    /// Remote name, such as `origin`.
    pub name: String,
    /// URL as configured, without any password.
    pub url: String,
    /// Normalized location; absent for a local path or a URL loom cannot read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<RemoteUrl>,
}

/// Who a repository is: path, names, context, remotes and forge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepoIdentity {
    /// Absolute path, as found (symbolic links kept). The key of a repository.
    pub path: PathBuf,
    /// Directory name.
    pub name: String,
    /// `<context>/<name>`, with a numeric suffix (`-2`, `-3`…) when another repository has the
    /// same one. `unassigned/<name>` for an unassigned repository.
    pub slug: String,
    /// Context and the rule that assigned it.
    pub context: ContextAssignment,
    /// Every remote, in configuration order.
    pub remotes: Vec<Remote>,
    /// Name of the primary remote: `origin`, otherwise the first one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_remote: Option<String>,
    /// Forge of the primary remote's host, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<ForgeKind>,
}

#[cfg(test)]
mod tests {
    use super::{ForgeKind, RemoteUrl, RepoUpdatePolicy};

    #[test]
    fn names_match_serialization() {
        for kind in [ForgeKind::Github, ForgeKind::Gitlab] {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::Value::from(kind.as_str())
            );
        }
        for policy in [
            RepoUpdatePolicy::Ff,
            RepoUpdatePolicy::Fetch,
            RepoUpdatePolicy::Off,
        ] {
            assert_eq!(
                serde_json::to_value(policy).unwrap(),
                serde_json::Value::from(policy.to_string())
            );
        }
    }

    #[test]
    fn remote_url_displays_host_port_and_path() {
        let mut url = RemoteUrl {
            host: "gitlab.example".to_owned(),
            port: None,
            path: "group/sub/repo".to_owned(),
        };
        assert_eq!(url.to_string(), "gitlab.example/group/sub/repo");
        url.port = Some(2222);
        assert_eq!(url.to_string(), "gitlab.example:2222/group/sub/repo");
    }
}
