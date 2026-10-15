//! `StorePort`: how loomd reads and writes the tasks of one context store.
//!
//! The port hides what holds the tasks (local files now, a central instance later). The
//! decisions on what was read (is the file the task of its directory, when did the task last
//! change) are here, without I/O.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;

use loom_core::{
    ContextName, EventEncodeError, EventLog, EventRecord, FormatError, Task, TaskDocument, TaskId,
    Timestamp, parse_task_md, render_event,
};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

/// Reads and writes the tasks of one context.
///
/// Every write of a task holds its lock, so that two writers never interleave on one task.
/// Implementations write `task.md` before appending to `events.jsonl`: a crash in between leaves
/// a file the log does not explain yet, which the next reconciliation records as a hand edit.
pub trait StorePort {
    /// The context of the tasks.
    fn context(&self) -> &ContextName;

    /// Creates the task of `write.document` with the events of `write`, all at once: a crash
    /// leaves either no task or the whole task.
    ///
    /// # Errors
    ///
    /// [`PortError::AlreadyExists`], [`PortError::WrongContext`] for a task of another context,
    /// [`PortError::MissingDocument`] without a document, or the write error.
    fn create(&self, write: &TaskWrite) -> Result<(), PortError>;

    /// The task with `id`: its file and its event log.
    ///
    /// # Errors
    ///
    /// [`PortError::NotFound`] when the task has no `task.md`, or the read error.
    fn read(&self, id: &TaskId) -> Result<StoredTask, PortError>;

    /// Reads the task with `id`, hands it to `change` and writes what `change` returns, under
    /// the task's lock: no other write of the task runs in between.
    ///
    /// # Errors
    ///
    /// The error of `change`, of the read or of the write.
    fn update<T, E: From<PortError>>(
        &self,
        id: &TaskId,
        change: impl FnOnce(StoredTask) -> Result<(TaskWrite, T), E>,
    ) -> Result<T, E>;

    /// Whether the store holds a task with `id`, active or archived. Looks at the directories
    /// only: no file is read.
    ///
    /// # Errors
    ///
    /// The read error of a directory.
    fn contains(&self, id: &TaskId) -> Result<bool, PortError>;

    /// IDs of the tasks of the context, active and archived, sorted, each once.
    ///
    /// # Errors
    ///
    /// The read error.
    fn list(&self) -> Result<Vec<TaskId>, PortError>;

    /// Moves the active task `id` to the archive of `year`, all at once and under its lock: a
    /// crash leaves the task in exactly one place, active or archived.
    ///
    /// # Errors
    ///
    /// [`PortError::NotFound`] when there is no such task, [`PortError::Archived`] when it is
    /// archived already, [`PortError::AlreadyExists`] when the archive already holds a task
    /// with this ID, or the write error.
    fn archive(&self, id: &TaskId, year: u16) -> Result<(), PortError>;

    /// The event log of the task with `id`; empty when it has none.
    ///
    /// # Errors
    ///
    /// [`PortError::NotFound`] when there is no such task, or the read error.
    fn read_events(&self, id: &TaskId) -> Result<EventLog, PortError>;
}

/// What a write changes: `task.md`, and events appended to `events.jsonl`.
#[derive(Debug, Clone, Default)]
pub struct TaskWrite {
    /// The new `task.md`; `None` leaves the file as it is.
    pub document: Option<TaskDocument>,
    /// Events to append, in order.
    pub events: Vec<EventRecord>,
}

/// Where the directory of a task is in its store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskLocation {
    /// `tasks/<id>`.
    Active,
    /// `archive/<yyyy>/<id>`, `<yyyy>` the year the task was closed: read-only.
    Archived(u16),
}

impl TaskLocation {
    /// The year of the archive, `None` for an active task.
    #[must_use]
    pub const fn archive_year(self) -> Option<u16> {
        match self {
            Self::Active => None,
            Self::Archived(year) => Some(year),
        }
    }
}

/// A task as read from its store.
#[derive(Debug, Clone)]
pub struct StoredTask {
    /// ID, the name of its directory.
    pub id: TaskId,
    /// Context of its store.
    pub context: ContextName,
    /// Where its directory is.
    pub location: TaskLocation,
    /// `task.md`.
    pub file: TaskFile,
    /// `events.jsonl`.
    pub events: EventLog,
}

impl StoredTask {
    /// The task, last updated at the time of its last event (its creation time without one).
    ///
    /// # Errors
    ///
    /// Why the file is invalid.
    pub fn task(&self) -> Result<Task, &InvalidTask> {
        let document = self.file.document.as_ref()?;
        let created = document.task(Timestamp::EPOCH).created;
        let updated = self.events.events.last().map_or(created, |event| event.at);
        Ok(document.task(updated))
    }
}

/// A `task.md` file as read.
#[derive(Debug, Clone)]
pub struct TaskFile {
    /// Hash of the bytes read.
    pub hash: ContentHash,
    /// The text, with invalid UTF-8 replaced (`U+FFFD`): for display only.
    pub text: String,
    /// The parsed document, or why the file is invalid.
    pub document: Result<TaskDocument, InvalidTask>,
}

