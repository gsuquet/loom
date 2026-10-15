//! Tasks: kinds, statuses, importance, links and relations.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use crate::Date;
use crate::context::ContextName;
use crate::id::TaskId;
use crate::timestamp::Timestamp;

/// Something I still have to do.
///
/// Serialized flat: `kind` and `format` for [`Kind`], `status`, `waiting_on` and `waiting_until`
/// for [`Status`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Task {
    /// Identifier, also the name of the task directory.
    pub id: TaskId,
    /// One-line title.
    pub title: String,
    /// Nature of the work.
    #[serde(flatten)]
    pub kind: Kind,
    /// The one context the task belongs to.
    pub context: ContextName,
    /// Where the task is in its lifecycle.
    #[serde(flatten)]
    pub status: Status,
    /// p0 (highest) to p3.
    pub importance: Importance,
    /// Due date, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<Date>,
    /// Pinned tasks come first in the priority list.
    #[serde(default)]
    pub pinned: bool,
    /// Links to the outside: origin, related items, repository.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// Relations to other tasks.
    #[serde(default)]
    pub relations: Relations,
    /// When the task was created.
    pub created: Timestamp,
    /// When the task last changed.
    pub updated: Timestamp,
}

impl Task {
    /// Why this version of loom cannot change the task, if it cannot: a kind, content format or
    /// status written by a newer version. Such a task is shown read-only, never rewritten.
    #[must_use]
    pub fn read_only_reason(&self) -> Option<ReadOnlyReason> {
        if let Some(reason) = self.kind.unknown() {
            return Some(reason);
        }
        match &self.status {
            Status::Unknown(name) => Some(ReadOnlyReason::UnknownStatus(name.clone())),
            Status::Inbox
            | Status::Ready
            | Status::Active
            | Status::Waiting { .. }
            | Status::Done
            | Status::Cancelled => None,
        }
    }
}

/// Why a task is read-only.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "reason", content = "value", rename_all = "snake_case")]
pub enum ReadOnlyReason {
    /// The kind is unknown to this version of loom.
    #[error("unknown kind `{0}`, written by a newer version of loom")]
    UnknownKind(String),
    /// The content format is unknown to this version of loom.
    #[error("unknown content format `{0}`, written by a newer version of loom")]
    UnknownContentFormat(String),
    /// The status is unknown to this version of loom.
    #[error("unknown status `{0}`, written by a newer version of loom")]
    UnknownStatus(String),
}

/// Nature of the work: drives the body template, the done condition and the default importance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "KindRepr", into = "KindRepr")]
pub enum Kind {
    /// Delivered: MR merged, remote closed.
    Story,
    /// Fix merged and verified.
    Bug,
    /// Done.
    Chore,
    /// Conclusion written: findings and decision.
    Research,
    /// Cause identified and disposition taken.
    Investigation,
    /// Verdict posted for the current head SHA.
    Review,
    /// The requester has an answer.
    Support,
    /// Delivered or published.
    Content {
        /// Talk, article, training or workshop, if set.
        format: Option<ContentFormat>,
    },
    /// Done (timesheets, expenses, staffing).
    Admin,
    /// Feedback written.
    Interview,
    /// Course or certification completed.
    Learning,
    /// A kind written by a newer version of loom: the task is read-only.
    Unknown {
        /// Name of the kind.
        name: String,
        /// Its `format`, kept as is.
        format: Option<String>,
    },
}

/// Format of a [`Kind::Content`] task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentFormat {
    /// A talk.
    Talk,
    /// An article.
    Article,
    /// A training.
    Training,
    /// A workshop.
    Workshop,
    /// A format written by a newer version of loom: the task is read-only.
    Unknown(String),
}

impl Kind {
    /// Name of the kind, as written in `kind`.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Story => "story",
            Self::Bug => "bug",
            Self::Chore => "chore",
            Self::Research => "research",
            Self::Investigation => "investigation",
            Self::Review => "review",
            Self::Support => "support",
            Self::Content { .. } => "content",
            Self::Admin => "admin",
            Self::Interview => "interview",
            Self::Learning => "learning",
            Self::Unknown { name, .. } => name,
        }
    }

    /// The reason a task of this kind is read-only, if the kind or its format is unknown.
    pub(crate) fn unknown(&self) -> Option<ReadOnlyReason> {
        match self {
            Self::Unknown { name, .. } => Some(ReadOnlyReason::UnknownKind(name.clone())),
            Self::Content {
                format: Some(ContentFormat::Unknown(format)),
            } => Some(ReadOnlyReason::UnknownContentFormat(format.clone())),
            Self::Story
            | Self::Bug
            | Self::Chore
            | Self::Research
            | Self::Investigation
            | Self::Review
            | Self::Support
            | Self::Content { .. }
            | Self::Admin
            | Self::Interview
            | Self::Learning => None,
        }
    }
}

