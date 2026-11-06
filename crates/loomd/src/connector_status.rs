//! Connector status (`docs/design/connector-ports.md`, "Connector status"): what the last polls
//! of each connector instance and forge credential found, for `GET /v1/connectors`.
//!
//! - Pure: [`is_stale`], [`stop_of`], [`forge_poll`] (`ReviewSync` onto the health of a
//!   connector) and [`forge_instances`].
//! - [`ConnectorStatuses`]: the one writer of `state/connectors.json`. A poll thread reports to
//!   it. It holds the status in memory, and only `last_attempt` and `last_success` go to the
//!   file, since this state is machine-local and each poll would otherwise commit to a store.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use loom_api::{
    ChangeEvent, ConnectorCounts, ConnectorFailure, ConnectorHealth, ConnectorKind, ConnectorList,
    ConnectorStatus, ContextName, PolledBy, ReviewList, ReviewListFailureKind, ReviewSyncOutcome,
    Secret, Timestamp,
};
use loom_config::config::Config;
use loom_config::forges::DEFAULT_POLL_INTERVAL;
use loom_config::layout::{STATE_DIR_MODE, STATE_FILE_MODE};
use loom_config::machine::MachineName;
use loom_forge::client::ForgeError;
use loom_store::atomic;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::events::Bus;
use crate::recurring::{Runner, runner};

/// An instance is stale after this many poll intervals without a success.
const STALE_INTERVALS: u32 = 3;
/// The version of the format of `state/connectors.json`.
const VERSION: u32 = 1;

/// No success in [`STALE_INTERVALS`] intervals, or never. A clock set back is not stale. Pure.
#[must_use]
pub fn is_stale(last_success: Option<Timestamp>, interval: Duration, now: Timestamp) -> bool {
    last_success.is_none_or(|success| {
        let window = interval.saturating_mul(STALE_INTERVALS).as_secs();
        now.unix_secs().saturating_sub(success.unix_secs()) >= window
    })
}

/// Why a poll of a forge stopped, as the health tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeStop {
    /// The token is refused or does not resolve.
    Credential,
    /// The token is good, and a permission is missing.
    Forbidden,
    /// Anything else.
    Unreachable,
}

/// The cause of a poll that `error` stopped. Pure.
#[must_use]
pub const fn stop_of(error: &ForgeError) -> ForgeStop {
    match error {
        ForgeError::Unauthorized { .. } => ForgeStop::Credential,
        // A token that SSO has not authorized is good, and it needs a step of mine.
        ForgeError::Forbidden { .. }
        | ForgeError::InsufficientScope { .. }
        | ForgeError::SsoRequired { .. } => ForgeStop::Forbidden,
        // A rate limit has its own outcome, and never reaches this.
        ForgeError::RateLimited { .. }
        | ForgeError::Transport(_)
        | ForgeError::BadRequest { .. }
        | ForgeError::NotFound { .. }
        | ForgeError::ApprovalRefused { .. }
        | ForgeError::Conflict { .. }
        | ForgeError::Unprocessable { .. }
        | ForgeError::Status { .. }
        | ForgeError::Graphql { .. }
        | ForgeError::Parse(_)
        | ForgeError::Unexpected(_) => ForgeStop::Unreachable,
    }
}

/// What a poll of a forge means for its status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgePoll {
    /// The health.
    pub health: ConnectorHealth,
    /// The poll succeeded: `last_success` moves.
    pub success: bool,
    /// The new counts; `None` keeps the last ones, since the poll counted nothing.
    pub items: Option<ConnectorCounts>,
}

/// The key of a failed list in [`ConnectorFailure`]. No MR key can look like this.
const fn list_key(list: ReviewList) -> &'static str {
    match list {
        ReviewList::Assigned => "list:assigned",
        ReviewList::Own => "list:own",
        ReviewList::Mentioned => "list:mentioned",
    }
}

