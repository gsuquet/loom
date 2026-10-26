//! Reads that the tabs of the task page need from `loomd`.

use std::collections::{BTreeMap, BTreeSet};

use futures_util::future::try_join_all;
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
    try_join_all(ids.iter().map(|id| related(client, id))).await
}

/// The children of `parents`, archived ones included, sorted by ID, in one request.
async fn children(client: &Client, parents: &[TaskId]) -> Result<Vec<TaskSummary>, String> {
    if parents.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<&str> = parents.iter().map(TaskId::as_str).collect();
    let list: TaskList = client
        .get(&format!("/v1/tasks?parent={}&archived=any", ids.join(",")))
        .await
        .map_err(|error| error.to_string())?;
    let mut found = list.tasks;
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
    // Level by level, one request per level, so that the cut falls on the deepest tasks first.
    let mut level: BTreeMap<TaskId, Vec<TaskSummary>> = BTreeMap::new();
    let mut frontier = vec![id.clone()];
    let mut count = 0;
    for _ in 0..MAX_DEPTH {
        let mut kids = children(client, &frontier).await?;
        if count + kids.len() > MAX_NODES {
            kids.truncate(MAX_NODES - count);
            data.truncated = true;
        }
        count += kids.len();
        frontier = kids.iter().map(|kid| kid.id.clone()).collect();
        for kid in kids {
            let parent = kid.task.as_ref().and_then(|t| t.relations.parent.clone());
            if let Some(parent) = parent {
                level.entry(parent).or_default().push(kid);
            }
        }
        if data.truncated || frontier.is_empty() {
            break;
        }
    }
    if !data.truncated && !frontier.is_empty() {
        // The tasks of the last level may have children that the tree does not show.
        data.truncated = !children(client, &frontier).await?.is_empty();
    }
    let mut shown = BTreeSet::from([id.clone()]);
    data.children = tree(&level, id, &mut shown);
    Ok(data)
}
