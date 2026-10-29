//! `task.md`: a TOML frontmatter between `+++` lines, then free Markdown notes.
//!
//! The frontmatter is kept as a [`DocumentMut`]: an update rewrites only the keys whose value
//! changed, so comments, key order and keys this version does not know survive (ADR-0013).

use std::ops::Range;

use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use toml_edit::{
    Array, ArrayOfTables, Datetime, Document, DocumentMut, Item, Table, TableLike, Value,
};

use crate::Date;
use crate::id::TaskId;
use crate::task::{
    ContentFormat, ExternalRef, Importance, Kind, Link, ReadOnlyReason, Relations, Status, Task,
};
use crate::timestamp::Timestamp;

/// The line that opens and closes the frontmatter.
const DELIMITER: &str = "+++";

/// Root keys this version reads and writes.
const ROOT_KEYS: [&str; 14] = [
    "id",
    "title",
    "kind",
    "format",
    "context",
    "status",
    "waiting_on",
    "waiting_until",
    "importance",
    "due",
    "pinned",
    "created",
    "links",
    "relations",
];

/// Keys of `[relations]` this version reads and writes.
const RELATION_KEYS: [&str; 3] = ["parent", "spawned_from", "blocks"];

/// Link roles this version knows; a link of another role is kept as is and not decoded.
const LINK_ROLES: [&str; 4] = ["origin", "related", "delivery", "repo"];

/// A `task.md` file: the frontmatter as an editable TOML document, the task it describes and the
/// free notes that follow.
///
/// Built by [`parse_task_md`] or [`TaskDocument::new`], written back by [`render_task_md`].
#[derive(Debug, Clone)]
pub struct TaskDocument {
    /// The opening `+++` line, with its line ending.
    open: String,
    /// The frontmatter, edited key by key.
    frontmatter: DocumentMut,
    /// The frontmatter as read, until a key changes: an unchanged file renders byte for byte.
    unchanged: Option<String>,
    /// Whether the file uses CRLF line endings.
    crlf: bool,
    /// The closing `+++` line, with its line ending if it has one.
    close: String,
    /// The empty line between the closing `+++` and the notes, if any.
    separator: String,
    /// Free Markdown notes.
    body: String,
    /// The task the frontmatter describes; `updated` is not stored and set to `created`.
    task: Task,
    /// Keys and links this version does not know, kept on rewrite.
    unknown: Vec<String>,
}

/// Why a `task.md` file cannot be read. Lines start at 1.
///
/// The file itself is never rewritten while it is invalid; the notes after the frontmatter are
/// returned by [`FormatError::body`] whenever they can be located.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    /// The first line is not `+++`: the whole file is taken as notes.
    #[error("line 1: a task file starts with a `+++` line")]
    MissingFrontmatter {
        /// The whole file.
        body: String,
    },
    /// No `+++` line closes the frontmatter.
    #[error("line {line}: no `+++` line closes the frontmatter opened on line 1")]
    UnclosedFrontmatter {
        /// The last line of the file.
        line: usize,
    },
    /// The frontmatter is not valid TOML.
    #[error("line {line}: invalid TOML: {message}")]
    InvalidToml {
        /// Line of the error, in the file.
        line: usize,
        /// What is wrong.
        message: String,
        /// The notes after the frontmatter.
        body: String,
    },
    /// A key is missing or has a value of the wrong type or form.
    #[error("line {line}: `{key}`: {message}")]
    InvalidField {
        /// Line of the key, or of the closing `+++` for a missing key.
        line: usize,
        /// The key.
        key: String,
        /// What is wrong.
        message: String,
        /// The notes after the frontmatter.
        body: String,
    },
}

impl FormatError {
    /// Line of the error in the file, starting at 1.
    #[must_use]
    pub const fn line(&self) -> usize {
        match self {
            Self::MissingFrontmatter { .. } => 1,
            Self::UnclosedFrontmatter { line }
            | Self::InvalidToml { line, .. }
            | Self::InvalidField { line, .. } => *line,
        }
    }

    /// The notes of the file, when they could be located despite the error.
    #[must_use]
    pub fn body(&self) -> Option<&str> {
        match self {
            Self::MissingFrontmatter { body }
            | Self::InvalidToml { body, .. }
            | Self::InvalidField { body, .. } => Some(body),
            Self::UnclosedFrontmatter { .. } => None,
        }
    }
}

