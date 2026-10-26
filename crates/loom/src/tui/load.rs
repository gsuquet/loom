//! Reads that the tabs of the task page need from `loomd`.

use std::collections::{BTreeMap, BTreeSet};

use loom_api::{Link, TaskDetail, TaskId, TaskList, TaskSummary};

use super::relations::{ChildNode, MAX_DEPTH, MAX_NODES, RelatedTask, RelationsData};
use crate::client::{Client, ClientError};

/// A task that another task names: `None` when loomd has no task with this ID.
async fn related(client: &Client, id: &TaskId) -> Result<RelatedTask, String> {
    match client.get::<TaskDetail>(&format!("/v1/tasks/{id}")).await {
        Ok(detail) => Ok(RelatedTask {
            id: id.clone(),
            summary: Some(detail.summary),
        }),
        Err(ClientError::Api { status: 404, .. }) => Ok(RelatedTask {
            id: id.clone(),
            summary: None,
        }),
        Err(error) => Err(error.to_string()),
    }
}

async fn related_all(client: &Client, ids: &[TaskId]) -> Result<Vec<RelatedTask>, String> {
    let mut tasks = Vec::new();
    for id in ids {
        tasks.push(related(client, id).await?);
    }
    Ok(tasks)
}

/// The children of `id`, archived ones included, sorted by ID.
async fn children(client: &Client, id: &TaskId) -> Result<Vec<TaskSummary>, String> {
    let mut found = Vec::new();
    for archived in [false, true] {
        let list: TaskList = client
            .get(&format!("/v1/tasks?parent={id}&archived={archived}"))
            .await
            .map_err(|error| error.to_string())?;
        found.extend(list.tasks);
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(found)
}

fn tree(
    level: &BTreeMap<TaskId, Vec<TaskSummary>>,
    id: &TaskId,
    shown: &mut BTreeSet<TaskId>,
) -> Vec<ChildNode> {
    let mut nodes = Vec::new();
    for task in level.get(id).into_iter().flatten() {
        // A hand-edited cycle is shown once.
        if shown.insert(task.id.clone()) {
            let children = tree(level, &task.id, shown);
            nodes.push(ChildNode {
                task: task.clone(),
                children,
            });
        }
    }
    nodes
}

/// Reads the relations of the task `id`: its parent, children (with their descendants, down to
/// [`MAX_DEPTH`] levels and [`MAX_NODES`] tasks), spawned-from and blocked tasks, and repository.
///
/// # Errors
///
/// The text of the error when `loomd` fails or does not answer. A related task that does not
/// exist is not an error: it shows as missing.
pub async fn fetch_relations(client: &Client, id: &TaskId) -> Result<RelationsData, String> {
    let detail: TaskDetail = client
        .get(&format!("/v1/tasks/{id}"))
        .await
        .map_err(|error| error.to_string())?;
    let mut data = RelationsData::default();
    if let Some(task) = &detail.summary.task {
        data.parent = match &task.relations.parent {
            Some(parent) => Some(related(client, parent).await?),
            None => None,
        };
        data.spawned_from = related_all(client, &task.relations.spawned_from).await?;
        data.blocks = related_all(client, &task.relations.blocks).await?;
        data.repos = task
            .links
            .iter()
            .filter_map(|link| match link {
                Link::Repo { repo } => Some(repo.clone()),
                Link::Origin(_) | Link::Related(_) => None,
            })
            .collect();
    }
    // Level by level, so that the cut falls on the deepest tasks first.
    let mut level: BTreeMap<TaskId, Vec<TaskSummary>> = BTreeMap::new();
    let mut frontier = vec![id.clone()];
    let mut count = 0;
    'levels: for depth in 0..MAX_DEPTH {
        let mut next = Vec::new();
        for parent in &frontier {
            let mut kids = children(client, parent).await?;
            if count + kids.len() > MAX_NODES {
                kids.truncate(MAX_NODES - count);
                data.truncated = true;
            }
            count += kids.len();
            next.extend(kids.iter().map(|kid| kid.id.clone()));
            level.insert(parent.clone(), kids);
            if data.truncated {
                break 'levels;
            }
        }
        if depth + 1 == MAX_DEPTH && !next.is_empty() {
            // Tasks of the last level may have children that the tree does not show.
            for leaf in &next {
                if !children(client, leaf).await?.is_empty() {
                    data.truncated = true;
                    break;
                }
            }
        }
        frontier = next;
    }
    let mut shown = BTreeSet::from([id.clone()]);
    data.children = tree(&level, id, &mut shown);
    Ok(data)
}
