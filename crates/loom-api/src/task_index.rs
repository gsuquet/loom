//! The tasks index: `POST /v1/tasks/reindex` and `loom reindex --json`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use loom_core::{ContextName, TaskId};

/// What a reindex wrote: the tasks index rebuilt from the files of every context store.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReindexResult {
    /// Tasks indexed, invalid ones included.
    pub tasks: usize,
    /// Tasks whose `task.md` cannot be read: indexed read-only, the file left as it is. Sorted
    /// by context and ID.
    pub invalid: Vec<InvalidTaskFile>,
    /// Tasks, or stores, that could not be read (permissions, I/O): they keep the row they had,
    /// the other tasks are indexed. Sorted by path.
    #[serde(default)]
    pub failed: Vec<FailedTaskFile>,
}

/// A task directory, or the `tasks/` directory of a store, that could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FailedTaskFile {
    /// The task directory, or `tasks/` when the store could not be listed.
    pub path: PathBuf,
    /// What failed.
    pub error: String,
}

/// A task whose `task.md` cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InvalidTaskFile {
    /// Context of its store.
    pub context: ContextName,
    /// ID, the name of its directory.
    pub id: TaskId,
    /// Why, with the line when known.
    pub error: String,
}