/// Reads a `task.md` file.
///
/// # Errors
///
/// [`FormatError`] when the frontmatter is missing, unclosed, not valid TOML, or when a key is
/// missing or invalid. Unknown keys, link roles, kinds and statuses are not errors.
pub fn parse_task_md(text: &str) -> Result<TaskDocument, FormatError> {
    let parts = split(text)?;
    let document =
        Document::parse(parts.frontmatter).map_err(|error| FormatError::InvalidToml {
            line: parts.line_at(error.span().map_or(0, |span| span.start)),
            message: error.message().trim_end().to_owned(),
            body: parts.body.to_owned(),
        })?;
    let (task, unknown) = decode(document.as_table()).map_err(|error| {
        let line = error
            .span
            .map_or(parts.close_line, |span| parts.line_at(span.start));
        FormatError::InvalidField {
            line,
            key: error.key,
            message: error.message,
            body: parts.body.to_owned(),
        }
    })?;
    Ok(TaskDocument {
        open: parts.open.to_owned(),
        frontmatter: document.into_mut(),
        unchanged: Some(parts.frontmatter.to_owned()),
        crlf: parts.open.ends_with("\r\n"),
        close: parts.close.to_owned(),
        separator: parts.separator.to_owned(),
        body: parts.body.to_owned(),
        task,
        unknown,
    })
}

/// Writes a `task.md` file: byte for byte what was read, except the keys that changed.
#[must_use]
pub fn render_task_md(document: &TaskDocument) -> String {
    let frontmatter = document.unchanged.clone().unwrap_or_else(|| {
        let text = document.frontmatter.to_string();
        if document.crlf {
            text.replace("\r\n", "\n").replace('\n', "\r\n")
        } else {
            text
        }
    });
    [
        document.open.as_str(),
        &frontmatter,
        &document.close,
        &document.separator,
        &document.body,
    ]
    .concat()
}

/// The sections a new task of `kind` starts its notes with, from its done condition.
#[must_use]
pub const fn body_template(kind: &Kind) -> &'static str {
    match kind {
        Kind::Story => "## Context\n\n## Acceptance\n\n## Notes\n",
        Kind::Bug => "## Symptoms\n\n## Cause\n\n## Fix\n\n## Verification\n",
        Kind::Chore | Kind::Admin => "## Notes\n",
        Kind::Research => "## Question\n\n## Findings\n\n## Decision\n",
        Kind::Investigation => "## Signal\n\n## Cause\n\n## Disposition\n",
        Kind::Review => "## Scope\n\n## Verdict\n",
        Kind::Support => "## Request\n\n## Answer\n",
        Kind::Content { .. } => "## Audience\n\n## Outline\n\n## Delivery\n",
        Kind::Interview => "## Notes\n\n## Feedback\n",
        Kind::Learning => "## Goal\n\n## Progress\n\n## Completion\n",
        Kind::Unknown { .. } => "",
    }
}

impl TaskDocument {
    /// A new file for `task`, its notes started with the [`body_template`] of its kind.
    ///
    /// `task.updated` is not written: it is the time of the last event of the task.
    #[must_use]
    pub fn new(task: &Task) -> Self {
        let mut frontmatter = DocumentMut::new();
        let root = frontmatter.as_table_mut();
        root.insert("id", Item::Value(task.id.as_str().into()));
        root.insert("title", Item::Value(task.title.as_str().into()));
        let (kind, format) = kind_values(&task.kind);
        root.insert("kind", Item::Value(kind));
        if let Some(format) = format {
            root.insert("format", Item::Value(format));
        }
        root.insert("context", Item::Value(task.context.as_str().into()));
        let (status, waiting_on, waiting_until) = status_values(&task.status);
        root.insert("status", Item::Value(status));
        for (key, value) in [("waiting_on", waiting_on), ("waiting_until", waiting_until)] {
            if let Some(value) = value {
                root.insert(key, Item::Value(value));
            }
        }
        root.insert("importance", Item::Value(importance_value(task.importance)));
        if let Some(due) = task.due {
            root.insert("due", Item::Value(date_value(due)));
        }
        if task.pinned {
            root.insert("pinned", Item::Value(true.into()));
        }
        root.insert("created", Item::Value(timestamp_value(task.created)));
        if !task.links.is_empty() {
            root.insert("links", Item::ArrayOfTables(links_tables(&task.links, [])));
        }
        if task.relations != Relations::default() {
            let mut relations = Table::new();
            relations.decor_mut().set_prefix("\n");
            write_relations(&mut relations, &task.relations);
            root.insert("relations", Item::Table(relations));
        }
        Self {
            open: format!("{DELIMITER}\n"),
            frontmatter,
            unchanged: None,
            crlf: false,
            close: format!("{DELIMITER}\n"),
            separator: "\n".to_owned(),
            body: body_template(&task.kind).to_owned(),
            task: Task {
                updated: task.created,
                ..task.clone()
            },
            unknown: Vec::new(),
        }
    }

