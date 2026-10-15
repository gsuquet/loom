//! Decisions of the task store of loomd, without I/O: the response of a reindex, the task a
//! changed path belongs to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use loom_api::{ContextName, FailedTaskFile, InvalidTaskFile, ReindexResult, TaskId};
use loom_store::files::TASKS_DIR;
use loom_store::port::task_dir_id;
use loom_store::tasks::{IndexedState, Reindexed, TaskFailure};

/// What a reindex reports: every row counted, the invalid ones listed by context and ID, the
/// failures with their path under `stores` (root of each context store).
#[must_use]
pub fn reindex_result(
    reindexed: &Reindexed,
    stores: &BTreeMap<ContextName, PathBuf>,
) -> ReindexResult {
    let mut invalid: Vec<InvalidTaskFile> = reindexed
        .rows
        .iter()
        .filter_map(|row| match &row.state {
            IndexedState::Invalid { error, .. } => Some(InvalidTaskFile {
                context: row.context.clone(),
                id: row.id.clone(),
                error: error.clone(),
            }),
            IndexedState::Valid { .. } => None,
        })
        .collect();
    invalid.sort_by(|a, b| (&a.context, &a.id).cmp(&(&b.context, &b.id)));
    ReindexResult {
        tasks: reindexed.rows.len(),
        invalid,
        failed: failed_files(&reindexed.failed, stores),
    }
}

/// `failures` with their path: the task directory, or `tasks/` of a store that could not be
/// listed. Sorted by path, each path once (its first error).
#[must_use]
pub fn failed_files(
    failures: &[TaskFailure],
    stores: &BTreeMap<ContextName, PathBuf>,
) -> Vec<FailedTaskFile> {
    let mut files: Vec<FailedTaskFile> = failures
        .iter()
        .map(|failure| {
            let tasks = stores
                .get(&failure.context)
                .map_or_else(|| PathBuf::from(TASKS_DIR), |root| root.join(TASKS_DIR));
            FailedTaskFile {
                path: failure
                    .id
                    .as_ref()
                    .map_or_else(|| tasks.clone(), |id| tasks.join(id.as_str())),
                error: failure.error.clone(),
            }
        })
        .collect();
    // Stable: the first error of a path stays.
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|later, first| later.path == first.path);
    files
}

/// The task a changed `path` belongs to, if any.
///
/// A task path is under `<root>/tasks/<id>/` of one of `stores` (context and store root, as
/// watched), `<id>` a task ID. Temporary directories of a creation (`.<id>.tmp`) and other
/// names are not tasks.
#[must_use]
pub fn task_of_path(
    stores: &[(ContextName, PathBuf)],
    path: &Path,
) -> Option<(ContextName, TaskId)> {
    stores
        .iter()
        .filter_map(|(context, root)| {
            // Component-wise: `tasks-nested` is not under `tasks`.
            let rest = path.strip_prefix(root.join(TASKS_DIR)).ok()?;
            let id = task_dir_id(rest.components().next()?.as_os_str())?;
            Some((root.as_os_str().len(), context, id))
        })
        .max_by_key(|(len, _, _)| *len)
        .map(|(_, context, id)| (context.clone(), id))
}

#[cfg(test)]
mod path_tests {
    use std::path::{Path, PathBuf};

    use loom_api::ContextName;

    use super::task_of_path;

    fn stores() -> Vec<(ContextName, PathBuf)> {
        vec![
            ("work".parse().unwrap(), PathBuf::from("/l/contexts/work")),
            ("home".parse().unwrap(), PathBuf::from("/u/stores/home")),
            // A store inside another one's root: the longest root wins, never the outer one.
            (
                "nested".parse().unwrap(),
                PathBuf::from("/l/contexts/work/tasks-nested"),
            ),
        ]
    }

    fn found(path: &str) -> Option<(String, String)> {
        task_of_path(&stores(), Path::new(path))
            .map(|(context, id)| (context.to_string(), id.to_string()))
    }

