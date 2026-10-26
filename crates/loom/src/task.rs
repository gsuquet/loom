//! `loom add|ls|show`: the context a task goes to, the request, and the output.
//!
//! No I/O: `task_cmd` reads the environment, the configuration and the index, and calls loomd.

use std::fmt::Write as _;

use jiff::Span;
use loom_api::{
    Actor, ContentFormat, ContextAssignment, ContextName, Date, EventRecord, FocusList, GoResult,
    Importance, Kind, Link, NewTask, RankReason, Relations, RepoSummary, SkippedLine, Status, Task,
    TaskDetail, TaskEdit, TaskId, TaskReadOnly, TaskSummary,
};
use loom_core::SkipReason;
use thiserror::Error;

use crate::cli::StatusArg;

/// Date of today for the tests, honored by debug builds only, as by loomd.
pub const TEST_TODAY_ENV: &str = "LOOM_TEST_TODAY";

/// Today: `test` (a `YYYY-MM-DD` [`TEST_TODAY_ENV`]) when `allowed`, otherwise `local`.
#[must_use]
pub fn today(test: Option<&str>, allowed: bool, local: impl FnOnce() -> Date) -> Date {
    test.filter(|_| allowed)
        .and_then(|text| text.parse().ok())
        .unwrap_or_else(local)
}

/// Environment variable naming the context, after `-c`.
pub const CONTEXT_ENV: &str = "LOOM_CONTEXT";

/// Which rule gave the context of a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextSource {
    /// `-c`/`--context`.
    Flag,
    /// [`CONTEXT_ENV`].
    Env,
    /// The repository of the current directory: a `[repos."<path>"]` override or its remote.
    Repo,
    /// The `match.paths` prefix of the current directory.
    Path,
    /// `default_context`.
    Default,
}

/// A context and the rule that gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedContext {
    /// The context.
    pub context: ContextName,
    /// The rule.
    pub source: ContextSource,
}

/// Why no context could be resolved.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextError {
    /// [`CONTEXT_ENV`] is set to an invalid name.
    #[error("{CONTEXT_ENV}={value} is not a context name: {reason}")]
    InvalidEnv {
        /// The value.
        value: String,
        /// Why it is invalid.
        reason: String,
    },
    /// No rule gives a context.
    #[error(
        "no context: pass -c <name>, set {CONTEXT_ENV}, run inside a repository or a `match.paths` directory of a context, or set `default_context` in config.toml"
    )]
    Missing,
}

/// The context of a command, first rule that gives one:
///
/// 1. `flag` (`-c`);
/// 2. `env` ([`CONTEXT_ENV`]), an empty value being unset;
/// 3. `cwd_repo`, the assignment of the indexed repository holding the current directory;
/// 4. `cwd_rule`, what the context rules give the current directory itself: its `match.paths`
///    prefix, then `default_context`.
///
/// An unassigned repository or directory gives nothing.
///
/// # Errors
///
/// [`ContextError::InvalidEnv`] for a bad [`CONTEXT_ENV`], [`ContextError::Missing`] when no
/// rule gives a context.
pub fn resolve_context(
    flag: Option<&ContextName>,
    env: Option<&str>,
    cwd_repo: Option<&ContextAssignment>,
    cwd_rule: Option<&ContextAssignment>,
) -> Result<ResolvedContext, ContextError> {
    if let Some(context) = flag {
        return Ok(ResolvedContext {
            context: context.clone(),
            source: ContextSource::Flag,
        });
    }
    if let Some(value) = env.filter(|value| !value.is_empty()) {
        let context = value
            .parse::<ContextName>()
            .map_err(|error| ContextError::InvalidEnv {
                value: value.to_owned(),
                reason: error.to_string(),
            })?;
        return Ok(ResolvedContext {
            context,
            source: ContextSource::Env,
        });
    }
    cwd_repo
        .and_then(assigned)
        .or_else(|| cwd_rule.and_then(assigned))
        .ok_or(ContextError::Missing)
}

fn assigned(assignment: &ContextAssignment) -> Option<ResolvedContext> {
    let (context, source) = match assignment {
        ContextAssignment::Override { context } | ContextAssignment::Host { context, .. } => {
            (context, ContextSource::Repo)
        }
        ContextAssignment::Path { context, .. } => (context, ContextSource::Path),
        ContextAssignment::Default { context } => (context, ContextSource::Default),
        ContextAssignment::Unassigned => return None,
    };
    Some(ResolvedContext {
        context: context.clone(),
        source,
    })
}

/// A date given on the command line, relative to `today`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid date {0:?}: expected YYYY-MM-DD, today, tomorrow, +<n>d (days) or +<n>w (weeks)")]
pub struct DateError(pub String);

/// Parses `YYYY-MM-DD`, `today`, `tomorrow`, `+<n>d` or `+<n>w`.
///
/// # Errors
///
/// [`DateError`] for anything else, or a date out of range.
pub fn parse_date(text: &str, today: Date) -> Result<Date, DateError> {
    let invalid = || DateError(text.to_owned());
    let days: i64 = match text {
        "today" => 0,
        "tomorrow" => 1,
        _ => {
            let Some(offset) = text.strip_prefix('+') else {
                return text.parse::<Date>().map_err(|_| invalid());
            };
            // On the unit, never on a byte index: the text may hold any character.
            let (count, factor) = if let Some(count) = offset.strip_suffix('d') {
                (count, 1)
            } else if let Some(count) = offset.strip_suffix('w') {
                (count, 7)
            } else {
                return Err(invalid());
            };
            // Digits only: `parse` alone would take a sign (`++3d`).
            if count.is_empty() || !count.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid());
            }
            count
                .parse::<i64>()
                .ok()
                .and_then(|count| count.checked_mul(factor))
                .ok_or_else(invalid)?
        }
    };
    let span = Span::new().try_days(days).map_err(|_| invalid())?;
    today.checked_add(span).map_err(|_| invalid())
}

/// Length of a task ID.
const ID_LEN: usize = 8;

/// Checks a task ID or ID prefix given on the command line.
///
/// 1 to 8 characters of Crockford base32, case ignored, `i`, `l` and `o` accepted (loomd reads
/// them as `1`, `1` and `0`). Used as a `clap` value parser, so a bad value is a usage error.
///
/// # Errors
///
/// A message naming the value.
pub fn task_prefix(text: &str) -> Result<String, String> {
    let valid = (1..=ID_LEN).contains(&text.len())
        && text.bytes().all(|byte| {
            let byte = byte.to_ascii_lowercase();
            byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte != b'u')
        });
    if valid {
        Ok(text.to_owned())
    } else {
        Err(format!(
            "{text:?} is not a task ID or prefix: 1 to {ID_LEN} letters and digits (Crockford base32, no `u`)"
        ))
    }
}