    /// The task the frontmatter describes, last updated at `updated`.
    ///
    /// `updated` is not stored in `task.md`: it changes with every event and would make every
    /// edit a conflict between machines. Callers take it from the last event of the task.
    #[must_use]
    pub fn task(&self, updated: Timestamp) -> Task {
        Task {
            updated,
            ..self.task.clone()
        }
    }

    /// The free notes after the frontmatter.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Replaces the notes.
    pub fn set_body(&mut self, body: impl Into<String>) {
        self.body = body.into();
        if self.separator.is_empty() && !self.body.is_empty() {
            if self.crlf { "\r\n" } else { "\n" }.clone_into(&mut self.separator);
            if !self.close.ends_with('\n') {
                self.close.push_str(&self.separator);
            }
        }
    }

    /// Keys and links of the frontmatter this version does not know, such as `estimate`,
    /// `relations.duplicates` or `links[1]` (a link of an unknown role). They are kept on rewrite.
    #[must_use]
    pub fn unknown_keys(&self) -> &[String] {
        &self.unknown
    }

    /// Writes the fields of `task` that differ from the current ones, and only those keys.
    ///
    /// `task.updated` is ignored: it is not stored in `task.md`.
    ///
    /// # Errors
    ///
    /// The [`ReadOnlyReason`] of the current task when its kind, content format or status was
    /// written by a newer version of loom: such a file is never rewritten.
    pub fn set_task(&mut self, task: &Task) -> Result<(), ReadOnlyReason> {
        if let Some(reason) = self.task.read_only_reason() {
            return Err(reason);
        }
        let task = Task {
            updated: task.created,
            ..task.clone()
        };
        if task == self.task {
            return Ok(());
        }
        let current = &self.task;
        let root = self.frontmatter.as_table_mut();
        if current.id != task.id {
            set(root, "id", Some(task.id.as_str().into()));
        }
        if current.title != task.title {
            set(root, "title", Some(task.title.as_str().into()));
        }
        if current.kind != task.kind {
            let (kind, format) = kind_values(&task.kind);
            set(root, "kind", Some(kind));
            set(root, "format", format);
        }
        if current.context != task.context {
            set(root, "context", Some(task.context.as_str().into()));
        }
        if current.status != task.status {
            let (status, waiting_on, waiting_until) = status_values(&task.status);
            set(root, "status", Some(status));
            set(root, "waiting_on", waiting_on);
            set(root, "waiting_until", waiting_until);
        }
        if current.importance != task.importance {
            set(root, "importance", Some(importance_value(task.importance)));
        }
        if current.due != task.due {
            set(root, "due", task.due.map(date_value));
        }
        if current.pinned != task.pinned {
            set(root, "pinned", task.pinned.then(|| true.into()));
        }
        if current.created != task.created {
            set(root, "created", Some(timestamp_value(task.created)));
        }
        if current.links != task.links {
            update_links(root, &task.links);
        }
        if current.relations != task.relations {
            update_relations(root, &task.relations);
        }
        self.unchanged = None;
        self.task = task;
        Ok(())
    }
}

/// A file cut at its `+++` lines.
struct Parts<'a> {
    open: &'a str,
    frontmatter: &'a str,
    close: &'a str,
    separator: &'a str,
    body: &'a str,
    /// Line of the closing `+++`.
    close_line: usize,
}

impl Parts<'_> {
    /// Line in the file of the byte at `offset` in the frontmatter.
    fn line_at(&self, offset: usize) -> usize {
        let before = self.frontmatter.get(..offset).unwrap_or(self.frontmatter);
        2 + before.matches('\n').count()
    }
}

/// The first line of `text`, with its line ending, and the rest.
fn first_line(text: &str) -> (&str, &str) {
    text.find('\n')
        .map_or((text, ""), |end| text.split_at(end + 1))
}

/// Whether `line` is a `+++` delimiter, with or without its line ending.
fn is_delimiter(line: &str) -> bool {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line) == DELIMITER
}

