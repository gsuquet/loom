//! Search results: `GET /v1/search` and `loom search --json`.

use std::fmt;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Results a search returns when no limit is given.
pub const DEFAULT_SEARCH_LIMIT: usize = 20;

/// What a search result is. Repositories only for now; tasks and notes join in v0.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    /// A repository of the index.
    Repo,
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

/// One match of a search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchResult {
    /// What matched.
    pub kind: SearchKind,
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

/// Results of a search, best first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchResults {
    /// The results, at most the requested limit.
    pub results: Vec<SearchResult>,
}

#[cfg(test)]
mod tests {
    use super::{SearchField, SearchKind};

    #[test]
    fn fields_and_kinds_serialize_as_their_names() {
        for field in SearchField::ALL {
            assert_eq!(
                serde_json::to_string(&field).unwrap(),
                format!("\"{field}\"")
            );
        }
        assert_eq!(
            serde_json::to_string(&SearchKind::Repo).unwrap(),
            "\"repo\""
        );
    }
}
