//! Decisions of the task store of loomd, without I/O: the response of a reindex, the task a
//! changed path belongs to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use loom_api::{
    ContextName, FailedTaskFile, InvalidTaskFile, ReindexResult, TaskId, TaskStoreHealth,
};
use loom_store::files::{ARCHIVE_DIR, TASKS_DIR};
use loom_store::port::{archive_year_dir, task_dir_id};
use loom_store::tasks::{IndexedState, IndexedTask, Reindexed, TaskFailure};

/// What a reindex reports: every row counted, the invalid ones listed by context and ID, the
/// failures with their path under `stores` (root of each context store).
#[must_use]
pub fn reindex_result(
    reindexed: &Reindexed,
    stores: &BTreeMap<ContextName, PathBuf>,
) -> ReindexResult {
    ReindexResult {
        tasks: reindexed.rows.len(),
        invalid: invalid_files(&reindexed.rows),
        failed: failed_files(&reindexed.failed, stores),
    }
}

/// The tasks of `rows` whose `task.md` cannot be read, sorted by context and ID.
#[must_use]
pub fn invalid_files(rows: &[IndexedTask]) -> Vec<InvalidTaskFile> {
    let mut invalid: Vec<InvalidTaskFile> = rows
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
    invalid
}

/// The problems of the task stores: the invalid files of `rows` (every indexed task), the
/// failures of the last reconciliation, and whether the file watcher runs.
#[must_use]
pub fn task_store_health(
    watching: bool,
    failed: Vec<FailedTaskFile>,
    rows: &[IndexedTask],
) -> TaskStoreHealth {
    TaskStoreHealth {
        watching,
        invalid: invalid_files(rows),
        failed,
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

/// What a changed path of a task store asks loomd to reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchedPath {
    /// A path in the directory of this task.
    Task(ContextName, TaskId),
    /// A year directory of an archive (`<root>/archive/<yyyy>`), new or removed: every task.
    /// With inotify, a directory created with its content (a first move of the year, a sync, a
    /// hand copy) gives one event for the directory only, none for the tasks inside it.
    ArchiveYear,
    /// Nothing to reconcile.
    Other,
}

/// What `path`, changed in one of `stores` (context and store root, as watched), asks to
/// reconcile.
///
/// A task path is under `<root>/tasks/<id>/` or `<root>/archive/<yyyy>/<id>/`, `<id>` a task
/// ID and `<yyyy>` a year. Temporary directories of a creation (`.<id>.tmp`) and other names
/// are not tasks. When stores are nested, the longest root wins.
#[must_use]
pub fn watched_path(stores: &[(ContextName, PathBuf)], path: &Path) -> WatchedPath {
    stores
        .iter()
        .filter_map(|(context, root)| {
            // Component-wise: `tasks-nested` is not under `tasks`.
            let watched = if let Ok(rest) = path.strip_prefix(root.join(TASKS_DIR)) {
                let id = task_dir_id(rest.components().next()?.as_os_str())?;
                WatchedPath::Task(context.clone(), id)
            } else {
                let rest = path.strip_prefix(root.join(ARCHIVE_DIR)).ok()?;
                let mut components = rest.components();
                archive_year_dir(components.next()?.as_os_str())?;
                match components.next() {
                    None => WatchedPath::ArchiveYear,
                    Some(dir) => WatchedPath::Task(context.clone(), task_dir_id(dir.as_os_str())?),
                }
            };
            Some((root.as_os_str().len(), watched))
        })
        .max_by_key(|(len, _)| *len)
        .map_or(WatchedPath::Other, |(_, watched)| watched)
}

#[cfg(test)]
mod path_tests {
    use std::path::{Path, PathBuf};

    use loom_api::ContextName;

    use super::{WatchedPath, watched_path};

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
        match watched_path(&stores(), Path::new(path)) {
            WatchedPath::Task(context, id) => Some((context.to_string(), id.to_string())),
            WatchedPath::ArchiveYear | WatchedPath::Other => None,
        }
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
        assert_eq!(
            found("/l/contexts/work/archive/2026/k3q7zm2a/task.md"),
            task("work", "k3q7zm2a")
        );
        assert_eq!(
            found("/l/contexts/work/archive/2026/k3q7zm2a"),
            task("work", "k3q7zm2a")
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
            "/l/contexts/work/archive",
            "/l/contexts/work/archive/26/k3q7zm2a/task.md",
            "/l/contexts/work/archive/2026/notes/task.md",
        ] {
            assert_eq!(
                watched_path(&stores(), Path::new(path)),
                WatchedPath::Other,
                "{path}"
            );
        }
    }

    #[test]
    fn a_year_directory_of_an_archive_asks_for_every_task() {
        for path in [
            "/l/contexts/work/archive/2026",
            "/u/stores/home/archive/1999",
        ] {
            assert_eq!(
                watched_path(&stores(), Path::new(path)),
                WatchedPath::ArchiveYear,
                "{path}"
            );
        }
        // Not a year, or not an archive.
        for path in ["/l/contexts/work/archive/26", "/l/contexts/work/tasks/2026"] {
            assert_eq!(
                watched_path(&stores(), Path::new(path)),
                WatchedPath::Other,
                "{path}"
            );
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

    use super::{failed_files, reindex_result, task_store_health};

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

    #[test]
    fn health_lists_the_invalid_files_sorted_and_carries_the_rest_as_given() {
        let valid = IndexedTask {
            state: IndexedState::Valid {
                task: Box::new(loom_api::Task {
                    id: "k3q7zm2a".parse().unwrap(),
                    title: "Fine".to_owned(),
                    kind: loom_api::Kind::Chore,
                    context: context("work"),
                    status: loom_api::Status::Ready,
                    importance: loom_api::Importance::P2,
                    due: None,
                    pinned: false,
                    links: Vec::new(),
                    relations: loom_api::Relations::default(),
                    created: loom_api::Timestamp::EPOCH,
                    updated: loom_api::Timestamp::EPOCH,
                }),
                body: String::new(),
            },
            ..invalid("work", "k3q7zm2a", "")
        };
        let rows = vec![
            invalid("work", "m0000000", "line 2: invalid TOML"),
            valid,
            invalid("home", "k3r00000", "no `+++` frontmatter"),
        ];
        let failed = failed_files(
            &[TaskFailure {
                context: context("work"),
                id: None,
                error: "permission denied".to_owned(),
            }],
            &stores(),
        );
        let health = task_store_health(false, failed.clone(), &rows);
        assert!(!health.watching);
        let ids: Vec<(&str, &str)> = health
            .invalid
            .iter()
            .map(|file| (file.context.as_str(), file.id.as_str()))
            .collect();
        assert_eq!(ids, [("home", "k3r00000"), ("work", "m0000000")]);
        assert_eq!(health.invalid[1].error, "line 2: invalid TOML");
        assert_eq!(health.failed, failed);
        // A clean store: nothing listed.
        let clean = task_store_health(true, Vec::new(), &[]);
        assert!(clean.watching && clean.invalid.is_empty() && clean.failed.is_empty());
    }
}