/// Why `loom add --parent` cannot use a context.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParentContextError {
    /// The context was chosen with `-c` or [`CONTEXT_ENV`] and differs from the parent's.
    #[error(
        "the parent {parent} is in context {parent_context}, not {chosen}: a task and its parent share one context"
    )]
    Mismatch {
        /// The parent's ID.
        parent: TaskId,
        /// Its context.
        parent_context: ContextName,
        /// The context chosen explicitly.
        chosen: ContextName,
    },
    /// The context of the command could not be resolved.
    #[error(transparent)]
    Context(#[from] ContextError),
}

/// The context of a task added with `--parent`: a task and its parent share one context.
///
/// A context chosen explicitly (`-c`, [`CONTEXT_ENV`]) must be the parent's; one inferred
/// from the current directory, or none, gives way to the parent's.
///
/// # Errors
///
/// [`ParentContextError::Mismatch`] for an explicit context other than the parent's; an
/// invalid [`CONTEXT_ENV`].
pub fn context_with_parent(
    resolved: Result<ResolvedContext, ContextError>,
    parent: &TaskId,
    parent_context: &ContextName,
) -> Result<ContextName, ParentContextError> {
    match resolved {
        Ok(ResolvedContext {
            context,
            source: ContextSource::Flag | ContextSource::Env,
        }) if &context != parent_context => Err(ParentContextError::Mismatch {
            parent: parent.clone(),
            parent_context: parent_context.clone(),
            chosen: context,
        }),
        Ok(_) | Err(ContextError::Missing) => Ok(parent_context.clone()),
        Err(error @ ContextError::InvalidEnv { .. }) => Err(error.into()),
    }
}

/// Kinds accepted by `--kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum KindArg {
    /// Delivered: MR merged, remote closed.
    Story,
    /// Fix merged and verified.
    Bug,
    /// Done.
    Chore,
    /// Conclusion written.
    Research,
    /// Cause identified and disposition taken.
    Investigation,
    /// Verdict posted.
    Review,
    /// The requester has an answer.
    Support,
    /// Delivered or published; `--format` tells which.
    Content,
    /// Timesheets, expenses, staffing.
    Admin,
    /// Feedback written.
    Interview,
    /// Course or certification completed.
    Learning,
}

/// Formats accepted by `--format`, with `--kind content`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FormatArg {
    /// A talk.
    Talk,
    /// An article.
    Article,
    /// A training.
    Training,
    /// A workshop.
    Workshop,
}

/// The kind of `--kind` and `--format`.
#[must_use]
pub fn kind_of(kind: KindArg, format: Option<FormatArg>) -> Kind {
    match kind {
        KindArg::Story => Kind::Story,
        KindArg::Bug => Kind::Bug,
        KindArg::Chore => Kind::Chore,
        KindArg::Research => Kind::Research,
        KindArg::Investigation => Kind::Investigation,
        KindArg::Review => Kind::Review,
        KindArg::Support => Kind::Support,
        KindArg::Content => Kind::Content {
            format: format.map(|format| match format {
                FormatArg::Talk => ContentFormat::Talk,
                FormatArg::Article => ContentFormat::Article,
                FormatArg::Training => ContentFormat::Training,
                FormatArg::Workshop => ContentFormat::Workshop,
            }),
        },
        KindArg::Admin => Kind::Admin,
        KindArg::Interview => Kind::Interview,
        KindArg::Learning => Kind::Learning,
    }
}

/// Importances accepted by `--importance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ImportanceArg {
    /// Highest.
    P0,
    /// High.
    P1,
    /// Normal.
    P2,
    /// Low.
    P3,
}

impl From<ImportanceArg> for Importance {
    fn from(importance: ImportanceArg) -> Self {
        match importance {
            ImportanceArg::P0 => Self::P0,
            ImportanceArg::P1 => Self::P1,
            ImportanceArg::P2 => Self::P2,
            ImportanceArg::P3 => Self::P3,
        }
    }
}

/// What `loom add` asks for, once the context, the parent, the repository and the dates are
/// resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddRequest {
    /// Title.
    pub title: String,
    /// Context.
    pub context: ContextName,
    /// Kind.
    pub kind: Kind,
    /// Importance; the default of the kind when `None`.
    pub importance: Option<Importance>,
    /// Due date.
    pub due: Option<Date>,
    /// Parent task.
    pub parent: Option<TaskId>,
    /// Repository slug.
    pub repo: Option<String>,
    /// `--ready`: skip the inbox.
    pub ready: bool,
}

/// Default repository of `loom add`: the repository of the current directory when its context
/// is the task's.
#[must_use]
pub fn default_repo(cwd_repo: Option<&RepoSummary>, context: &ContextName) -> Option<String> {
    let repo = cwd_repo?;
    (repo.identity.context.context_name() == context.as_str()).then(|| repo.identity.slug.clone())
}

/// The body of `POST /v1/tasks`: `inbox` unless `--ready`, always set explicitly.
#[must_use]
pub fn new_task(request: AddRequest) -> NewTask {
    NewTask {
        title: request.title,
        kind: request.kind,
        context: request.context,
        status: if request.ready {
            Status::Ready
        } else {
            Status::Inbox
        },
        importance: request.importance,
        due: request.due,
        pinned: false,
        links: request
            .repo
            .map(|repo| Link::Repo { repo })
            .into_iter()
            .collect(),
        relations: Relations {
            parent: request.parent,
            ..Relations::default()
        },
    }
}

/// Slug of the first [`Link::Repo`] of `task`.
#[must_use]
pub fn linked_repo(task: &Task) -> Option<&str> {
    task.links.iter().find_map(|link| match link {
        Link::Repo { repo } => Some(repo.as_str()),
        Link::Origin(_) | Link::Related(_) => None,
    })
}

/// `links` with `repo` as its [`Link::Repo`] (`None` removes it), the other links kept in
/// order.
#[must_use]
pub fn with_repo(links: &[Link], repo: Option<String>) -> Vec<Link> {
    let mut out: Vec<Link> = links
        .iter()
        .filter(|link| !matches!(link, Link::Repo { .. }))
        .cloned()
        .collect();
    if let Some(repo) = repo {
        out.push(Link::Repo { repo });
    }
    out
}

/// `GET /v1/tasks` query of `loom ls`: the priority list, every task with `status`, or the
/// archived tasks (with `status` when given), sorted by ID; one context, or all of them.
#[must_use]
pub fn list_api_path(
    context: Option<&ContextName>,
    status: Option<&str>,
    archived: bool,
) -> String {
    let mut query = Vec::new();
    if let Some(context) = context {
        query.push(format!("context={context}"));
    }
    if let Some(status) = status {
        query.push(format!("status={status}"));
    }
    if archived {
        query.push("archived=true".to_owned());
    } else if status.is_none() {
        query.push("ranked=true".to_owned());
    }
    format!("/v1/tasks?{}", query.join("&"))
}