/// The health of a poll that `outcome` ended. `stop` is its cause when the poll failed, and
/// `failures` the failed polls in a row, this one included. Pure.
#[must_use]
pub fn forge_poll(
    outcome: &ReviewSyncOutcome,
    stop: Option<ForgeStop>,
    failures: u32,
) -> ForgePoll {
    match outcome {
        ReviewSyncOutcome::Synced {
            mrs,
            failed,
            failed_lists,
            ..
        } => {
            let mut other: Vec<ConnectorFailure> = failed
                .iter()
                .map(|failure| ConnectorFailure {
                    key: failure.key.clone(),
                    reason: failure.error.clone(),
                })
                .collect();
            let mut refused = Vec::new();
            for failure in failed_lists {
                let key = list_key(failure.list);
                match failure.kind {
                    ReviewListFailureKind::Forbidden => {
                        refused.push(format!("{key}: {}", failure.error));
                    }
                    ReviewListFailureKind::Other => other.push(ConnectorFailure {
                        key: key.to_owned(),
                        reason: failure.error.clone(),
                    }),
                }
            }
            let items = ConnectorCounts {
                listed: *mrs,
                failed: failed.len() + failed_lists.len(),
                ..ConnectorCounts::default()
            };
            // Ahead of `Partial`: a refused list does not heal by itself.
            let health = if !refused.is_empty() {
                ConnectorHealth::Forbidden {
                    error: refused.join("; "),
                    failed: other,
                }
            } else if other.is_empty() {
                ConnectorHealth::Ok
            } else {
                ConnectorHealth::Partial { failed: other }
            };
            ForgePoll {
                health,
                success: true,
                items: Some(items),
            }
        }
        ReviewSyncOutcome::RateLimited { until } => ForgePoll {
            health: ConnectorHealth::RateLimited { until: *until },
            success: false,
            items: None,
        },
        ReviewSyncOutcome::Failed { error } => {
            let error = error.clone();
            let health = match stop {
                Some(ForgeStop::Credential) => ConnectorHealth::Credential {
                    which: Secret::Token,
                    error,
                },
                Some(ForgeStop::Forbidden) => ConnectorHealth::Forbidden {
                    error,
                    failed: Vec::new(),
                },
                Some(ForgeStop::Unreachable) | None => {
                    ConnectorHealth::Unreachable { error, failures }
                }
            };
            ForgePoll {
                health,
                success: false,
                items: None,
            }
        }
    }
}

/// A connector instance or a forge credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    /// The context.
    pub context: ContextName,
    /// Its name. For a forge: its host.
    pub name: String,
    /// Its kind.
    pub kind: ConnectorKind,
    /// The host that loomd calls.
    pub host: String,
    /// The machine that polls it.
    pub polled_by: PolledBy,
    /// The time between two polls.
    pub interval: Duration,
}

/// Every forge credential of the configuration, held on `machine` or not, as an instance. Pure.
#[must_use]
pub fn forge_instances(config: &Config, machine: &MachineName) -> Vec<Instance> {
    config
        .forge_credentials()
        .into_iter()
        .map(|credential| {
            let machines = config
                .contexts
                .get(credential.context)
                .and_then(|context| context.machines.as_deref());
            let interval = config
                .forges
                .get(credential.host)
                .map_or(DEFAULT_POLL_INTERVAL, |forge| forge.poll_interval.get());
            Instance {
                context: credential.context.clone(),
                name: credential.host.as_str().to_owned(),
                kind: credential.kind.into(),
                host: credential.host.as_str().to_owned(),
                polled_by: match runner(machines, machine) {
                    Runner::This => PolledBy::ThisMachine,
                    Runner::Other(first) => PolledBy::Other {
                        machine: first.as_str().to_owned(),
                    },
                    Runner::Unpinned => PolledBy::EveryMachine,
                },
                interval,
            }
        })
        .collect()
}

/// What one poll tells [`ConnectorStatuses`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The start of the poll.
    pub at: Timestamp,
    /// The poll called the vendor: `last_attempt` moves. A poll that a rate limit skipped did not.
    pub attempted: bool,
    /// The poll succeeded: `last_success` moves.
    pub success: bool,
    /// The health.
    pub health: ConnectorHealth,
    /// The new counts; `None` keeps the last ones.
    pub items: Option<ConnectorCounts>,
    /// The time of the next poll.
    pub next_poll: Option<Timestamp>,
}

