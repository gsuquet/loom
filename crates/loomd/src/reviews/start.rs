//! A start (`POST /v1/reviews/queue/{key}/start`): reads the MR of a queue entry and has the task
//! thread create its Review task. It runs on the poller thread: it never runs during a poll, so a
//! poll cannot hand over what it read before the start. The decisions are in
//! `loomd::review_queue`.

use std::time::Instant;

use axum::http::StatusCode;
use loom_api::{
    ApiError, ContextName, ErrorCode, ForgeKind, QueueReason, ReviewSyncOutcome, TaskId,
};
use loom_forge::client::{GithubClient, GitlabClient, GitlabMr, Token};
use loom_forge::model::{MrKey, MrState};
use loom_store::Reader;
use loomd::api::{internal, invalid_request, unavailable};
use loomd::review_queue::{
    StartRefusal, StartTarget, active_reviews_of, can_start, start_target, waits_no_more,
};
use loomd::review_sync::{MrUpdate, url_host};
use tokio::sync::oneshot;
use tracing::Span;

use super::{
    GitlabRead, Poller, ReadFailure, Request, ReviewHandle, Stopped, forget_etag, github_update,
    gitlab_update, gone_update, project_id,
};
use crate::mr_worker::PassTrigger;
use crate::repos::Failure;
use crate::tasks::read_entry;

/// Why the read of a start stopped: only a 404 on the MR itself removes its entry, not a 404 on
/// the login of the token or on another read.
enum StartRead {
    MrGone,
    Stopped(Stopped),
}

impl From<Stopped> for StartRead {
    fn from(stopped: Stopped) -> Self {
        Self::Stopped(stopped)
    }
}

impl From<ReadFailure> for StartRead {
    fn from(failure: ReadFailure) -> Self {
        match failure {
            ReadFailure::MrGone(_) => Self::MrGone,
            ReadFailure::Other(error) => Self::Stopped(error.into()),
        }
    }
}

impl ReviewHandle {
    /// Starts on the MR `key`, after a poll that runs: its Review task, and whether this request
    /// created it. Never a second task.
    ///
    /// # Errors
    ///
    /// The refusal of the start.
    pub async fn start(
        &self,
        context: Option<ContextName>,
        key: MrKey,
    ) -> Result<(TaskId, bool), Failure> {
        let gone = || {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                unavailable("loomd is stopping"),
            )
        };
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request::Start {
                context,
                key,
                reply,
                parent: Span::current(),
            })
            .map_err(|_| gone())?;
        answer.await.map_err(|_| gone())?
    }
}