/// `GET /v1/focus` path, filtered by `context` when given.
#[must_use]
pub fn focus_api_path(context: Option<&ContextName>) -> String {
    context.map_or_else(
        || "/v1/focus".to_owned(),
        |context| format!("/v1/focus?context={context}"),
    )
}

/// `--why` text of a rank reason.
#[must_use]
pub const fn reason_text(reason: RankReason) -> &'static str {
    match reason {
        RankReason::First => "first",
        RankReason::Pinned => "pinned",
        RankReason::Urgent => "overdue or due today",
        RankReason::Importance => "more important",
        RankReason::DueDate => "due earlier",
        RankReason::Age => "older",
        RankReason::Id => "tie, by id",
    }
}

fn status_text(status: &Status) -> String {
    match status {
        Status::Inbox => "inbox".to_owned(),
        Status::Ready => "ready".to_owned(),
        Status::Active => "active".to_owned(),
        Status::Waiting { on, until } => {
            let mut text = "waiting".to_owned();
            if let Some(on) = on {
                let _ = write!(text, " on {on}");
            }
            if let Some(until) = until {
                let _ = write!(text, " until {until}");
            }
            text
        }
        Status::Done => "done".to_owned(),
        Status::Cancelled => "cancelled".to_owned(),
        Status::Unknown(name) => name.clone(),
    }
}

const fn importance_text(importance: Importance) -> &'static str {
    match importance {
        Importance::P0 => "p0",
        Importance::P1 => "p1",
        Importance::P2 => "p2",
        Importance::P3 => "p3",
    }
}

#[must_use]
pub fn read_only_text(read_only: &TaskReadOnly) -> String {
    match read_only {
        TaskReadOnly::UnknownKind(value) => format!("unknown kind {value}"),
        TaskReadOnly::UnknownContentFormat(value) => format!("unknown content format {value}"),
        TaskReadOnly::UnknownStatus(value) => format!("unknown status {value}"),
        TaskReadOnly::InvalidFile(error) => format!("invalid task.md: {error}"),
        TaskReadOnly::Archived(year) => format!("archived in archive/{year}"),
    }
}

/// Table of `loom ls`: a `#` column when the list is ranked, a `WHY` column with `why`.
#[must_use]
pub fn render_list(tasks: &[TaskSummary], why: bool) -> String {
    if tasks.is_empty() {
        return String::new();
    }
    let ranked = tasks.iter().any(|task| task.rank.is_some());
    let mut header = vec!["ID", "CONTEXT", "STATUS", "P", "KIND", "DUE", "TITLE"];
    if ranked {
        header.insert(0, "#");
    }
    if why {
        header.push("WHY");
    }
    let mut rows: Vec<Vec<String>> = vec![header.iter().map(|&cell| cell.to_owned()).collect()];
    for summary in tasks {
        let mut row = Vec::with_capacity(header.len());
        if ranked {
            row.push(
                summary
                    .rank
                    .map_or_else(String::new, |rank| rank.position.to_string()),
            );
        }
        row.push(summary.prefix.clone());
        row.push(summary.context.to_string());
        if let Some(task) = &summary.task {
            row.push(status_text(&task.status));
            row.push(importance_text(task.importance).to_owned());
            row.push(task.kind.name().to_owned());
            row.push(task.due.map_or_else(String::new, |due| due.to_string()));
            let pin = if task.pinned { "* " } else { "" };
            row.push(format!("{pin}{}", task.title));
        } else {
            row.extend(["", "", "", ""].map(str::to_owned));
            row.push(
                summary
                    .read_only
                    .as_ref()
                    .map_or_else(String::new, read_only_text),
            );
        }
        if why {
            row.push(
                summary
                    .rank
                    .map_or("", |rank| reason_text(rank.reason))
                    .to_owned(),
            );
        }
        rows.push(row);
    }
    let widths: Vec<usize> = (0..header.len())
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.iter().fold(String::new(), |mut text, row| {
        let mut line = String::new();
        for (cell, width) in row.iter().zip(&widths) {
            let _ = write!(line, "{cell:<width$}  ");
        }
        text.push_str(line.trim_end());
        text.push('\n');
        text
    })
}

/// Text of `loom show`: one field per line, then the notes.
#[must_use]
pub fn render_detail(detail: &TaskDetail) -> String {
    let summary = &detail.summary;
    let mut out = String::new();
    let _ = writeln!(out, "id:         {} ({})", summary.id, summary.prefix);
    let _ = writeln!(out, "context:    {}", summary.context);
    if let Some(task) = &summary.task {
        let _ = writeln!(out, "title:      {}", task.title);
        let _ = writeln!(out, "kind:       {}", task.kind.name());
        let _ = writeln!(out, "status:     {}", status_text(&task.status));
        let _ = writeln!(out, "importance: {}", importance_text(task.importance));
        if let Some(due) = task.due {
            let _ = writeln!(out, "due:        {due}");
        }
        if task.pinned {
            let _ = writeln!(out, "pinned:     yes");
        }
        if let Some(repo) = linked_repo(task) {
            let _ = writeln!(out, "repo:       {repo}");
        }
        if let Some(parent) = &task.relations.parent {
            let _ = writeln!(out, "parent:     {parent}");
        }
        let _ = writeln!(out, "created:    {}", task.created);
        let _ = writeln!(out, "updated:    {}", task.updated);
    }
    if let Some(read_only) = &summary.read_only {
        let _ = writeln!(out, "read-only:  {}", read_only_text(read_only));
    }
    if !detail.body.trim().is_empty() {
        out.push('\n');
        out.push_str(detail.body.trim_end());
        out.push('\n');
    }
    out
}

/// One `key=value` of `loom set`, as typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// The key.
    pub key: SetKey,
    /// The value, as typed.
    pub value: String,
}

/// Keys of `loom set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SetKey {
    /// `title`.
    Title,
    /// `kind`.
    Kind,
    /// `format`, of a `content` task.
    Format,
    /// `importance`.
    Importance,
    /// `due`: a date, or `none`.
    Due,
    /// `parent`: an ID or prefix, or `none`.
    Parent,
    /// `repo`: a repository, or `none`.
    Repo,
    /// `pinned`: `yes` or `no`.
    Pinned,
    /// `status`: a status other than `waiting`.
    Status,
}