/// What a report changed in the health of an instance.
///
/// This is the seam of outage notifications: the poll thread gets it from
/// [`ConnectorStatuses::report`] and can count refusals in a row from it. Nothing delivers a
/// notification today (see "Connector status" in the design).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// The health before this report.
    pub previous: ConnectorHealth,
    /// The health after this report.
    pub current: ConnectorHealth,
}

/// One object of `state/connectors.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedTimes {
    context: ContextName,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_attempt: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_success: Option<Timestamp>,
}

/// The content of `state/connectors.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedState {
    v: u32,
    #[serde(default)]
    connectors: Vec<SavedTimes>,
}

/// The times that `path` holds. A missing file is empty, and an invalid one is logged and empty:
/// connector status must never stop loomd, and the next write replaces the file.
fn load_times(path: &Path) -> Vec<SavedTimes> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            warn!(path = %path.display(), %error, "cannot read the connector times");
            return Vec::new();
        }
    };
    match serde_json::from_slice::<SavedState>(&bytes) {
        Ok(state) if state.v == VERSION => state.connectors,
        Ok(state) => {
            warn!(path = %path.display(), v = state.v, "unknown version of the connector times, they are replaced at the next write");
            Vec::new()
        }
        Err(error) => {
            warn!(path = %path.display(), %error, "invalid connector times, they are replaced at the next write");
            Vec::new()
        }
    }
}

/// One instance and what it reported.
#[derive(Debug)]
struct Entry {
    instance: Instance,
    health: ConnectorHealth,
    last_attempt: Option<Timestamp>,
    last_success: Option<Timestamp>,
    next_poll: Option<Timestamp>,
    items: ConnectorCounts,
    /// `stale` at the last report: an event tells when it changes.
    stale: bool,
}

impl Entry {
    /// `polled_by: other` makes no claim: this machine does not know.
    fn stale_at(&self, now: Timestamp) -> bool {
        !matches!(self.instance.polled_by, PolledBy::Other { .. })
            && is_stale(self.last_success, self.instance.interval, now)
    }

    fn status(&self, now: Timestamp) -> ConnectorStatus {
        ConnectorStatus {
            context: self.instance.context.clone(),
            name: self.instance.name.clone(),
            kind: self.instance.kind,
            host: self.instance.host.clone(),
            polled_by: self.instance.polled_by.clone(),
            health: self.health.clone(),
            stale: self.stale_at(now),
            last_attempt: self.last_attempt,
            last_success: self.last_success,
            next_poll: self.next_poll,
            items: self.items,
            capabilities: self.instance.kind.capabilities(),
        }
    }
}

type Key = (ContextName, String);

#[derive(Debug)]
struct Inner {
    entries: BTreeMap<Key, Entry>,
    /// The bytes of the last write that succeeded: the same bytes write nothing, and a failed
    /// write leaves this as it was, so the next report tries again.
    written: Option<Vec<u8>>,
}

impl Inner {
    fn render(&self) -> serde_json::Result<Vec<u8>> {
        let connectors = self
            .entries
            .values()
            .filter(|entry| entry.last_attempt.is_some() || entry.last_success.is_some())
            .map(|entry| SavedTimes {
                context: entry.instance.context.clone(),
                name: entry.instance.name.clone(),
                last_attempt: entry.last_attempt,
                last_success: entry.last_success,
            })
            .collect();
        let mut bytes = serde_json::to_vec_pretty(&SavedState {
            v: VERSION,
            connectors,
        })?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Writes the file when its bytes change. A failure is logged and never stops a poll.
    fn save(&mut self, path: &Path) {
        let bytes = match self.render() {
            Ok(bytes) => bytes,
            Err(error) => {
                warn!(%error, "cannot encode the connector times");
                return;
            }
        };
        if self.written.as_ref() == Some(&bytes) {
            return;
        }
        match write_state(path, &bytes) {
            Ok(()) => self.written = Some(bytes),
            Err(error) => {
                warn!(path = %path.display(), %error, "cannot write the connector times");
            }
        }
    }
}

/// Creates `state/` (mode 0700), restricts an existing one, and replaces the file atomically.
fn write_state(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        DirBuilder::new()
            .recursive(true)
            .mode(STATE_DIR_MODE)
            .create(dir)?;
        fs::set_permissions(dir, Permissions::from_mode(STATE_DIR_MODE))?;
    }
    atomic::write_file(path, bytes, STATE_FILE_MODE)
}