fn split(text: &str) -> Result<Parts<'_>, FormatError> {
    let (open, mut rest) = first_line(text);
    if !is_delimiter(open) {
        return Err(FormatError::MissingFrontmatter {
            body: text.to_owned(),
        });
    }
    let start = open.len();
    let mut end = start;
    let mut line = 2;
    loop {
        if rest.is_empty() {
            return Err(FormatError::UnclosedFrontmatter { line: line - 1 });
        }
        let (current, next) = first_line(rest);
        if is_delimiter(current) {
            let after = &text[end + current.len()..];
            let separator_len = if after.starts_with("\r\n") {
                2
            } else {
                usize::from(after.starts_with('\n'))
            };
            let (separator, body) = after.split_at(separator_len);
            return Ok(Parts {
                open,
                frontmatter: &text[start..end],
                close: current,
                separator,
                body,
                close_line: line,
            });
        }
        end += current.len();
        rest = next;
        line += 1;
    }
}

/// A key of the frontmatter that cannot be read.
struct FieldError {
    key: String,
    /// Where the key is in the frontmatter; `None` when it is missing.
    span: Option<Range<usize>>,
    message: String,
}

impl FieldError {
    fn missing(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            span: None,
            message: "missing".to_owned(),
        }
    }
}

/// Reads the task of a frontmatter, and lists what this version does not know.
fn decode(root: &Table) -> Result<(Task, Vec<String>), FieldError> {
    let kind: String = required(root, "kind")?;
    let format: Option<String> = field(root, "format")?;
    let kind = from_json::<Kind>(
        root,
        "kind",
        serde_json::json!({ "kind": kind, "format": format }),
    )?;
    let status: String = required(root, "status")?;
    let waiting_on: Option<String> = field(root, "waiting_on")?;
    let waiting_until: Option<Date> = field(root, "waiting_until")?;
    let status = from_json::<Status>(
        root,
        "status",
        serde_json::json!({
            "status": status,
            "waiting_on": waiting_on,
            "waiting_until": waiting_until.map(|date| date.to_string()),
        }),
    )?;
    let created = required(root, "created")?;
    let mut unknown: Vec<String> = root
        .iter()
        .map(|(key, _)| key)
        .filter(|key| !ROOT_KEYS.contains(key))
        .map(str::to_owned)
        .collect();
    let links = decode_links(root, &mut unknown)?;
    let relations = field::<Relations>(root, "relations")?.unwrap_or_default();
    if let Some(relations) = root.get("relations").and_then(Item::as_table_like) {
        unknown.extend(
            relations
                .iter()
                .map(|(key, _)| key)
                .filter(|key| !RELATION_KEYS.contains(key))
                .map(|key| format!("relations.{key}")),
        );
    }
    let task = Task {
        id: required(root, "id")?,
        title: required(root, "title")?,
        kind,
        context: required(root, "context")?,
        status,
        importance: required(root, "importance")?,
        due: field(root, "due")?,
        pinned: field(root, "pinned")?.unwrap_or(false),
        links,
        relations,
        created,
        updated: created,
    };
    Ok((task, unknown))
}

/// The links of known roles; a link of another role is listed in `unknown` as `links[<n>]`.
fn decode_links(root: &Table, unknown: &mut Vec<String>) -> Result<Vec<Link>, FieldError> {
    let Some((key, item)) = root.get_key_value("links") else {
        return Ok(Vec::new());
    };
    let elements: Vec<(Json, Option<Range<usize>>)> = match item {
        Item::ArrayOfTables(tables) => tables
            .iter()
            .map(|table| (table_to_json(table), table.span()))
            .collect(),
        Item::Value(Value::Array(array)) => array
            .iter()
            .map(|value| (value_to_json(value), value.span()))
            .collect(),
        Item::None | Item::Value(_) | Item::Table(_) => {
            return Err(FieldError {
                key: "links".to_owned(),
                span: key.span(),
                message: "expected an array of tables".to_owned(),
            });
        }
    };
    let mut links = Vec::new();
    for (index, (element, span)) in elements.into_iter().enumerate() {
        let invalid = |message: String| FieldError {
            key: format!("links[{index}]"),
            span: span.clone().or_else(|| key.span()),
            message,
        };
        let Some(role) = element.get("role").and_then(Json::as_str) else {
            return Err(invalid("missing `role`".to_owned()));
        };
        if !LINK_ROLES.contains(&role) {
            unknown.push(format!("links[{index}]"));
            continue;
        }
        if let Some(object) = element.as_object() {
            let known: &[&str] = if role == "repo" {
                &["role", "repo"]
            } else {
                &["role", "provider", "key", "url"]
            };
            unknown.extend(
                object
                    .keys()
                    .filter(|name| !known.contains(&name.as_str()))
                    .map(|name| format!("links[{index}].{name}")),
            );
        }
        links.push(serde_json::from_value(element).map_err(|error| invalid(error.to_string()))?);
    }
    Ok(links)
}