impl SetKey {
    const ALL: [(&'static str, Self); 9] = [
        ("title", Self::Title),
        ("kind", Self::Kind),
        ("format", Self::Format),
        ("importance", Self::Importance),
        ("due", Self::Due),
        ("parent", Self::Parent),
        ("repo", Self::Repo),
        ("pinned", Self::Pinned),
        ("status", Self::Status),
    ];

    /// Name of the key.
    #[must_use]
    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, key)| *key == self)
            .map_or("", |(name, _)| name)
    }
}

/// Parses one `key=value` of `loom set`; used as a `clap` value parser, so a bad one is a usage
/// error. The value is checked by [`set_plan`].
///
/// # Errors
///
/// A message naming the argument.
pub fn assignment(text: &str) -> Result<Assignment, String> {
    let names = || {
        SetKey::ALL
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let (key, value) = text
        .split_once('=')
        .ok_or_else(|| format!("{text:?} is not key=value (keys: {})", names()))?;
    let key = SetKey::ALL
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, key)| *key)
        .ok_or_else(|| format!("unknown key {key:?} (keys: {})", names()))?;
    Ok(Assignment {
        key,
        value: value.to_owned(),
    })
}

/// What `loom set` changes: the `PATCH` fields, the parent (resolved by the caller against the
/// task's relations), the repository link, and the pin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetPlan {
    /// Fields of `PATCH /v1/tasks/{id}`, the relations and links left out.
    pub edit: TaskEdit,
    /// `Some(None)`: remove the parent; `Some(Some(prefix))`: set it.
    pub parent: Option<Option<String>>,
    /// `Some(None)`: remove the repository link; `Some(Some(arg))`: set it.
    pub repo: Option<Option<String>>,
    /// `pin` or `unpin`.
    pub pinned: Option<bool>,
    /// The status to move to, as named in the path of the action.
    pub status: Option<StatusArg>,
}

/// Why `loom set` cannot apply its arguments.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SetError {
    /// A key is given twice.
    #[error("{0} is given twice")]
    Repeated(&'static str),
    /// A value is invalid for its key.
    #[error("invalid {key}: {reason}")]
    Invalid {
        /// The key.
        key: &'static str,
        /// Why.
        reason: String,
    },
    /// `format` without `kind=content`.
    #[error("format applies with kind=content only: set both")]
    FormatWithoutContent,
}

/// The changes of `loom set`, dates relative to `today`.
///
/// # Errors
///
/// [`SetError`] for a repeated key, an invalid value, or `format` without `kind=content`.
pub fn set_plan(assignments: &[Assignment], today: Date) -> Result<SetPlan, SetError> {
    let mut seen = Vec::new();
    let mut plan = SetPlan::default();
    let mut kind = None;
    let mut format = None;
    for Assignment { key, value } in assignments {
        if seen.contains(key) {
            return Err(SetError::Repeated(key.name()));
        }
        seen.push(*key);
        let invalid = |reason: String| SetError::Invalid {
            key: key.name(),
            reason,
        };
        let choice = |values: &[String]| format!("expected one of {}", values.join(", "));
        match key {
            SetKey::Title => plan.edit.title = Some(value.clone()),
            SetKey::Kind => {
                kind = Some(
                    <KindArg as clap::ValueEnum>::from_str(value, false)
                        .map_err(|_| invalid(choice(&value_names::<KindArg>())))?,
                );
            }
            SetKey::Format => {
                format = Some(
                    <FormatArg as clap::ValueEnum>::from_str(value, false)
                        .map_err(|_| invalid(choice(&value_names::<FormatArg>())))?,
                );
            }
            SetKey::Importance => {
                let importance = <ImportanceArg as clap::ValueEnum>::from_str(value, false)
                    .map_err(|_| invalid(choice(&value_names::<ImportanceArg>())))?;
                plan.edit.importance = Some(importance.into());
            }
            SetKey::Due => {
                plan.edit.due = Some(if value == "none" {
                    None
                } else {
                    Some(parse_date(value, today).map_err(|error| invalid(error.to_string()))?)
                });
            }
            SetKey::Parent => {
                plan.parent = Some(if value == "none" {
                    None
                } else {
                    Some(task_prefix(value).map_err(invalid)?)
                });
            }
            SetKey::Repo => {
                plan.repo = Some(if value.is_empty() || value == "none" {
                    None
                } else if value.trim().is_empty() || value.chars().any(char::is_control) {
                    return Err(invalid("expected a repository or none".to_owned()));
                } else {
                    Some(value.clone())
                });
            }
            SetKey::Status => {
                let status = <StatusArg as clap::ValueEnum>::from_str(value, false)
                    .map_err(|_| invalid(choice(&value_names::<StatusArg>())))?;
                if status == StatusArg::Waiting {
                    return Err(invalid(
                        "use `loom snooze` to move a task to waiting".to_owned(),
                    ));
                }
                plan.status = Some(status);
            }
            SetKey::Pinned => {
                plan.pinned = Some(match value.as_str() {
                    "yes" | "true" => true,
                    "no" | "false" => false,
                    _ => return Err(invalid(choice(&["yes".to_owned(), "no".to_owned()]))),
                });
            }
        }
    }
    match (kind, format) {
        (Some(kind), None) => plan.edit.kind = Some(kind_of(kind, None)),
        (Some(KindArg::Content), Some(format)) => {
            plan.edit.kind = Some(kind_of(KindArg::Content, Some(format)));
        }
        (None, None) => {}
        (Some(_) | None, Some(_)) => return Err(SetError::FormatWithoutContent),
    }
    Ok(plan)
}

/// The names `clap` accepts for `T`, in declaration order.
fn value_names<T: clap::ValueEnum>() -> Vec<String> {
    T::value_variants()
        .iter()
        .filter_map(clap::ValueEnum::to_possible_value)
        .map(|value| value.get_name().to_owned())
        .collect()
}

/// The relations of a task with `parent` as its parent, the others kept.
#[must_use]
pub fn with_parent(relations: &Relations, parent: Option<TaskId>) -> Relations {
    Relations {
        parent,
        ..relations.clone()
    }
}

/// The line a task gesture prints: prefix, context, status and title.
#[must_use]
pub fn render_change(detail: &TaskDetail) -> String {
    render_summary(&detail.summary)
}

fn render_summary(summary: &TaskSummary) -> String {
    summary.task.as_ref().map_or_else(
        || format!("{} {}\n", summary.prefix, summary.context),
        |task| {
            format!(
                "{} {} {} {}\n",
                summary.prefix,
                summary.context,
                status_text(&task.status),
                task.title
            )
        },
    )
}

