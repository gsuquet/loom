//! `loom add|ls|show`: the context a task goes to, the request, and the output.
//!
//! No I/O: `task_cmd` reads the environment, the configuration and the index, and calls loomd.

use std::fmt::Write as _;
use std::time::Duration;

use loom_api::{
    ContentFormat, ContextAssignment, ContextName, Date, Importance, Kind, NewTask, RankReason,
    Relations, Status, TaskDetail, TaskId, TaskReadOnly, TaskSummary,
};
use thiserror::Error;

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

/// Seconds in a civil day: a civil date moves by whole days.
const SECONDS_PER_DAY: u64 = 86_400;

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
    let days = match text {
        "today" => 0,
        "tomorrow" => 1,
        _ => match text.strip_prefix('+') {
            Some(offset) => {
                let (count, unit) = offset.split_at(offset.len().saturating_sub(1));
                let count: u64 = count.parse().map_err(|_| invalid())?;
                match unit {
                    "d" => count,
                    "w" => count.checked_mul(7).ok_or_else(invalid)?,
                    _ => return Err(invalid()),
                }
            }
            None => return text.parse::<Date>().map_err(|_| invalid()),
        },
    };
    let span = days
        .checked_mul(SECONDS_PER_DAY)
        .map(Duration::from_secs)
        .ok_or_else(invalid)?;
    today.checked_add(span).map_err(|_| invalid())
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

/// What `loom add` asks for, once the context, the parent and the dates are resolved.
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
    /// `--ready`: skip the inbox.
    pub ready: bool,
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
        links: Vec::new(),
        relations: Relations {
            parent: request.parent,
            ..Relations::default()
        },
    }
}

/// `GET /v1/tasks` query of `loom ls`: the priority list, or every task with `status`; one
/// context, or all of them.
#[must_use]
pub fn list_api_path(context: Option<&ContextName>, status: Option<&str>) -> String {
    let mut query = Vec::new();
    if let Some(context) = context {
        query.push(format!("context={context}"));
    }
    match status {
        Some(status) => query.push(format!("status={status}")),
        None => query.push("ranked=true".to_owned()),
    }
    format!("/v1/tasks?{}", query.join("&"))
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

fn read_only_text(read_only: &TaskReadOnly) -> String {
    match read_only {
        TaskReadOnly::UnknownKind(value) => format!("unknown kind {value}"),
        TaskReadOnly::UnknownContentFormat(value) => format!("unknown content format {value}"),
        TaskReadOnly::UnknownStatus(value) => format!("unknown status {value}"),
        TaskReadOnly::InvalidFile(error) => format!("invalid task.md: {error}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use loom_api::{Task, TaskRank, Timestamp};

    fn name(text: &str) -> ContextName {
        text.parse().unwrap()
    }

    fn date(text: &str) -> Date {
        text.parse().unwrap()
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
        for bad in ["", "+", "+d", "+3m", "next week", "2026-13-01", "+-x"] {
            assert_eq!(
                parse_date(bad, today),
                Err(DateError(bad.to_owned())),
                "{bad}"
            );
        }
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
            ready: false,
        };
        let new = new_task(request.clone());
        assert_eq!(new.status, Status::Inbox);
        assert_eq!(new.kind, Kind::Admin);
        assert_eq!(new.relations.parent, request.parent);
        assert_eq!(
            new_task(AddRequest {
                ready: true,
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
            list_api_path(Some(&name("work")), None),
            "/v1/tasks?context=work&ranked=true"
        );
        assert_eq!(list_api_path(None, None), "/v1/tasks?ranked=true");
        assert_eq!(list_api_path(None, Some("inbox")), "/v1/tasks?status=inbox");
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
}