/// The value of `key`, `None` when it is absent.
fn field<T: DeserializeOwned>(table: &Table, key: &str) -> Result<Option<T>, FieldError> {
    let Some(item) = table.get(key) else {
        return Ok(None);
    };
    let Some(json) = item_to_json(item) else {
        return Ok(None);
    };
    from_json(table, key, json).map(Some)
}

fn required<T: DeserializeOwned>(table: &Table, key: &str) -> Result<T, FieldError> {
    field(table, key)?.ok_or_else(|| FieldError::missing(key))
}

/// Deserializes `json`, read from `key`, through the serde form of `T`.
fn from_json<T: DeserializeOwned>(table: &Table, key: &str, json: Json) -> Result<T, FieldError> {
    serde_json::from_value(json).map_err(|error| FieldError {
        key: key.to_owned(),
        span: table.key(key).and_then(toml_edit::Key::span),
        message: error.to_string(),
    })
}

/// A TOML item as JSON, for the serde forms of the domain types. Dates and times become their
/// RFC 3339 text, the form [`Date`] and [`Timestamp`] read.
fn item_to_json(item: &Item) -> Option<Json> {
    match item {
        Item::None => None,
        Item::Value(value) => Some(value_to_json(value)),
        Item::Table(table) => Some(table_to_json(table)),
        Item::ArrayOfTables(tables) => {
            Some(Json::Array(tables.iter().map(table_to_json).collect()))
        }
    }
}

fn table_to_json(table: &Table) -> Json {
    Json::Object(
        table
            .iter()
            .filter_map(|(key, item)| item_to_json(item).map(|json| (key.to_owned(), json)))
            .collect(),
    )
}

fn value_to_json(value: &Value) -> Json {
    match value {
        Value::String(text) => Json::String(text.value().clone()),
        Value::Integer(number) => Json::from(*number.value()),
        Value::Float(number) => {
            serde_json::Number::from_f64(*number.value()).map_or(Json::Null, Json::Number)
        }
        Value::Boolean(flag) => Json::Bool(*flag.value()),
        Value::Datetime(datetime) => Json::String(datetime.value().to_string()),
        Value::Array(array) => Json::Array(array.iter().map(value_to_json).collect()),
        Value::InlineTable(table) => Json::Object(
            table
                .iter()
                .map(|(key, value)| (key.to_owned(), value_to_json(value)))
                .collect(),
        ),
    }
}

/// Sets `key` to `value`, or removes it for `None`. An existing key keeps its place, the comments
/// above it and the spacing and comment around its value.
fn set(table: &mut dyn TableLike, key: &str, value: Option<Value>) {
    let Some(mut value) = value else {
        table.remove(key);
        return;
    };
    match table.get_mut(key) {
        Some(Item::Value(current)) => {
            *value.decor_mut() = current.decor().clone();
            *current = value;
        }
        Some(item) => *item = Item::Value(value),
        None => {
            table.insert(key, Item::Value(value));
        }
    }
}