impl ContentFormat {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Talk => "talk",
            Self::Article => "article",
            Self::Training => "training",
            Self::Workshop => "workshop",
            Self::Unknown(name) => name,
        }
    }

    fn from_name(name: String) -> Self {
        match name.as_str() {
            "talk" => Self::Talk,
            "article" => Self::Article,
            "training" => Self::Training,
            "workshop" => Self::Workshop,
            _ => Self::Unknown(name),
        }
    }
}

/// Serialized form of [`Kind`]: `kind`, plus `format` for content.
#[derive(Serialize, Deserialize, JsonSchema)]
struct KindRepr {
    /// story, bug, chore, research, investigation, review, support, content, admin, interview
    /// or learning. Any other name makes the task read-only.
    kind: String,
    /// Content only: talk, article, training or workshop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    format: Option<String>,
}

impl From<Kind> for KindRepr {
    fn from(kind: Kind) -> Self {
        let format = match &kind {
            Kind::Content { format } => format.as_ref().map(|format| format.name().to_owned()),
            Kind::Unknown { format, .. } => format.clone(),
            Kind::Story
            | Kind::Bug
            | Kind::Chore
            | Kind::Research
            | Kind::Investigation
            | Kind::Review
            | Kind::Support
            | Kind::Admin
            | Kind::Interview
            | Kind::Learning => None,
        };
        Self {
            kind: kind.name().to_owned(),
            format,
        }
    }
}

impl From<KindRepr> for Kind {
    fn from(repr: KindRepr) -> Self {
        match repr.kind.as_str() {
            "story" => Self::Story,
            "bug" => Self::Bug,
            "chore" => Self::Chore,
            "research" => Self::Research,
            "investigation" => Self::Investigation,
            "review" => Self::Review,
            "support" => Self::Support,
            "content" => Self::Content {
                format: repr.format.map(ContentFormat::from_name),
            },
            "admin" => Self::Admin,
            "interview" => Self::Interview,
            "learning" => Self::Learning,
            _ => Self::Unknown {
                name: repr.kind,
                format: repr.format,
            },
        }
    }
}

impl JsonSchema for Kind {
    fn schema_name() -> Cow<'static, str> {
        "Kind".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        KindRepr::json_schema(generator)
    }
}

/// Where a task is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StatusRepr", into = "StatusRepr")]
pub enum Status {
    /// To triage; the default for ingested items.
    Inbox,
    /// Triaged, in the priority list.
    Ready,
    /// Being worked on.
    Active,
    /// Blocked on someone or something.
    Waiting {
        /// What or whom the task waits on.
        on: Option<String>,
        /// Hidden from the priority list until this date.
        until: Option<Date>,
    },
    /// Finished.
    Done,
    /// Dropped.
    Cancelled,
    /// A status written by a newer version of loom: the task is read-only.
    Unknown(String),
}

/// Name of a known [`Status`], without the details of `waiting`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusName {
    /// [`Status::Inbox`].
    Inbox,
    /// [`Status::Ready`].
    Ready,
    /// [`Status::Active`].
    Active,
    /// [`Status::Waiting`].
    Waiting,
    /// [`Status::Done`].
    Done,
    /// [`Status::Cancelled`].
    Cancelled,
}