/// The status of every instance, and the one writer of `state/connectors.json`. Each poll thread
/// shares it. The file is written under the mutex, so an older report cannot replace a newer one.
#[derive(Debug)]
pub struct ConnectorStatuses {
    path: PathBuf,
    inner: Mutex<Inner>,
    events: Arc<Bus>,
}

impl ConnectorStatuses {
    /// The status of `instances`, with the times that `path` holds. Times of an instance that is
    /// not in the list leave the file at the next write.
    #[must_use]
    pub fn open(path: PathBuf, instances: Vec<Instance>, events: Arc<Bus>, now: Timestamp) -> Self {
        let mut times: BTreeMap<Key, SavedTimes> = load_times(&path)
            .into_iter()
            .map(|saved| ((saved.context.clone(), saved.name.clone()), saved))
            .collect();
        let entries = instances
            .into_iter()
            .map(|instance| {
                let key = (instance.context.clone(), instance.name.clone());
                let saved = times.remove(&key);
                let mut entry = Entry {
                    instance,
                    health: ConnectorHealth::NotPolledYet,
                    last_attempt: saved.as_ref().and_then(|saved| saved.last_attempt),
                    last_success: saved.as_ref().and_then(|saved| saved.last_success),
                    next_poll: None,
                    items: ConnectorCounts::default(),
                    stale: false,
                };
                entry.stale = entry.stale_at(now);
                (key, entry)
            })
            .collect();
        Self {
            path,
            inner: Mutex::new(Inner {
                entries,
                written: None,
            }),
            events,
        }
    }

    /// Every instance at `now`, or those of `context`, sorted by context, then name.
    #[must_use]
    pub fn list(&self, context: Option<&ContextName>, now: Timestamp) -> ConnectorList {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        ConnectorList {
            connectors: inner
                .entries
                .values()
                .filter(|entry| context.is_none_or(|context| *context == entry.instance.context))
                .map(|entry| entry.status(now))
                .collect(),
        }
    }

    /// The first poll of an instance is at `next_poll`. Writes nothing and sends no event.
    pub fn schedule(&self, context: &ContextName, name: &str, next_poll: Timestamp) {
        if let Some(entry) = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .get_mut(&(context.clone(), name.to_owned()))
        {
            entry.next_poll = Some(next_poll);
        }
    }

    /// Records a poll of an instance, writes the file when its bytes change, and sends
    /// `connector.changed` when the health, the counts or `stale` changed. `None` for an instance
    /// that is not configured.
    pub fn report(&self, context: &ContextName, name: &str, report: Report) -> Option<Transition> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = inner.entries.get_mut(&(context.clone(), name.to_owned()))?;
        let previous_items = entry.items;
        let previous_stale = entry.stale;
        let previous = std::mem::replace(&mut entry.health, report.health);
        if report.attempted {
            entry.last_attempt = Some(report.at);
        }
        if report.success {
            entry.last_success = Some(report.at);
        }
        if let Some(items) = report.items {
            entry.items = items;
        }
        entry.next_poll = report.next_poll;
        entry.stale = entry.stale_at(report.at);
        let transition = Transition {
            previous,
            current: entry.health.clone(),
        };
        let changed = transition.previous != transition.current
            || previous_items != entry.items
            || previous_stale != entry.stale;
        inner.save(&self.path);
        drop(inner);
        // After the status: a client that reads it on this event sees the change.
        if changed {
            self.events.publish(ChangeEvent::ConnectorChanged {
                context: context.clone(),
                name: name.to_owned(),
            });
        }
        Some(transition)
    }
}

#[cfg(test)]
mod tests;