fn kind_values(kind: &Kind) -> (Value, Option<Value>) {
    let format = match kind {
        Kind::Content { format } => format.as_ref().map(ContentFormat::name),
        Kind::Unknown { format, .. } => format.as_deref(),
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
    (kind.name().into(), format.map(Value::from))
}

fn status_values(status: &Status) -> (Value, Option<Value>, Option<Value>) {
    match status {
        Status::Waiting { on, until } => (
            "waiting".into(),
            on.as_deref().map(Value::from),
            until.map(date_value),
        ),
        Status::Unknown(name) => (name.as_str().into(), None, None),
        Status::Inbox | Status::Ready | Status::Active | Status::Done | Status::Cancelled => {
            let name = status
                .name()
                .map(|name| name.to_string())
                .unwrap_or_default();
            (name.into(), None, None)
        }
    }
}

fn importance_value(importance: Importance) -> Value {
    match importance {
        Importance::P0 => "p0",
        Importance::P1 => "p1",
        Importance::P2 => "p2",
        Importance::P3 => "p3",
    }
    .into()
}

/// A TOML local date, `2026-10-15`.
fn date_value(date: Date) -> Value {
    datetime_value(date.to_string())
}

/// A TOML offset date-time, `2026-10-12T09:14:00Z`.
fn timestamp_value(timestamp: Timestamp) -> Value {
    datetime_value(timestamp.to_string())
}

/// `text` as a TOML date-time; as a string if TOML cannot represent it, which RFC 3339 text in
/// the forms of [`Date`] and [`Timestamp`] never triggers.
fn datetime_value(text: String) -> Value {
    text.parse::<Datetime>()
        .map_or_else(|_| Value::from(text), Value::from)
}

fn link_table(link: &Link) -> Table {
    let mut table = Table::new();
    match link {
        Link::Origin(external) => external_link(&mut table, "origin", external),
        Link::Related(external) => external_link(&mut table, "related", external),
        Link::Delivery(external) => external_link(&mut table, "delivery", external),
        Link::Repo { repo } => {
            set(&mut table, "role", Some("repo".into()));
            set(&mut table, "repo", Some(repo.as_str().into()));
        }
    }
    table.decor_mut().set_prefix("\n");
    table
}

fn external_link(table: &mut Table, role: &str, external: &ExternalRef) {
    set(table, "role", Some(role.into()));
    write_external(table, external);
}

fn write_external(table: &mut Table, external: &ExternalRef) {
    set(table, "provider", Some(external.provider.as_str().into()));
    set(table, "key", Some(external.key.as_str().into()));
    set(table, "url", external.url.as_deref().map(Value::from));
}

/// The tables of `links`, in order: a link already in `existing` keeps its table (comments and
/// unknown keys included), a new one gets a fresh table.
fn links_tables(
    links: &[Link],
    existing: impl IntoIterator<Item = (Option<Link>, Table)>,
) -> ArrayOfTables {
    let mut existing: Vec<(Option<Link>, Table)> = existing.into_iter().collect();
    let mut tables = ArrayOfTables::new();
    for link in links {
        let reused = existing
            .iter()
            .position(|(decoded, _)| decoded.as_ref() == Some(link))
            .map(|index| existing.remove(index).1);
        tables.push(reused.unwrap_or_else(|| link_table(link)));
    }
    // Links of a role this version does not know stay, after the known ones.
    for (_, table) in existing.into_iter().filter(|(decoded, table)| {
        decoded.is_none() && table.get("role").and_then(Item::as_str).is_some()
    }) {
        tables.push(table);
    }
    tables
}

fn update_links(root: &mut Table, links: &[Link]) {
    let existing: Vec<(Option<Link>, Table)> = match root.get("links") {
        Some(Item::ArrayOfTables(tables)) => tables
            .iter()
            .map(|table| (decode_link(&table_to_json(table)), table.clone()))
            .collect(),
        Some(Item::Value(Value::Array(array))) => array
            .iter()
            .filter_map(Value::as_inline_table)
            .map(|inline| {
                let table = inline.clone().into_table();
                (decode_link(&table_to_json(&table)), table)
            })
            .collect(),
        Some(_) | None => Vec::new(),
    };
    let tables = links_tables(links, existing);
    if tables.is_empty() {
        root.remove("links");
    } else if let Some(item) = root.get_mut("links") {
        *item = Item::ArrayOfTables(tables);
    } else {
        root.insert("links", Item::ArrayOfTables(tables));
    }
}

/// The link of a known role in `json`, `None` for an unknown role or an invalid link.
fn decode_link(json: &Json) -> Option<Link> {
    let role = json.get("role").and_then(Json::as_str)?;
    if LINK_ROLES.contains(&role) {
        serde_json::from_value(json.clone()).ok()
    } else {
        None
    }
}

fn update_relations(root: &mut Table, relations: &Relations) {
    if let Some(table) = root.get_mut("relations").and_then(Item::as_table_like_mut) {
        write_relations(table, relations);
        return;
    }
    let mut table = Table::new();
    table.decor_mut().set_prefix("\n");
    write_relations(&mut table, relations);
    root.insert("relations", Item::Table(table));
}

fn write_relations(table: &mut dyn TableLike, relations: &Relations) {
    set(
        table,
        "parent",
        relations.parent.as_ref().map(|id| id.as_str().into()),
    );
    set(
        table,
        "spawned_from",
        Some(ids_value(&relations.spawned_from)),
    );
    set(table, "blocks", Some(ids_value(&relations.blocks)));
}

fn ids_value(ids: &[TaskId]) -> Value {
    Value::Array(ids.iter().map(TaskId::as_str).collect::<Array>())
}

#[cfg(test)]
mod tests;