impl TaskFile {
    /// Reads `bytes` as the `task.md` of task `id` in `context`.
    #[must_use]
    pub fn parse(bytes: Vec<u8>, id: &TaskId, context: &ContextName) -> Self {
        let hash = ContentHash::of(&bytes);
        let (text, document) = match String::from_utf8(bytes) {
            Ok(text) => {
                let document = parse_task_md(&text)
                    .map_err(InvalidTask::from)
                    .and_then(|document| belongs(document, id, context));
                (text, document)
            }
            Err(error) => (
                String::from_utf8_lossy(error.as_bytes()).into_owned(),
                Err(InvalidTask::NotUtf8),
            ),
        };
        Self {
            hash,
            text,
            document,
        }
    }
}

/// `document`, if it is the task `id` of `context`.
fn belongs(
    document: TaskDocument,
    id: &TaskId,
    context: &ContextName,
) -> Result<TaskDocument, InvalidTask> {
    let task = document.task(Timestamp::EPOCH);
    if task.id != *id {
        return Err(InvalidTask::WrongId {
            expected: id.clone(),
            found: task.id,
        });
    }
    if task.context != *context {
        return Err(InvalidTask::WrongContext {
            expected: context.clone(),
            found: task.context,
        });
    }
    Ok(document)
}

/// Why a `task.md` cannot be used. The file is kept as it is, never rewritten.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InvalidTask {
    /// The file is not valid UTF-8.
    #[error("the file is not valid UTF-8")]
    NotUtf8,
    /// The frontmatter is invalid.
    #[error(transparent)]
    Format(#[from] FormatError),
    /// `id` is not the name of the task directory.
    #[error("`id` is `{found}`, but the task directory is `{expected}`")]
    WrongId {
        /// Name of the directory.
        expected: TaskId,
        /// `id` of the file.
        found: TaskId,
    },
    /// `context` is not the context of the store.
    #[error("`context` is `{found}`, but the task is in the store of `{expected}`")]
    WrongContext {
        /// Context of the store.
        expected: ContextName,
        /// `context` of the file.
        found: ContextName,
    },
    /// The same task ID is in the stores of several contexts (a task directory copied by hand):
    /// loomd cannot tell which one a command is for.
    #[error(
        "duplicate ID across contexts: `{id}` is in the stores of {}; keep one task directory and remove the others",
        contexts_text(.contexts)
    )]
    DuplicateId {
        /// The ID.
        id: TaskId,
        /// The contexts whose stores hold it, in the order of the stores.
        contexts: Vec<ContextName>,
    },
}

fn contexts_text(contexts: &[ContextName]) -> String {
    contexts
        .iter()
        .map(ContextName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Lowercase hex SHA-256 of a file: tells whether it changed since loomd last read or wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContentHash(String);

impl ContentHash {
    /// The hash of `bytes`.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(
            Sha256::digest(bytes)
                .iter()
                .fold(String::with_capacity(64), |mut hex, byte| {
                    let _ = write!(hex, "{byte:02x}");
                    hex
                }),
        )
    }

    /// A hash as stored in the index.
    #[must_use]
    pub const fn from_hex(hex: String) -> Self {
        Self(hex)
    }

    /// The hash in hex.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A store operation failed.
#[derive(Debug, Error)]
pub enum PortError {
    /// No such task in the store.
    #[error("no task `{id}` in the store of `{context}`")]
    NotFound {
        /// Context of the store.
        context: ContextName,
        /// The task.
        id: TaskId,
    },
    /// The task is archived: it cannot move or change.
    #[error("task `{id}` is archived in archive/{year} of the store of `{context}`")]
    Archived {
        /// Context of the store.
        context: ContextName,
        /// The task.
        id: TaskId,
        /// Year of its archive.
        year: u16,
    },
    /// A task with this ID already exists.
    #[error("task `{id}` already exists in the store of `{context}`")]
    AlreadyExists {
        /// Context of the store.
        context: ContextName,
        /// The task.
        id: TaskId,
    },
    /// The task belongs to another context.
    #[error("task `{id}` belongs to `{found}`, not to the store of `{expected}`")]
    WrongContext {
        /// The task.
        id: TaskId,
        /// Context of the store.
        expected: ContextName,
        /// Context of the task.
        found: ContextName,
    },
    /// A task is created from its document, which is missing.
    #[error("a task is created with its task.md")]
    MissingDocument,
    /// An event cannot be serialized.
    #[error(transparent)]
    Encode(#[from] EventEncodeError),
    /// A file or directory cannot be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
}

/// The text to append to an `events.jsonl` whose last byte is `last`: the lines of `events`,
/// after a line ending when the last line was cut (a crash during an append), so that every
/// appended line stays whole.
///
/// # Errors
///
/// [`EventEncodeError`] when an event cannot be serialized.
pub fn appended(last: Option<u8>, events: &[EventRecord]) -> Result<String, EventEncodeError> {
    let mut text = String::new();
    if events.is_empty() {
        return Ok(text);
    }
    if last.is_some_and(|byte| byte != b'\n') {
        text.push('\n');
    }
    for event in events {
        text.push_str(&render_event(event)?);
    }
    Ok(text)
}

/// The year of the archive directory `name`: four digits, 1970 to 9999. Other names are not
/// archive years.
#[must_use]
pub fn archive_year_dir(name: &OsStr) -> Option<u16> {
    let name = name.to_str()?;
    if name.len() != 4 || !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    name.parse()
        .ok()
        .filter(|year| (1970..=9999).contains(year))
}

/// The task whose directory is `name`: only canonical task IDs are tasks (temporary
/// directories start with `.`).
#[must_use]
pub fn task_dir_id(name: &OsStr) -> Option<TaskId> {
    name.to_str()?.parse().ok()
}

#[cfg(test)]
mod tests;