/// Output of `loom go`: the activated task, the previous focus if one was demoted, and the
/// repository path (or a notice when the task has no local repository).
#[must_use]
pub fn render_go(result: &GoResult) -> String {
    let mut out = render_change(&result.task);
    if let Some(previous) = &result.switched_from {
        let title = previous
            .task
            .as_ref()
            .map_or("", |task| task.title.as_str());
        let _ = writeln!(out, "switched from {} ready {title}", previous.prefix);
    }
    if let Some(path) = &result.repo_path {
        let _ = writeln!(out, "{}", path.display());
    } else if let Some(slug) = result.task.summary.task.as_ref().and_then(linked_repo) {
        let _ = writeln!(
            out,
            "no local path for repository {slug}: staying in the current directory"
        );
    } else {
        let _ = writeln!(
            out,
            "no repository linked to {}: staying in the current directory",
            result.task.summary.prefix
        );
    }
    out
}

/// Output of `loom focus`: one line per context that has a focus task.
#[must_use]
pub fn render_focus(list: &FocusList) -> String {
    let mut out = String::new();
    for entry in &list.entries {
        let title = entry
            .task
            .task
            .as_ref()
            .map_or("", |task| task.title.as_str());
        match &entry.repo_path {
            Some(path) => {
                let _ = writeln!(
                    out,
                    "{}  {}  {title}  {}",
                    entry.context,
                    entry.task.prefix,
                    path.display()
                );
            }
            None => {
                let _ = writeln!(out, "{}  {}  {title}", entry.context, entry.task.prefix);
            }
        }
    }
    out
}

/// Who changed the task after the `loom edit` save, once loomd recorded it.
///
/// The actors of the events after the first hand-edit event of `new_events` (the save), in
/// order, without repeats. A later hand edit (a second save, another window) is "a hand edit".
#[must_use]
pub fn changed_after_save(new_events: &[EventRecord]) -> Vec<String> {
    let Some(save) = new_events
        .iter()
        .position(|record| record.actor == Actor::File)
    else {
        return Vec::new();
    };
    let mut actors: Vec<String> = Vec::new();
    for record in &new_events[save + 1..] {
        let name = match &record.actor {
            Actor::File => "a hand edit".to_owned(),
            Actor::User => "user".to_owned(),
            Actor::Daemon => "loomd".to_owned(),
            Actor::Agent { name } => format!("agent {name}"),
        };
        if !actors.contains(&name) {
            actors.push(name);
        }
    }
    actors
}

/// Whether the index holds the file `loom edit` saved: the same task, `updated` aside (it comes
/// from the log, not from the file), and the same notes.
///
/// loomd takes the file as the truth, so the index ends with exactly this, whatever else
/// changed the task meanwhile. Before that, the index may already list the hand-edit event
/// but not its result, or show a change made by someone else.
///
/// Exact because the index builds its task from the file with the same parser and
/// `TaskDocument::task`: the two sides differ only in `updated`. A field the index would take
/// from the log or the configuration instead of the file breaks this comparison.
#[must_use]
pub fn holds_saved(indexed: &TaskDetail, saved: &Task, saved_notes: &str) -> bool {
    indexed.body == saved_notes
        && indexed.summary.task.as_ref().is_some_and(|task| {
            Task {
                updated: saved.updated,
                ..task.clone()
            } == *saved
        })
}

/// Who changed the task while it was open in the editor: the actors of the events appended
/// since then other than the hand edit itself, in order, without repeats. Empty when nothing
/// else changed it.
#[must_use]
pub fn changed_meanwhile(new_events: &[EventRecord]) -> Vec<String> {
    let mut actors: Vec<String> = Vec::new();
    for record in new_events {
        let name = match &record.actor {
            Actor::File => continue,
            Actor::User => "user".to_owned(),
            Actor::Daemon => "loomd".to_owned(),
            Actor::Agent { name } => format!("agent {name}"),
        };
        if !actors.contains(&name) {
            actors.push(name);
        }
    }
    actors
}

/// Warning of `loom show` for the lines of `events.jsonl` that loom does not replay; `None` when
/// there are none. A line written by a newer loom stops the recording of hand edits.
#[must_use]
pub fn skipped_notice(skipped: &[SkippedLine]) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let noun = if skipped.len() == 1 { "line" } else { "lines" };
    let mut out = format!(
        "events.jsonl has {} {noun} that loom does not replay (kept as written):\n",
        skipped.len()
    );
    for line in skipped {
        let _ = writeln!(out, "  line {}: {}", line.line, line.reason);
    }
    if skipped
        .iter()
        .any(|line| matches!(line.reason, SkipReason::NewerVersion { .. }))
    {
        out.push_str("Hand edits of this task are not recorded until loom is upgraded.\n");
    }
    Some(out)
}

/// The editor of `loom edit`: `VISUAL`, then `EDITOR`, then `vi`; an empty value is unset.
#[must_use]
pub fn editor(visual: Option<&str>, editor: Option<&str>) -> String {
    visual
        .filter(|value| !value.trim().is_empty())
        .or_else(|| editor.filter(|value| !value.trim().is_empty()))
        .unwrap_or("vi")
        .to_owned()
}

