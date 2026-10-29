//! Repositories: identity, remotes, forge and update policy. Their state is in
//! [`crate::RepoState`].

use std::fmt;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ContextAssignment, LastUpdate, MaybeKnown, RepoState, Timestamp};

/// Forge that hosts a remote (`[forges."<host>"] kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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

/// Who can see a project on its forge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ForgeVisibility {
    /// Everyone.
    Public,
    /// Every signed-in user of the forge (GitLab, GitHub Enterprise).
    Internal,
    /// Only the members.
    Private,
}

impl ForgeVisibility {
    /// Lowercase name, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Internal => "internal",
            Self::Private => "private",
        }
    }
}

impl fmt::Display for ForgeVisibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The project of a repository on its forge, as the forge API gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ForgeProject {
    /// Numeric ID of the project on the forge (GitHub `databaseId`, GitLab `id`).
    pub id: u64,
    /// Default branch on the forge. `null` for an empty project.
    pub default_branch: Option<String>,
    /// Who can see the project.
    pub visibility: ForgeVisibility,
    /// The project is archived: it is read-only on the forge.
    pub archived: bool,
    /// URL of the project page.
    pub web_url: String,
    /// Number of open merge requests (pull requests on GitHub). `null` when the forge does not
    /// give the count (GitLab: above 10,000, or merge requests not visible).
    pub open_mrs: Option<u64>,
}

/// The forge metadata of a repository, read by `loomd` with the forge poll. Never used for
/// updates: the git state stays the source for branches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ForgeMetadata {
    /// When `loomd` last asked the forge.
    pub refreshed_at: Timestamp,
    /// The project, unknown when the forge call failed (the reason is in `error`).
    pub project: MaybeKnown<ForgeProject>,
    /// Why `project` is unknown, in English. `null` when it is known.
    pub error: Option<String>,
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
    /// Path without `/` at the start or at the end and without `.git`, such as `gsuquet/loom`.
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
    /// Normalized location. Absent for a local path or a URL loom cannot read.
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

/// A repository of the index: its identity and how loom knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepoSummary {
    /// Identity, its fields inlined.
    #[serde(flatten)]
    pub identity: RepoIdentity,
    /// Added by `POST /v1/repos/register`: a scan never removes it while it exists on disk.
    pub registered: bool,
    /// Last time the repository was used (`loom repo show`, or a `loom` command run inside
    /// it), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<Timestamp>,
    /// State (the repo card), `null` until it is read for the first time.
    #[serde(default)]
    pub state: Option<RepoState>,
    /// Result of the last update (fetch, fast-forward), `null` until the first one.
    #[serde(default)]
    pub last_update: Option<LastUpdate>,
    /// Forge metadata, `null` when no credential of the context gives access to the host of
    /// the primary remote, or before the first forge poll.
    #[serde(default)]
    pub forge_metadata: Option<ForgeMetadata>,
}

/// Repositories of the index, sorted by slug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepoList {
    /// The repositories.
    pub repos: Vec<RepoSummary>,
}

/// Body of `POST /v1/repos/register`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegisterRequest {
    /// Absolute path of the repository or of a directory inside it.
    pub path: PathBuf,
    /// Registration on use (a `loom` command run inside the repository). A repository already
    /// in the index is left as it is: only its `last_used` is set. A repository not in the
    /// index is registered. `last_used` is set to now in both cases.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub on_use: bool,
}

/// Outcome of a scan of the repository roots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScanResult {
    /// Repositories added to the index.
    pub added: u64,
    /// Repositories whose identity changed (remotes, context, forge or slug).
    pub updated: u64,
    /// Repositories removed from the index: no longer on disk.
    pub removed: u64,
    /// Paths that could not be read. The repositories below them are kept as they were.
    pub errors: Vec<ScanIssue>,
    /// Duration of the scan, in whole milliseconds.
    pub duration_ms: u64,
}

/// A path a scan could not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScanIssue {
    /// The root, directory or repository.
    pub path: PathBuf,
    /// What went wrong, in English.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::{ForgeKind, ForgeVisibility, RemoteUrl, RepoUpdatePolicy};

    #[test]
    fn names_match_serialization() {
        for kind in [ForgeKind::Github, ForgeKind::Gitlab] {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::Value::from(kind.as_str())
            );
        }
        for visibility in [
            ForgeVisibility::Public,
            ForgeVisibility::Internal,
            ForgeVisibility::Private,
        ] {
            assert_eq!(
                serde_json::to_value(visibility).unwrap(),
                serde_json::Value::from(visibility.to_string())
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
