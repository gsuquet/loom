//! The Relations tab of the task page: the rows of the relations and the choice among them.
//!
//! Pure functions; the state keeps the loaded relations and the selected row, `actions` reads
//! them from loomd.

use loom_api::{TaskId, TaskSummary};

use super::render::importance_label;

/// How deep the tree of children goes.
pub const MAX_DEPTH: usize = 4;
/// How many descendants the tree of children holds at most.
pub const MAX_NODES: usize = 100;

/// What the Relations tab holds for the open task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationsView {
    /// The relations, once loaded.
    pub load: RelationsLoad,
    /// The selected task, counted over the tasks that Enter can follow.
    pub selected: usize,
}

/// Whether the relations of the task are loaded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RelationsLoad {
    /// Not asked yet.
    #[default]
    NotLoaded,
    /// Asked, no answer yet.
    Loading,
    /// The relations.
    Loaded(Box<RelationsData>),
    /// The answer was an error.
    Failed(String),
}

/// A task that another task names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedTask {
    /// Its ID, as the relation holds it.
    pub id: TaskId,
    /// The task, or `None` when loomd has no task with this ID.
    pub summary: Option<TaskSummary>,
}

/// A child with its own children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildNode {
    /// The child.
    pub task: TaskSummary,
    /// Its children.
    pub children: Vec<Self>,
}

/// Everything that links a task to the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationsData {
    /// The parent task.
    pub parent: Option<RelatedTask>,
    /// The children, with their descendants.
    pub children: Vec<ChildNode>,
    /// The tasks this one was spawned from.
    pub spawned_from: Vec<RelatedTask>,
    /// The tasks this one blocks.
    pub blocks: Vec<RelatedTask>,
    /// Slugs of the repositories the task works in.
    pub repos: Vec<String>,
    /// The tree of children was cut at `MAX_DEPTH` or `MAX_NODES`.
    pub truncated: bool,
}