impl StatusName {
    /// Every status name, in lifecycle order.
    pub const ALL: [Self; 6] = [
        Self::Inbox,
        Self::Ready,
        Self::Active,
        Self::Waiting,
        Self::Done,
        Self::Cancelled,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Ready => "ready",
            Self::Active => "active",
            Self::Waiting => "waiting",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for StatusName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a task can move from `from` to `to`.
///
/// - An open status (inbox, ready, active, waiting) can move to any other status.
/// - `waiting` can move to `waiting`, to change what it waits on or until when.
/// - `done` and `cancelled` can only be reopened, to `ready`.
#[must_use]
pub const fn is_legal_transition(from: StatusName, to: StatusName) -> bool {
    match from {
        StatusName::Inbox | StatusName::Ready | StatusName::Active => from as u8 != to as u8,
        StatusName::Waiting => true,
        StatusName::Done | StatusName::Cancelled => matches!(to, StatusName::Ready),
    }
}

impl Status {
    /// Name of the status; `None` when it is unknown.
    #[must_use]
    pub const fn name(&self) -> Option<StatusName> {
        match self {
            Self::Inbox => Some(StatusName::Inbox),
            Self::Ready => Some(StatusName::Ready),
            Self::Active => Some(StatusName::Active),
            Self::Waiting { .. } => Some(StatusName::Waiting),
            Self::Done => Some(StatusName::Done),
            Self::Cancelled => Some(StatusName::Cancelled),
            Self::Unknown(_) => None,
        }
    }
}

/// Serialized form of [`Status`]: `status`, plus `waiting_on` and `waiting_until` for `waiting`.
#[derive(Serialize, Deserialize, JsonSchema)]
struct StatusRepr {
    /// inbox, ready, active, waiting, done or cancelled. Any other name makes the task read-only.
    status: String,
    /// `waiting` only: what or whom the task waits on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    waiting_on: Option<String>,
    /// `waiting` only: the priority list does not show the task until this date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    waiting_until: Option<Date>,
}

impl From<Status> for StatusRepr {
    fn from(status: Status) -> Self {
        let name = status.name().map(StatusName::as_str);
        match status {
            Status::Waiting { on, until } => Self {
                status: "waiting".to_owned(),
                waiting_on: on,
                waiting_until: until,
            },
            Status::Unknown(name) => Self {
                status: name,
                waiting_on: None,
                waiting_until: None,
            },
            Status::Inbox | Status::Ready | Status::Active | Status::Done | Status::Cancelled => {
                Self {
                    status: name.unwrap_or_default().to_owned(),
                    waiting_on: None,
                    waiting_until: None,
                }
            }
        }
    }
}

impl From<StatusRepr> for Status {
    fn from(repr: StatusRepr) -> Self {
        match repr.status.as_str() {
            "inbox" => Self::Inbox,
            "ready" => Self::Ready,
            "active" => Self::Active,
            "waiting" => Self::Waiting {
                on: repr.waiting_on,
                until: repr.waiting_until,
            },
            "done" => Self::Done,
            "cancelled" => Self::Cancelled,
            _ => Self::Unknown(repr.status),
        }
    }
}

impl JsonSchema for Status {
    fn schema_name() -> Cow<'static, str> {
        "Status".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        StatusRepr::json_schema(generator)
    }
}

/// Importance, p0 (highest) to p3.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Importance {
    /// Highest.
    P0,
    /// High.
    P1,
    /// Normal.
    P2,
    /// Low.
    P3,
}

impl Importance {
    /// Importance of a new task of `kind` when none is given: p1 when someone waits on me (bug,
    /// investigation, support, review), p3 for chores, research and learning, p2 otherwise.
    #[must_use]
    pub const fn default_for(kind: &Kind) -> Self {
        match kind {
            Kind::Bug | Kind::Investigation | Kind::Support | Kind::Review => Self::P1,
            Kind::Story
            | Kind::Content { .. }
            | Kind::Admin
            | Kind::Interview
            | Kind::Unknown { .. } => Self::P2,
            Kind::Chore | Kind::Research | Kind::Learning => Self::P3,
        }
    }
}

/// A link from a task to the outside, by role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Link {
    /// Where the task came from (dedup key `(provider, key)`).
    Origin(ExternalRef),
    /// An item that belongs to the same done condition: an MR, a CI pipeline.
    Related(ExternalRef),
    /// The repository to work in, a slug of the repo index.
    Repo {
        /// Repo slug, such as `company-a/checkout`.
        repo: String,
    },
}

