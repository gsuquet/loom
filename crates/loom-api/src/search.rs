//! Search results: `GET /v1/search` and `loom search --json`.

use std::fmt;
use std::path::PathBuf;

use loom_core::{ContextName, TaskId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Results a search returns when no limit is given.
pub const DEFAULT_SEARCH_LIMIT: usize = 20;

/// What a search result is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    /// A repository of the index.
    Repo,
    /// A task of a context store.
    Task,
}

impl SearchKind {
    /// Every kind.
    pub const ALL: [Self; 2] = [Self::Repo, Self::Task];

    /// Name, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Task => "task",
        }
    }
}

impl fmt::Display for SearchKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A searched field of a repository.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SearchField {
    /// Directory name.
    Name,
    /// `<context>/<name>`.
    Slug,
    /// Absolute path.
    Path,
    /// Remote URLs, as configured.
    Remotes,
    /// Context name, `unassigned` included.
    Context,
    /// Branch the repository is on, empty when detached or not read yet.
    Branch,
}

impl SearchField {
    /// Every field, in the order of the search table's columns.
    pub const ALL: [Self; 6] = [
        Self::Name,
        Self::Slug,
        Self::Path,
        Self::Remotes,
        Self::Context,
        Self::Branch,
    ];

    /// Name, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Slug => "slug",
            Self::Path => "path",
            Self::Remotes => "remotes",
            Self::Context => "context",
            Self::Branch => "branch",
        }
    }
}

impl fmt::Display for SearchField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A searched field of a task.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskSearchField {
    /// Title; empty when `task.md` is invalid.
    Title,
    /// The notes after the frontmatter; the whole file when `task.md` is invalid.
    Notes,
    /// Keys of the origin and related links (`CORE-1042`, `!88`).
    LinkKeys,
}

impl TaskSearchField {
    /// Every field, in the order of the search table's columns.
    pub const ALL: [Self; 3] = [Self::Title, Self::Notes, Self::LinkKeys];

    /// Name, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Notes => "notes",
            Self::LinkKeys => "link_keys",
        }
    }
}

impl fmt::Display for TaskSearchField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One match of a search, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchResult {
    /// A repository: `kind` is `repo`.
    Repo(RepoMatch),
    /// A task: `kind` is `task`.
    Task(TaskMatch),
}

impl SearchResult {
    /// What matched.
    #[must_use]
    pub const fn kind(&self) -> SearchKind {
        match self {
            Self::Repo(_) => SearchKind::Repo,
            Self::Task(_) => SearchKind::Task,
        }
    }
}

/// A repository that matches a search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RepoMatch {
    /// Slug of the repository.
    pub slug: String,
    /// Directory name of the repository.
    pub name: String,
    /// Context name, `unassigned` for a repository without context.
    pub context: String,
    /// Absolute path of the repository.
    pub path: PathBuf,
    /// Full-text relevance (SQLite FTS5 `bm25`, sign flipped): higher is more relevant. Only
    /// one of the ranking keys; the order of the results is the ranking.
    pub score: f64,
    /// Fields holding at least one of the query terms, in column order.
    pub matched_fields: Vec<SearchField>,
}

/// A task that matches a search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskMatch {
    /// ID of the task.
    pub id: TaskId,
    /// Context of its store.
    pub context: ContextName,
    /// Title; empty when `task.md` is invalid.
    pub title: String,
    /// Status as written in `task.md`, a status of a newer loom included; `null` when
    /// `task.md` is invalid.
    pub status: Option<String>,
    /// Keys of the origin and related links, in file order.
    pub link_keys: Vec<String>,
    /// Full-text relevance (SQLite FTS5 `bm25`, sign flipped): higher is more relevant. Only
    /// one of the ranking keys; the order of the results is the ranking.
    pub score: f64,
    /// Fields holding at least one of the query terms, in column order.
    pub matched_fields: Vec<TaskSearchField>,
}

/// Results of a search, best first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchResults {
    /// The results, at most the requested limit.
    pub results: Vec<SearchResult>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{RepoMatch, SearchField, SearchKind, SearchResult, TaskMatch, TaskSearchField};

    #[test]
    fn fields_and_kinds_serialize_as_their_names() {
        for field in SearchField::ALL {
            assert_eq!(
                serde_json::to_string(&field).unwrap(),
                format!("\"{field}\"")
            );
        }
        for field in TaskSearchField::ALL {
            assert_eq!(
                serde_json::to_string(&field).unwrap(),
                format!("\"{field}\"")
            );
        }
        for kind in SearchKind::ALL {
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{kind}\""));
        }
    }

    #[test]
    fn a_repo_result_keeps_its_v0_2_shape() {
        let result = SearchResult::Repo(RepoMatch {
            slug: "personal/loom".to_owned(),
            name: "loom".to_owned(),
            context: "personal".to_owned(),
            path: "/home/me/perso/loom".into(),
            score: 1.5,
            matched_fields: vec![SearchField::Name],
        });
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(
            value,
            json!({
                "kind": "repo",
                "slug": "personal/loom",
                "name": "loom",
                "context": "personal",
                "path": "/home/me/perso/loom",
                "score": 1.5,
                "matched_fields": ["name"],
            })
        );
        assert_eq!(
            serde_json::from_value::<SearchResult>(value).unwrap(),
            result
        );
        assert_eq!(result.kind(), SearchKind::Repo);
    }

    #[test]
    fn a_task_result_is_tagged_task() {
        let result = SearchResult::Task(TaskMatch {
            id: "k3q7zm2a".parse().unwrap(),
            context: "work".parse().unwrap(),
            title: "Login timeout".to_owned(),
            status: Some("active".to_owned()),
            link_keys: vec!["CORE-1042".to_owned()],
            score: 2.0,
            matched_fields: vec![TaskSearchField::LinkKeys],
        });
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(
            value,
            json!({
                "kind": "task",
                "id": "k3q7zm2a",
                "context": "work",
                "title": "Login timeout",
                "status": "active",
                "link_keys": ["CORE-1042"],
                "score": 2.0,
                "matched_fields": ["link_keys"],
            })
        );
        assert_eq!(
            serde_json::from_value::<SearchResult>(value).unwrap(),
            result
        );
        assert_eq!(result.kind(), SearchKind::Task);
    }
}