/// One line of the tab.
// The rows are built for one frame and dropped: boxing the summary would only add a step.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The title of a section.
    Header(&'static str),
    /// A task. `depth` is the level in the tree of children.
    Task {
        /// Level in the tree, from 0.
        depth: usize,
        /// The task ID.
        id: TaskId,
        /// The task, or `None` when it does not exist.
        summary: Option<TaskSummary>,
    },
    /// A repository slug.
    Repo(String),
    /// The text under a section that has no entry, or a note under the tree.
    Note(&'static str),
}

impl Row {
    /// The task that Enter follows on this row, if any.
    #[must_use]
    pub const fn target(&self) -> Option<&TaskId> {
        match self {
            Self::Task {
                id,
                summary: Some(_),
                ..
            } => Some(id),
            Self::Task { summary: None, .. } | Self::Header(_) | Self::Repo(_) | Self::Note(_) => {
                None
            }
        }
    }
}

fn related_rows(rows: &mut Vec<Row>, tasks: &[RelatedTask]) {
    for task in tasks {
        rows.push(Row::Task {
            depth: 0,
            id: task.id.clone(),
            summary: task.summary.clone(),
        });
    }
}

fn child_rows(rows: &mut Vec<Row>, nodes: &[ChildNode], depth: usize) {
    for node in nodes {
        rows.push(Row::Task {
            depth,
            id: node.task.id.clone(),
            summary: Some(node.task.clone()),
        });
        child_rows(rows, &node.children, depth + 1);
    }
}

/// The lines of the tab, in order: parent, children, spawned from, blocks, repository.
#[must_use]
pub fn rows(data: &RelationsData) -> Vec<Row> {
    let mut rows = vec![Row::Header("Parent")];
    match &data.parent {
        Some(parent) => related_rows(&mut rows, std::slice::from_ref(parent)),
        None => rows.push(Row::Note("none")),
    }
    rows.push(Row::Header("Children"));
    if data.children.is_empty() {
        rows.push(Row::Note("none"));
    } else {
        child_rows(&mut rows, &data.children, 0);
        if data.truncated {
            rows.push(Row::Note("the tree is cut here"));
        }
    }
    rows.push(Row::Header("Spawned from"));
    if data.spawned_from.is_empty() {
        rows.push(Row::Note("none"));
    }
    related_rows(&mut rows, &data.spawned_from);
    rows.push(Row::Header("Blocks"));
    if data.blocks.is_empty() {
        rows.push(Row::Note("none"));
    }
    related_rows(&mut rows, &data.blocks);
    rows.push(Row::Header("Repository"));
    if data.repos.is_empty() {
        rows.push(Row::Note("none"));
    }
    rows.extend(data.repos.iter().cloned().map(Row::Repo));
    rows
}

/// How many tasks Enter can follow.
#[must_use]
pub fn selectable(data: &RelationsData) -> usize {
    rows(data)
        .iter()
        .filter(|row| row.target().is_some())
        .count()
}

/// The task that Enter follows when the `selected`th followable task is selected.
#[must_use]
pub fn target_at(data: &RelationsData, selected: usize) -> Option<TaskId> {
    rows(data)
        .iter()
        .filter_map(Row::target)
        .nth(selected)
        .cloned()
}

/// The place of the task `id` among the followable tasks, if it is one of them.
#[must_use]
pub fn index_of(data: &RelationsData, id: &TaskId) -> Option<usize> {
    rows(data)
        .iter()
        .filter_map(Row::target)
        .position(|task| task == id)
}

/// The index in [`rows`] of the `selected`th followable task.
#[must_use]
pub fn row_of(data: &RelationsData, selected: usize) -> Option<usize> {
    rows(data)
        .iter()
        .enumerate()
        .filter(|(_, row)| row.target().is_some())
        .nth(selected)
        .map(|(at, _)| at)
}

/// The text of a task row, without the selection mark: `prefix  status  importance  title`.
#[must_use]
pub fn task_text(id: &TaskId, summary: Option<&TaskSummary>) -> String {
    let Some(summary) = summary else {
        return format!("{id}  (no such task)");
    };
    summary.task.as_ref().map_or_else(
        || format!("{}  (invalid task file)", summary.prefix),
        |task| {
            format!(
                "{}  {}  {}  {}",
                summary.prefix,
                task.status
                    .name()
                    .map_or_else(|| "unknown".to_owned(), |n| n.to_string()),
                importance_label(task.importance),
                task.title
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_api::{Importance, Kind, Relations, Status, Task, Timestamp};

    fn id(text: &str) -> TaskId {
        text.parse().unwrap()
    }

    fn summary(task_id: &str, title: &str) -> TaskSummary {
        TaskSummary {
            id: id(task_id),
            prefix: task_id[..4].to_owned(),
            context: "work".parse().unwrap(),
            task: Some(Task {
                id: id(task_id),
                title: title.to_owned(),
                kind: Kind::Chore,
                context: "work".parse().unwrap(),
                status: Status::Ready,
                importance: Importance::P2,
                due: None,
                pinned: false,
                links: Vec::new(),
                relations: Relations::default(),
                created: Timestamp::from_unix_secs(100).unwrap(),
                updated: Timestamp::from_unix_secs(100).unwrap(),
            }),
            read_only: None,
            rank: None,
        }
    }

    fn related(task_id: &str, title: Option<&str>) -> RelatedTask {
        RelatedTask {
            id: id(task_id),
            summary: title.map(|title| summary(task_id, title)),
        }
    }

    fn data() -> RelationsData {
        RelationsData {
            parent: Some(related("aaaaaaaa", Some("Project"))),
            children: vec![ChildNode {
                task: summary("bbbbbbbb", "Child"),
                children: vec![ChildNode {
                    task: summary("cccccccc", "Grandchild"),
                    children: Vec::new(),
                }],
            }],
            spawned_from: vec![related("dddddddd", None)],
            blocks: vec![related("eeeeeeee", Some("Blocked"))],
            repos: vec!["work/api".to_owned()],
            truncated: false,
        }
    }

    #[test]
    fn rows_list_the_sections_in_order_with_the_tree_indented() {
        let rows = rows(&data());
        let shape: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Row::Header(name) => format!("# {name}"),
                Row::Task { depth, id, .. } => format!("{depth} {id}"),
                Row::Repo(slug) => format!("repo {slug}"),
                Row::Note(text) => format!("note {text}"),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "# Parent",
                "0 aaaaaaaa",
                "# Children",
                "0 bbbbbbbb",
                "1 cccccccc",
                "# Spawned from",
                "0 dddddddd",
                "# Blocks",
                "0 eeeeeeee",
                "# Repository",
                "repo work/api",
            ]
        );
    }

    #[test]
    fn an_empty_section_says_none_and_a_cut_tree_says_so() {
        let empty = rows(&RelationsData::default());
        assert_eq!(empty.iter().filter(|r| **r == Row::Note("none")).count(), 5);
        let cut = RelationsData {
            children: vec![ChildNode {
                task: summary("bbbbbbbb", "Child"),
                children: Vec::new(),
            }],
            truncated: true,
            ..RelationsData::default()
        };
        assert!(rows(&cut).contains(&Row::Note("the tree is cut here")));
    }

    #[test]
    fn only_tasks_that_exist_can_be_followed_in_row_order() {
        let data = data();
        // parent, child, grandchild, blocked: the spawned-from task does not exist.
        assert_eq!(selectable(&data), 4);
        let ids: Vec<_> = (0..5).map(|at| target_at(&data, at)).collect();
        assert_eq!(
            ids,
            [
                Some(id("aaaaaaaa")),
                Some(id("bbbbbbbb")),
                Some(id("cccccccc")),
                Some(id("eeeeeeee")),
                None
            ]
        );
        assert_eq!(row_of(&data, 0), Some(1));
        assert_eq!(row_of(&data, 3), Some(8));
    }

    #[test]
    fn task_text_shows_prefix_status_importance_and_title() {
        let s = summary("bbbbbbbb", "Child");
        assert_eq!(task_text(&s.id, Some(&s)), "bbbb  ready  p2  Child");
        assert_eq!(task_text(&id("dddddddd"), None), "dddddddd  (no such task)");
    }
}