/// An item of a forge or tracker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExternalRef {
    /// Provider, such as `jira` or `gitlab`.
    pub provider: String,
    /// Key at the provider, such as `CORE-1042` or `!88`.
    pub key: String,
    /// Web URL, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Relations of a task to other tasks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Relations {
    /// The parent task: a project is a parent task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<TaskId>,
    /// Tasks this one was spawned from (a Bug spawned from a Support task).
    #[serde(default)]
    pub spawned_from: Vec<TaskId>,
    /// Tasks this one blocks.
    #[serde(default)]
    pub blocks: Vec<TaskId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> serde_json::Value {
        json!({
            "id": "k3q7zm2a",
            "title": "Login timeout on mobile checkout",
            "kind": "content",
            "format": "talk",
            "context": "company-a",
            "status": "waiting",
            "waiting_on": "review from Alice",
            "waiting_until": "2026-10-20",
            "importance": "p1",
            "due": "2026-10-15",
            "pinned": false,
            "links": [
                { "role": "origin", "provider": "jira", "key": "CORE-1042", "url": "https://jira.example/CORE-1042" },
                { "role": "repo", "repo": "company-a/checkout" }
            ],
            "relations": { "parent": "h8w2nc4d", "spawned_from": [], "blocks": [] },
            "created": "2026-10-12T09:14:00Z",
            "updated": "2026-10-12T09:14:00Z"
        })
    }

    #[test]
    fn a_task_round_trips_through_its_flat_serialized_form() {
        let task: Task = serde_json::from_value(sample()).unwrap();
        assert_eq!(
            task.kind,
            Kind::Content {
                format: Some(ContentFormat::Talk)
            }
        );
        assert_eq!(
            task.status,
            Status::Waiting {
                on: Some("review from Alice".to_owned()),
                until: Some("2026-10-20".parse().unwrap()),
            }
        );
        assert_eq!(task.read_only_reason(), None);
        assert_eq!(serde_json::to_value(&task).unwrap(), sample());
    }

    #[test]
    fn unknown_kinds_formats_and_statuses_are_kept_and_make_the_task_read_only() {
        let mut unknown_kind = sample();
        unknown_kind["kind"] = json!("meeting");
        let mut unknown_format = sample();
        unknown_format["format"] = json!("podcast");
        let mut unknown_status = sample();
        unknown_status["status"] = json!("snoozed");
        let object = unknown_status.as_object_mut().unwrap();
        object.remove("waiting_on");
        object.remove("waiting_until");
        let cases = [
            (
                unknown_kind,
                ReadOnlyReason::UnknownKind("meeting".to_owned()),
            ),
            (
                unknown_format,
                ReadOnlyReason::UnknownContentFormat("podcast".to_owned()),
            ),
            (
                unknown_status,
                ReadOnlyReason::UnknownStatus("snoozed".to_owned()),
            ),
        ];
        for (value, reason) in cases {
            let task: Task = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(task.read_only_reason(), Some(reason));
            assert_eq!(serde_json::to_value(&task).unwrap(), value);
        }
    }

    #[test]
    fn every_kind_name_round_trips() {
        let kinds = [
            Kind::Story,
            Kind::Bug,
            Kind::Chore,
            Kind::Research,
            Kind::Investigation,
            Kind::Review,
            Kind::Support,
            Kind::Content { format: None },
            Kind::Content {
                format: Some(ContentFormat::Workshop),
            },
            Kind::Admin,
            Kind::Interview,
            Kind::Learning,
        ];
        for kind in kinds {
            let value = serde_json::to_value(&kind).unwrap();
            assert_eq!(serde_json::from_value::<Kind>(value).unwrap(), kind);
            assert_eq!(kind.unknown(), None);
        }
    }

    #[test]
    fn default_importance_per_kind() {
        assert_eq!(Importance::default_for(&Kind::Bug), Importance::P1);
        assert_eq!(Importance::default_for(&Kind::Story), Importance::P2);
        assert_eq!(Importance::default_for(&Kind::Learning), Importance::P3);
        assert!(Importance::P0 < Importance::P3);
    }

    /// The full table: 6 x 6 pairs.
    #[test]
    fn transition_table() {
        use StatusName::{Active, Cancelled, Done, Inbox, Ready, Waiting};
        let legal = [
            (Inbox, [Ready, Active, Waiting, Done, Cancelled].as_slice()),
            (Ready, &[Inbox, Active, Waiting, Done, Cancelled]),
            (Active, &[Inbox, Ready, Waiting, Done, Cancelled]),
            (Waiting, &[Inbox, Ready, Active, Waiting, Done, Cancelled]),
            (Done, &[Ready]),
            (Cancelled, &[Ready]),
        ];
        for (from, targets) in legal {
            for to in StatusName::ALL {
                assert_eq!(
                    is_legal_transition(from, to),
                    targets.contains(&to),
                    "{from} -> {to}"
                );
            }
        }
    }
}
