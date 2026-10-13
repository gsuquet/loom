//! Disk usage of the loom home, computed by loomd and returned in its status.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Timestamp;

/// Space a context store (`contexts/<name>/`) takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextDiskUsage {
    /// Name of the directory under `contexts/`.
    pub name: String,
    /// Allocated bytes of the directory and everything under it.
    pub bytes: u64,
}

/// Space the loom home takes on disk, by part.
///
/// Sizes are allocated bytes (blocks, not file lengths), symbolic links are counted as links
/// and never followed, and a file with several hard links in the home is counted once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiskUsage {
    /// Instant the sizes were measured; loomd keeps them for a while (see the API reference).
    pub computed_at: Timestamp,
    /// Every part below, added up: the whole loom home.
    pub total_bytes: u64,
    /// `versions/`: previous versions kept for `loom rollback`.
    pub versions_bytes: u64,
    /// `index.db`, `index.db-wal` and `index.db-shm`.
    pub index_bytes: u64,
    /// `index.db-wal` alone (included in `index_bytes`).
    pub index_wal_bytes: u64,
    /// `log/`.
    pub logs_bytes: u64,
    /// `state/`.
    pub state_bytes: u64,
    /// Everything else: the home directory itself, `config.toml`, `run/`, `staging/`,
    /// `contexts/` itself and files directly in it, unknown entries.
    pub other_bytes: u64,
    /// One entry per directory under `contexts/`, sorted by name; empty without context stores.
    pub contexts: Vec<ContextDiskUsage>,
    /// Names of the directories in `versions/`, sorted.
    pub kept_versions: Vec<String>,
    /// Bytes an unprivileged process can still write on the volume of the loom home;
    /// `null` when the volume could not be queried.
    pub available_bytes: Option<u64>,
    /// What could not be measured (a directory that cannot be read counts as 0), in English.
    pub warnings: Vec<String>,
}

impl DiskUsage {
    /// Sum of the context stores.
    #[must_use]
    pub fn contexts_bytes(&self) -> u64 {
        self.contexts
            .iter()
            .fold(0, |sum, context| sum.saturating_add(context.bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextDiskUsage, DiskUsage};
    use crate::Timestamp;
    use serde_json::json;

    fn sample() -> DiskUsage {
        DiskUsage {
            computed_at: Timestamp::from_unix_secs(1_791_792_000).unwrap(),
            total_bytes: 35_000_000,
            versions_bytes: 30_000_000,
            index_bytes: 4_200_000,
            index_wal_bytes: 4_096,
            logs_bytes: 24_576,
            state_bytes: 8_192,
            other_bytes: 767_232,
            contexts: vec![
                ContextDiskUsage {
                    name: "a".to_owned(),
                    bytes: 4_096,
                },
                ContextDiskUsage {
                    name: "b".to_owned(),
                    bytes: 8_192,
                },
            ],
            kept_versions: vec!["0.1.0".to_owned()],
            available_bytes: Some(1),
            warnings: vec![],
        }
    }

    #[test]
    fn serializes_every_field() {
        assert_eq!(
            serde_json::to_value(sample()).unwrap(),
            json!({
                "computed_at": "2026-10-12T08:00:00Z",
                "total_bytes": 35_000_000,
                "versions_bytes": 30_000_000,
                "index_bytes": 4_200_000,
                "index_wal_bytes": 4_096,
                "logs_bytes": 24_576,
                "state_bytes": 8_192,
                "other_bytes": 767_232,
                "contexts": [{ "name": "a", "bytes": 4_096 }, { "name": "b", "bytes": 8_192 }],
                "kept_versions": ["0.1.0"],
                "available_bytes": 1,
                "warnings": [],
            })
        );
    }

    #[test]
    fn contexts_add_up_without_overflow() {
        assert_eq!(sample().contexts_bytes(), 12_288);
        let mut huge = sample();
        huge.contexts[0].bytes = u64::MAX;
        assert_eq!(huge.contexts_bytes(), u64::MAX);
    }
}