impl Poller {
    /// Starts on the MR `key`: answers with its Review task when it has one, otherwise reads the
    /// MR in full and creates the task with its mirror, and removes the entry.
    pub(super) fn start_review(
        &mut self,
        context: Option<&ContextName>,
        key: &MrKey,
    ) -> Result<(TaskId, bool), Failure> {
        let cannot_read = |error: &dyn std::fmt::Display| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                internal(format!("cannot read the tasks: {error}")),
            )
        };
        let rows = Reader::open(self.tasks.index_db())
            .and_then(|reader| reader.tasks(None))
            .map_err(|error| cannot_read(&error))?;
        let tasks = active_reviews_of(&rows, key);
        let entries: Vec<(ContextName, _)> = self
            .stores
            .iter()
            .filter_map(|(name, root)| Some((name.clone(), read_entry(root, key)?)))
            // A file with no reason only keeps a `seen_at`: it is not in the queue.
            .filter(|(_, entry)| !entry.reasons.is_empty())
            .collect();
        let hosts: Vec<(ContextName, Option<String>)> = entries
            .iter()
            .map(|(name, entry)| (name.clone(), url_host(&entry.web_url)))
            .collect();
        match start_target(context, &tasks, &hosts) {
            StartTarget::Task(id) => Ok((id, false)),
            StartTarget::NotFound => Err((
                StatusCode::NOT_FOUND,
                ApiError::new(
                    ErrorCode::NotFound,
                    format!("MR `{key}` has no queue entry and no Review task"),
                ),
            )),
            StartTarget::Ambiguous(contexts) => {
                let contexts: Vec<&str> = contexts.iter().map(ContextName::as_str).collect();
                Err((
                    StatusCode::BAD_REQUEST,
                    invalid_request(format!(
                        "MR `{key}` is in several contexts ({}): name one",
                        contexts.join(", ")
                    )),
                ))
            }
            StartTarget::Entry(name) => {
                let project =
                    entries
                        .iter()
                        .find(|(other, _)| *other == name)
                        .and_then(|(_, entry)| {
                            Some((
                                url_host(&entry.web_url)?,
                                project_id(&entry.forge_ids),
                                entry.reasons.contains(&QueueReason::Mentioned),
                            ))
                        });
                let Some((host, project, mentioned)) = project else {
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        internal(format!("the entry of MR `{key}` has no web URL host")),
                    ));
                };
                self.start_entry(&name, &host, (project, mentioned), key)
            }
        }
    }

    /// Reads the MR of an entry on `host` and creates its Review task. A forge that fails
    /// changes nothing: the entry stays, and the same request can succeed later.
    fn start_entry(
        &mut self,
        context: &ContextName,
        host: &str,
        (project, mentioned): (Option<u64>, bool),
        key: &MrKey,
    ) -> Result<(TaskId, bool), Failure> {
        let Some(index) = self
            .hosts
            .iter()
            .position(|polled| polled.target.context == *context && polled.target.host == host)
        else {
            // The machine that polls the context would not see this task before a store sync,
            // and could create a second one.
            return Err((
                StatusCode::BAD_REQUEST,
                invalid_request(format!(
                    "this machine does not poll {host} for context `{context}`: start on MR `{key}` on the first machine of the context, or add a credential for the host if it was removed"
                )),
            ));
        };
        if let Some((until, at)) = self.hosts[index].rate_limited
            && until > Instant::now()
        {
            return Err(forge_failure(format!(
                "{host} limits the rate until {at}: MR `{key}` is not started"
            )));
        }
        let target = self.hosts[index].target.clone();
        let token = self
            .resolver
            .resolve(&target.source)
            .map_err(|error| forge_failure(error.to_string()))?;
        // A read does not show a request through a team: what the last poll saw tells.
        let was_direct = self.hosts[index]
            .mrs
            .get(&key.dir_name())
            .is_some_and(|cache| cache.direct)
            || target.kind == ForgeKind::Gitlab;
        // In full: the mirror needs the MR, and a 304 would not give it.
        forget_etag(self.hosts[index].mrs.get_mut(&key.dir_name()));
        let read = match target.kind {
            ForgeKind::Github => {
                let client =
                    GithubClient::new(&self.hosts[index].api, token.clone(), &self.user_agent);
                self.read_github_start(index, &client, key)
            }
            ForgeKind::Gitlab => {
                let client =
                    GitlabClient::new(&self.hosts[index].api, token.clone(), &self.user_agent);
                self.read_gitlab_start(index, &client, key, project)
            }
        };
        let mut update = match read {
            Ok(update) => update,
            Err(StartRead::MrGone) => return Err(self.entry_is_gone(index, key)),
            Err(StartRead::Stopped(stopped)) => {
                forget_etag(self.hosts[index].mrs.get_mut(&key.dir_name()));
                return Err(match self.stopped(index, stopped, &token) {
                    ReviewSyncOutcome::RateLimited { until } => forge_failure(format!(
                        "{host} limits the rate until {until}: MR `{key}` is not started"
                    )),
                    ReviewSyncOutcome::Failed { error } => {
                        forge_failure(format!("{host}: {error}"))
                    }
                    ReviewSyncOutcome::Synced { .. } => {
                        forge_failure(format!("{host}: MR `{key}` is not started"))
                    }
                });
            }
        };
        let reviewer = self.hosts[index]
            .mrs
            .get(&key.dir_name())
            .is_some_and(|cache| cache.direct);
        // An MR can be in the queue for a reason other than a request: an assignee, or a to-do.
        let assigned = update.reasons.assigned == Some(true);
        let request_gone = waits_no_more(was_direct, reviewer, assigned || mentioned);
        let refusal = update
            .mr
            .as_ref()
            .and_then(|mr| can_start(mr.state, update.own, request_gone).err());
        if let Some(refusal) = refusal {
            return Err(self.refuse_start(index, key, update, refusal, &token));
        }
        update.start = true;
        let Some(applied) = self.hand_over(index, vec![update], PassTrigger::Start) else {
            return Err(stopping());
        };
        if let Some(failure) = applied.failed.first() {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                internal(token.redact(&failure.error)),
            ));
        }
        applied.created.first().map_or_else(
            || {
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    internal(format!("no Review task was created for MR `{key}`")),
                ))
            },
            |id| Ok((id.clone(), true)),
        )
    }

    /// The forge answers 404 for the MR of a start: as for a closed MR, the entry goes. No task
    /// means no pass of MR refs. The next GitLab list is full: a lost access also answers 404.
    fn entry_is_gone(&mut self, index: usize, key: &MrKey) -> Failure {
        let host = &mut self.hosts[index];
        host.mrs.remove(&key.dir_name());
        let Some(applied) = self.write_updates(index, vec![gone_update(key.clone())]) else {
            return stopping();
        };
        if let Some(failure) = applied.failed.first() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                internal(failure.error.clone()),
            );
        }
        (
            StatusCode::NOT_FOUND,
            ApiError::new(
                ErrorCode::NotFound,
                format!("the forge no longer shows MR `{key}`: its entry is removed"),
            ),
        )
    }

    /// The MR that a start read cannot start: the poll rules remove its entry. No task means no
    /// pass of MR refs.
    fn refuse_start(
        &mut self,
        index: usize,
        key: &MrKey,
        mut update: MrUpdate,
        refusal: StartRefusal,
        token: &Token,
    ) -> Failure {
        if refusal == StartRefusal::RequestGone {
            update.requested = Some(false);
        }
        let Some(applied) = self.write_updates(index, vec![update]) else {
            return stopping();
        };
        if let Some(failure) = applied.failed.first() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                internal(token.redact(&failure.error)),
            );
        }
        refused(key, refusal)
    }

    fn read_github_start(
        &mut self,
        index: usize,
        client: &GithubClient,
        key: &MrKey,
    ) -> Result<MrUpdate, StartRead> {
        let login = self.login(index, client)?;
        let cache = self.hosts[index].mrs.entry(key.dir_name()).or_default();
        github_update(client, key, Some(true), &login, cache, true).map_err(StartRead::from)
    }

    fn read_gitlab_start(
        &mut self,
        index: usize,
        client: &GitlabClient,
        key: &MrKey,
        project: Option<u64>,
    ) -> Result<MrUpdate, StartRead> {
        let project_id = project.ok_or_else(|| {
            StartRead::Stopped(Stopped::Other(
                "the ID of the GitLab project is unknown".to_owned(),
            ))
        })?;
        let login = self.login(index, client)?;
        let cache = self.hosts[index].mrs.entry(key.dir_name()).or_default();
        cache.project_id = Some(project_id);
        let mr = GitlabMr {
            project_id,
            iid: key.number(),
        };
        // GitLab has no team requests: the read tells whether I am requested. An MR assigned to
        // me is not a request, and `sync.json` must not say so: the next poll would see a removed
        // request, and cancel the task.
        let read = GitlabRead {
            key,
            reference: mr,
            found: Some(false),
            mentioned: None,
            login: &login,
            mirrored: true,
        };
        gitlab_update(client, &read, cache).map_err(StartRead::from)
    }
}

fn forge_failure(message: String) -> Failure {
    (
        StatusCode::BAD_GATEWAY,
        ApiError::new(ErrorCode::Forge, message),
    )
}

fn stopping() -> Failure {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        unavailable("loomd is stopping"),
    )
}

fn refused(key: &MrKey, refusal: StartRefusal) -> Failure {
    let reason = match refusal {
        StartRefusal::NotOpen(MrState::Merged) => "is merged",
        StartRefusal::NotOpen(MrState::Closed | MrState::Open) => "is closed",
        StartRefusal::Own => "is my own MR",
        StartRefusal::RequestGone => "no longer waits on me",
    };
    (
        StatusCode::CONFLICT,
        ApiError::new(
            ErrorCode::Conflict,
            format!("MR `{key}` {reason}: it gets no Review task"),
        ),
    )
}