/// Writes `repo_path` and a newline to `cd_file`, for the `loom` shell function to change directory.
/// Does nothing when `cd_file` is not set or empty, or when there is no path.
///
/// # Errors
///
/// Returns the error of the write.
pub fn write_cd_file(
    cd_file: Option<&std::path::Path>,
    repo_path: Option<&std::path::Path>,
) -> std::io::Result<()> {
    match (cd_file, repo_path) {
        (Some(cd_file), Some(repo_path)) if !cd_file.as_os_str().is_empty() => {
            std::fs::write(cd_file, format!("{}\n", repo_path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_api::{TaskRank, Timestamp};

    fn name(text: &str) -> ContextName {
        text.parse().unwrap()
    }

    fn date(text: &str) -> Date {
        text.parse().unwrap()
    }

    #[test]
    fn the_repository_path_is_written_for_the_shell_function() {
        let dir = tempfile::tempdir().unwrap();
        let cd_file = dir.path().join("cd");
        let repo = std::path::Path::new("/work/repo");

        write_cd_file(Some(&cd_file), Some(repo)).unwrap();
        assert_eq!(std::fs::read_to_string(&cd_file).unwrap(), "/work/repo\n");

        let untouched = dir.path().join("untouched");
        write_cd_file(Some(&untouched), None).unwrap();
        write_cd_file(None, Some(repo)).unwrap();
        write_cd_file(Some(std::path::Path::new("")), Some(repo)).unwrap();
        assert!(!untouched.exists());
        assert!(write_cd_file(Some(&dir.path().join("missing/cd")), Some(repo)).is_err());
    }

    #[test]
    fn the_flag_wins_over_every_other_rule() {
        let repo = ContextAssignment::Override {
            context: name("work"),
        };
        let resolved = resolve_context(Some(&name("home")), Some("lab"), Some(&repo), None);
        assert_eq!(
            resolved.unwrap(),
            ResolvedContext {
                context: name("home"),
                source: ContextSource::Flag
            }
        );
    }

    #[test]
    fn the_environment_comes_second_and_must_be_a_name() {
        let repo = ContextAssignment::Override {
            context: name("work"),
        };
        assert_eq!(
            resolve_context(None, Some("lab"), Some(&repo), None).unwrap(),
            ResolvedContext {
                context: name("lab"),
                source: ContextSource::Env
            }
        );
        // Empty: unset.
        assert_eq!(
            resolve_context(None, Some(""), Some(&repo), None)
                .unwrap()
                .source,
            ContextSource::Repo
        );
        assert!(matches!(
            resolve_context(None, Some("Not A Name"), Some(&repo), None),
            Err(ContextError::InvalidEnv { .. })
        ));
    }

    #[test]
    fn the_repository_of_the_current_directory_comes_third() {
        let host = ContextAssignment::Host {
            context: name("work"),
            pattern: "gitlab.example.com/company-a".to_owned(),
        };
        let path = ContextAssignment::Path {
            context: name("home"),
            prefix: "~/perso".to_owned(),
        };
        assert_eq!(
            resolve_context(None, None, Some(&host), Some(&path)).unwrap(),
            ResolvedContext {
                context: name("work"),
                source: ContextSource::Repo
            }
        );
        // An unassigned repository gives nothing: the directory rules decide.
        assert_eq!(
            resolve_context(
                None,
                None,
                Some(&ContextAssignment::Unassigned),
                Some(&path)
            )
            .unwrap()
            .source,
            ContextSource::Path
        );
    }

    #[test]
    fn match_paths_then_default_context_come_last() {
        let path = ContextAssignment::Path {
            context: name("home"),
            prefix: "~/perso".to_owned(),
        };
        assert_eq!(
            resolve_context(None, None, None, Some(&path)).unwrap(),
            ResolvedContext {
                context: name("home"),
                source: ContextSource::Path
            }
        );
        let default = ContextAssignment::Default {
            context: name("personal"),
        };
        assert_eq!(
            resolve_context(None, None, None, Some(&default)).unwrap(),
            ResolvedContext {
                context: name("personal"),
                source: ContextSource::Default
            }
        );
        assert_eq!(
            resolve_context(None, None, None, Some(&ContextAssignment::Unassigned)),
            Err(ContextError::Missing)
        );
        assert_eq!(
            resolve_context(None, None, None, None),
            Err(ContextError::Missing)
        );
    }

    #[test]
    fn dates_are_absolute_or_relative_to_today() {
        let today = date("2026-10-14");
        assert_eq!(parse_date("2026-11-02", today), Ok(date("2026-11-02")));
        assert_eq!(parse_date("today", today), Ok(today));
        assert_eq!(parse_date("tomorrow", today), Ok(date("2026-10-15")));
        assert_eq!(parse_date("+3d", today), Ok(date("2026-10-17")));
        assert_eq!(parse_date("+2w", today), Ok(date("2026-10-28")));
        for bad in [
            "",
            "+",
            "+d",
            "+3m",
            "next week",
            "2026-13-01",
            "+-x",
            "++3d",
            "+-3d",
            "+é",
            "+3€",
            "+99999999999999999999d",
        ] {
            assert_eq!(
                parse_date(bad, today),
                Err(DateError(bad.to_owned())),
                "{bad}"
            );
        }
    }

    #[test]
    fn task_prefixes_are_crockford_base32() {
        for good in ["k3q7", "K3Q7ZM2A", "1", "il0o"] {
            assert_eq!(task_prefix(good), Ok(good.to_owned()), "{good}");
        }
        for bad in ["", "k3q7zm2a9", "a b", "a/b", "abc?x", "u", "k3é", "-1"] {
            assert!(task_prefix(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_task_takes_the_context_of_its_parent_unless_another_is_chosen() {
        let parent: TaskId = "k3q7zm2a".parse().unwrap();
        let work = name("work");
        let resolved = |context: &str, source| {
            Ok(ResolvedContext {
                context: name(context),
                source,
            })
        };
        // Inferred from the current directory, or missing: the parent's.
        for source in [
            ContextSource::Repo,
            ContextSource::Path,
            ContextSource::Default,
        ] {
            assert_eq!(
                context_with_parent(resolved("personal", source), &parent, &work),
                Ok(work.clone())
            );
        }
        assert_eq!(
            context_with_parent(Err(ContextError::Missing), &parent, &work),
            Ok(work.clone())
        );
        // Chosen explicitly: it must be the parent's.
        for source in [ContextSource::Flag, ContextSource::Env] {
            assert_eq!(
                context_with_parent(resolved("work", source), &parent, &work),
                Ok(work.clone())
            );
            assert!(matches!(
                context_with_parent(resolved("personal", source), &parent, &work),
                Err(ParentContextError::Mismatch { .. })
            ));
        }
        let invalid = ContextError::InvalidEnv {
            value: "X".to_owned(),
            reason: "bad".to_owned(),
        };
        assert_eq!(
            context_with_parent(Err(invalid.clone()), &parent, &work),
            Err(ParentContextError::Context(invalid))
        );
    }

    #[test]
    fn added_tasks_go_to_the_inbox_unless_ready() {
        let request = AddRequest {
            title: "Renew passport".to_owned(),
            context: name("personal"),
            kind: kind_of(KindArg::Admin, None),
            importance: None,
            due: Some(date("2026-11-02")),
            parent: Some("k3q7zm2a".parse().unwrap()),
            repo: Some("personal/dotfiles".to_owned()),
            ready: false,
        };
        let new = new_task(request.clone());
        assert_eq!(new.status, Status::Inbox);
        assert_eq!(new.kind, Kind::Admin);
        assert_eq!(new.relations.parent, request.parent);
        assert_eq!(
            new.links,
            [Link::Repo {
                repo: "personal/dotfiles".to_owned()
            }]
        );
        assert_eq!(
            new_task(AddRequest {
                ready: true,
                repo: None,
                ..request
            })
            .status,
            Status::Ready
        );
        assert_eq!(
            kind_of(KindArg::Content, Some(FormatArg::Talk)),
            Kind::Content {
                format: Some(ContentFormat::Talk)
            }
        );
    }

    #[test]
    fn ls_asks_for_the_priority_list_unless_a_status_is_given() {
        assert_eq!(
            list_api_path(Some(&name("work")), None, false),
            "/v1/tasks?context=work&ranked=true"
        );
        assert_eq!(list_api_path(None, None, false), "/v1/tasks?ranked=true");
        assert_eq!(
            list_api_path(None, Some("inbox"), false),
            "/v1/tasks?status=inbox"
        );
    }

    #[test]
    fn ls_archived_asks_for_the_archived_tasks_by_id() {
        assert_eq!(
            list_api_path(Some(&name("work")), None, true),
            "/v1/tasks?context=work&archived=true"
        );
        assert_eq!(
            list_api_path(None, Some("cancelled"), true),
            "/v1/tasks?status=cancelled&archived=true"
        );
    }

    fn summary(id: &str, title: &str, rank: Option<(usize, RankReason)>) -> TaskSummary {
        let created = Timestamp::from_unix_secs(1_791_792_000).unwrap();
        TaskSummary {
            id: id.parse().unwrap(),
            prefix: id[..4].to_owned(),
            context: name("work"),
            task: Some(Task {
                id: id.parse().unwrap(),
                title: title.to_owned(),
                kind: Kind::Bug,
                context: name("work"),
                status: Status::Active,
                importance: Importance::P1,
                due: Some(date("2026-10-15")),
                pinned: false,
                links: Vec::new(),
                relations: Relations::default(),
                created,
                updated: created,
            }),
            read_only: None,
            rank: rank.map(|(position, reason)| TaskRank { position, reason }),
        }
    }

    #[test]
    fn ls_why_prints_the_rule_of_each_line() {
        let tasks = [
            summary("k3q7zm2a", "Login timeout", Some((1, RankReason::First))),
            summary(
                "h8w2aaaa",
                "Checkout 5xx",
                Some((2, RankReason::Importance)),
            ),
        ];
        assert_eq!(
            render_list(&tasks, true),
            "#  ID    CONTEXT  STATUS  P   KIND  DUE         TITLE          WHY\n\
             1  k3q7  work     active  p1  bug   2026-10-15  Login timeout  first\n\
             2  h8w2  work     active  p1  bug   2026-10-15  Checkout 5xx   more important\n"
        );
        assert!(!render_list(&tasks, false).contains("WHY"));
        assert_eq!(render_list(&[], true), "");
    }

    #[test]
    fn an_unranked_list_has_no_position_and_shows_invalid_files() {
        let mut invalid = summary("zzzz0000", "", None);
        invalid.task = None;
        invalid.read_only = Some(TaskReadOnly::InvalidFile("line 2: x".to_owned()));
        let text = render_list(
            &[summary("k3q7zm2a", "Login timeout", None), invalid],
            false,
        );
        assert!(text.starts_with("ID    CONTEXT"), "{text}");
        assert!(
            text.ends_with(
                "zzzz  work                                   invalid task.md: line 2: x\n"
            ),
            "{text:?}"
        );
    }

    #[test]
    fn skipped_log_lines_are_listed_and_a_newer_line_says_what_it_costs() {
        assert_eq!(skipped_notice(&[]), None);
        let damaged = SkippedLine {
            line: 3,
            raw: "{\"garbage\"".to_owned(),
            reason: SkipReason::Invalid {
                message: "EOF while parsing".to_owned(),
            },
        };
        assert_eq!(
            skipped_notice(std::slice::from_ref(&damaged)).as_deref(),
            Some(
                "events.jsonl has 1 line that loom does not replay (kept as written):\n  \
                 line 3: invalid event: EOF while parsing\n"
            )
        );
        let newer = SkippedLine {
            line: 5,
            raw: "{}".to_owned(),
            reason: SkipReason::NewerVersion { v: 9 },
        };
        let text = skipped_notice(&[damaged, newer]).unwrap();
        assert!(text.starts_with("events.jsonl has 2 lines that loom does not replay"));
        assert!(
            text.contains("  line 5: schema version 9, newer than"),
            "{text}"
        );
        assert!(
            text.ends_with("Hand edits of this task are not recorded until loom is upgraded.\n"),
            "{text}"
        );
    }

    #[test]
    fn show_prints_the_fields_then_the_notes() {
        let detail = TaskDetail {
            summary: summary("k3q7zm2a", "Login timeout", None),
            body: "## Notes\n\nRepro on iOS.\n".to_owned(),
            actions: Vec::new(),
        };
        let text = render_detail(&detail);
        assert!(text.starts_with("id:         k3q7zm2a (k3q7)\ncontext:    work\ntitle:      Login timeout\nkind:       bug\nstatus:     active\nimportance: p1\ndue:        2026-10-15\n"), "{text}");
        assert!(text.ends_with("\n\n## Notes\n\nRepro on iOS.\n"), "{text}");
    }

    fn plan(args: &[&str]) -> Result<SetPlan, SetError> {
        let assignments: Vec<Assignment> =
            args.iter().map(|arg| assignment(arg).unwrap()).collect();
        set_plan(&assignments, date("2026-10-14"))
    }

    #[test]
    fn set_reads_key_value_pairs() {
        assert!(assignment("title").is_err());
        assert!(
            assignment("colour=red")
                .unwrap_err()
                .contains("unknown key")
        );
        let set = plan(&[
            "title=Login timeout = 30 s",
            "importance=p0",
            "due=+3d",
            "parent=k3q7",
            "repo=work/checkout",
            "pinned=yes",
            "kind=content",
            "format=talk",
        ])
        .unwrap();
        assert_eq!(set.edit.title.as_deref(), Some("Login timeout = 30 s"));
        assert_eq!(set.edit.importance, Some(Importance::P0));
        assert_eq!(set.edit.due, Some(Some(date("2026-10-17"))));
        assert_eq!(
            set.edit.kind,
            Some(Kind::Content {
                format: Some(ContentFormat::Talk)
            })
        );
        assert_eq!(set.parent, Some(Some("k3q7".to_owned())));
        assert_eq!(set.repo, Some(Some("work/checkout".to_owned())));
        assert_eq!(set.pinned, Some(true));
        assert_eq!(set.edit.relations, None);

        let cleared = plan(&["due=none", "parent=none", "repo=none", "pinned=no"]).unwrap();
        assert_eq!(cleared.edit.due, Some(None));
        assert_eq!(cleared.parent, Some(None));
        assert_eq!(cleared.repo, Some(None));
        assert_eq!(cleared.pinned, Some(false));
        assert_eq!(plan(&["repo="]).unwrap().repo, Some(None));
        assert_eq!(
            plan(&["status=done"]).unwrap().status,
            Some(StatusArg::Done)
        );
    }

    #[test]
    fn set_sends_waiting_to_snooze() {
        let Err(SetError::Invalid { key, reason }) = plan(&["status=waiting"]) else {
            panic!("waiting must be refused");
        };
        assert_eq!(key, "status");
        assert!(reason.contains("loom snooze"), "{reason}");
        assert!(matches!(
            plan(&["status=later"]),
            Err(SetError::Invalid { .. })
        ));
    }

    #[test]
    fn set_refuses_repeats_bad_values_and_a_lone_format() {
        assert_eq!(
            plan(&["due=today", "due=none"]),
            Err(SetError::Repeated("due"))
        );
        for bad in [
            "importance=p9",
            "due=soon",
            "parent=a/b",
            "repo=   ",
            "pinned=maybe",
            "kind=epic",
        ] {
            assert!(
                matches!(plan(&[bad]), Err(SetError::Invalid { .. })),
                "{bad}"
            );
        }
        assert_eq!(plan(&["format=talk"]), Err(SetError::FormatWithoutContent));
        assert_eq!(
            plan(&["kind=bug", "format=talk"]),
            Err(SetError::FormatWithoutContent)
        );
    }

    #[test]
    fn a_new_parent_keeps_the_other_relations() {
        let blocked: TaskId = "bbbbbbbb".parse().unwrap();
        let relations = Relations {
            parent: None,
            spawned_from: Vec::new(),
            blocks: vec![blocked.clone()],
        };
        let parent: TaskId = "k3q7zm2a".parse().unwrap();
        let moved = with_parent(&relations, Some(parent.clone()));
        assert_eq!(moved.parent, Some(parent));
        assert_eq!(moved.blocks, [blocked]);
    }

    #[test]
    fn a_new_repo_keeps_the_other_links() {
        let origin = Link::Origin(loom_api::ExternalRef {
            provider: "jira".to_owned(),
            key: "CORE-1042".to_owned(),
            url: None,
        });
        let links = vec![
            origin.clone(),
            Link::Repo {
                repo: "work/old".to_owned(),
            },
        ];
        assert_eq!(
            with_repo(&links, Some("work/new".to_owned())),
            [
                origin.clone(),
                Link::Repo {
                    repo: "work/new".to_owned()
                }
            ]
        );
        assert_eq!(with_repo(&links, None), [origin]);
    }

    #[test]
    fn go_and_focus_render_human_output() {
        let since = Timestamp::from_unix_secs(1_791_792_000).unwrap();
        let detail = TaskDetail {
            summary: summary("k3q7zm2a", "Login timeout", None),
            body: String::new(),
            actions: Vec::new(),
        };
        let prev = {
            let mut prev = summary("h8w2aaaa", "Checkout 5xx", None);
            prev.task.as_mut().unwrap().status = Status::Ready;
            prev
        };
        let with_repo = GoResult {
            task: detail.clone(),
            switched_from: Some(prev),
            since,
            repo_path: Some("/home/me/work/checkout".into()),
        };
        assert_eq!(
            render_go(&with_repo),
            "k3q7 work active Login timeout\nswitched from h8w2 ready Checkout 5xx\n/home/me/work/checkout\n"
        );
        let without_repo = GoResult {
            task: detail,
            switched_from: None,
            since,
            repo_path: None,
        };
        assert_eq!(
            render_go(&without_repo),
            "k3q7 work active Login timeout\nno repository linked to k3q7: staying in the current directory\n"
        );
        let focus = FocusList {
            entries: vec![loom_api::FocusEntry {
                context: name("work"),
                task: summary("k3q7zm2a", "Login timeout", None),
                since,
                repo_path: Some("/home/me/work/checkout".into()),
            }],
        };
        assert_eq!(
            render_focus(&focus),
            "work  k3q7  Login timeout  /home/me/work/checkout\n"
        );
        assert_eq!(render_focus(&FocusList { entries: vec![] }), "");
        assert_eq!(focus_api_path(None), "/v1/focus");
        assert_eq!(
            focus_api_path(Some(&name("work"))),
            "/v1/focus?context=work"
        );
    }

    #[test]
    fn the_editor_is_visual_then_editor_then_vi() {
        assert_eq!(editor(Some("code --wait"), Some("nano")), "code --wait");
        assert_eq!(editor(Some(" "), Some("nano")), "nano");
        assert_eq!(editor(None, None), "vi");
    }

    #[test]
    fn a_gesture_prints_one_line() {
        let detail = TaskDetail {
            summary: summary("k3q7zm2a", "Login timeout", None),
            body: String::new(),
            actions: Vec::new(),
        };
        assert_eq!(render_change(&detail), "k3q7 work active Login timeout\n");
    }

    fn record(actor: Actor) -> EventRecord {
        let origin = loom_core::Origin {
            actor,
            machine_id: loom_api::MachineId("desk".to_owned()),
        };
        EventRecord::new(
            "aaaaaaaa".parse().unwrap(),
            loom_api::Event::NoteLogged {
                text: "x".to_owned(),
            },
            Timestamp::from_unix_secs(1_791_792_000).unwrap(),
            &origin,
        )
    }

    #[test]
    fn only_the_changes_after_the_save_are_named() {
        // A user change while the editor was open, the save, then loomd and a second save.
        let events = [
            record(Actor::User),
            record(Actor::File),
            record(Actor::Daemon),
            record(Actor::File),
        ];
        assert_eq!(changed_after_save(&events), ["loomd", "a hand edit"]);
        assert_eq!(
            changed_after_save(&[record(Actor::User), record(Actor::File)]),
            Vec::<String>::new()
        );
        assert_eq!(
            changed_after_save(&[record(Actor::User)]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_index_holds_the_save_once_task_and_notes_match() {
        let indexed = TaskDetail {
            summary: summary("k3q7zm2a", "Edited", None),
            body: "## Notes\n".to_owned(),
            actions: Vec::new(),
        };
        let mut saved = indexed.summary.task.clone().unwrap();
        // `updated` comes from the log, never from the file.
        saved.updated = Timestamp::from_unix_secs(1).unwrap();
        assert!(holds_saved(&indexed, &saved, "## Notes\n"));
        assert!(!holds_saved(&indexed, &saved, "## Notes\nmore\n"));
        let mut older = saved.clone();
        older.title = "Original".to_owned();
        assert!(!holds_saved(&indexed, &older, "## Notes\n"));
        let mut invalid = indexed;
        invalid.summary.task = None;
        assert!(!holds_saved(&invalid, &saved, "## Notes\n"));
    }

    #[test]
    fn changes_by_others_while_editing_are_named() {
        assert_eq!(
            changed_meanwhile(&[record(Actor::File)]),
            Vec::<String>::new()
        );
        assert_eq!(
            changed_meanwhile(&[
                record(Actor::User),
                record(Actor::File),
                record(Actor::User),
                record(Actor::Agent {
                    name: "reviewer".to_owned()
                }),
            ]),
            ["user", "agent reviewer"]
        );
    }

    #[test]
    fn value_names_follow_the_cli() {
        assert_eq!(value_names::<ImportanceArg>(), ["p0", "p1", "p2", "p3"]);
        assert!(value_names::<KindArg>().contains(&"investigation".to_owned()));
    }
}