    #[test]
    fn a_path_in_a_task_directory_is_that_task() {
        let task = |context: &str, id: &str| Some((context.to_owned(), id.to_owned()));
        assert_eq!(
            found("/l/contexts/work/tasks/k3q7zm2a/task.md"),
            task("work", "k3q7zm2a")
        );
        assert_eq!(
            found("/l/contexts/work/tasks/k3q7zm2a/.task.md.tmp"),
            task("work", "k3q7zm2a")
        );
        assert_eq!(
            found("/l/contexts/work/tasks/k3q7zm2a"),
            task("work", "k3q7zm2a")
        );
        assert_eq!(
            found("/u/stores/home/tasks/h8w2nc4d/events.jsonl"),
            task("home", "h8w2nc4d")
        );
    }

    #[test]
    fn other_paths_are_no_task() {
        for path in [
            "/l/contexts/work/tasks",
            "/l/contexts/work/tasks/.k3q7zm2a.tmp/task.md",
            "/l/contexts/work/tasks/notes/task.md",
            "/l/contexts/work/notes/k3q7zm2a/task.md",
            "/l/contexts/workshop/tasks/k3q7zm2a/task.md",
            "/elsewhere/tasks/k3q7zm2a/task.md",
        ] {
            assert_eq!(found(path), None, "{path}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use loom_api::{ContextName, ReindexResult};
    use loom_store::port::{ContentHash, TaskLocation};
    use loom_store::tasks::{IndexedState, IndexedTask, Reindexed, TaskFailure};

    use super::{failed_files, reindex_result};

    fn context(name: &str) -> ContextName {
        name.parse().unwrap()
    }

    fn invalid(context_name: &str, id: &str, error: &str) -> IndexedTask {
        IndexedTask {
            id: id.parse().unwrap(),
            context: context(context_name),
            hash: ContentHash::of(b""),
            location: TaskLocation::Active,
            state: IndexedState::Invalid {
                error: error.to_owned(),
                text: String::new(),
            },
        }
    }

    fn stores() -> BTreeMap<ContextName, PathBuf> {
        BTreeMap::from([
            (context("work"), PathBuf::from("/l/contexts/work")),
            (context("home"), PathBuf::from("/u/stores/home")),
        ])
    }

    fn failure(context_name: &str, id: Option<&str>, error: &str) -> TaskFailure {
        TaskFailure {
            context: context(context_name),
            id: id.map(|id| id.parse().unwrap()),
            error: error.to_owned(),
        }
    }

    #[test]
    fn invalid_files_are_listed_by_context_then_id() {
        let reindexed = Reindexed {
            rows: vec![
                invalid("work", "bbbbbbbb", "line 2: invalid TOML"),
                invalid("home", "cccccccc", "the file is not valid UTF-8"),
                invalid(
                    "work",
                    "aaaaaaaa",
                    "line 1: a task file starts with a `+++` line",
                ),
            ],
            failed: Vec::new(),
        };
        let result = reindex_result(&reindexed, &stores());
        assert_eq!(result.tasks, 3);
        let listed: Vec<(String, String)> = result
            .invalid
            .iter()
            .map(|file| (file.context.to_string(), file.id.to_string()))
            .collect();
        assert_eq!(
            listed,
            [
                ("home".to_owned(), "cccccccc".to_owned()),
                ("work".to_owned(), "aaaaaaaa".to_owned()),
                ("work".to_owned(), "bbbbbbbb".to_owned()),
            ]
        );
        assert_eq!(
            result.invalid[1].error,
            "line 1: a task file starts with a `+++` line"
        );
        assert_eq!(result.failed, []);
    }

    #[test]
    fn failures_get_the_path_of_their_task_or_store() {
        let failures = [
            failure("work", Some("k3q7zm2a"), "permission denied"),
            failure("home", None, "cannot list"),
            failure("work", Some("k3q7zm2a"), "a second error of the same task"),
        ];
        let files: Vec<(PathBuf, String)> = failed_files(&failures, &stores())
            .into_iter()
            .map(|file| (file.path, file.error))
            .collect();
        assert_eq!(
            files,
            [
                (
                    PathBuf::from("/l/contexts/work/tasks/k3q7zm2a"),
                    "permission denied".to_owned()
                ),
                (
                    PathBuf::from("/u/stores/home/tasks"),
                    "cannot list".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn nothing_indexed_reports_nothing() {
        let empty = Reindexed {
            rows: Vec::new(),
            failed: Vec::new(),
        };
        assert_eq!(reindex_result(&empty, &stores()), ReindexResult::default());
    }
}
