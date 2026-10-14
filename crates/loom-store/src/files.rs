//! The files adapter of [`StorePort`]: one directory per task under the context store,
//! `<store>/tasks/<id>/{task.md,events.jsonl}` (ADR-0013).
//!
//! - `task.md` is replaced atomically (temporary file, sync, rename, sync of the directory).
//! - `events.jsonl` only grows: lines are appended and synced.
//! - A creation builds the whole task directory under a hidden name and renames it into place.
//! - Every write of a task holds the task's lock, for the whole read, change and write.

use std::collections::HashMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use loom_core::{ContextName, EventLog, TaskId, Timestamp, parse_events, render_task_md};

use crate::atomic::{self, sync_dir, sync_parent};
use crate::port::{PortError, StorePort, StoredTask, TaskFile, TaskWrite, appended, task_dir_id};

/// Mode of the task directories and of `tasks/`: owner only, the store holds private notes.
pub const TASK_DIR_MODE: u32 = 0o700;
/// Mode of `task.md` and `events.jsonl`: owner read and write.
pub const TASK_FILE_MODE: u32 = 0o600;

/// `task.md`, in each task directory.
pub const TASK_FILE: &str = "task.md";
/// `events.jsonl`, in each task directory.
pub const EVENTS_FILE: &str = "events.jsonl";
/// `tasks/`, in each context store.
pub const TASKS_DIR: &str = "tasks";

/// The tasks of one context, as files under its store root.
#[derive(Debug)]
pub struct FileStore {
    context: ContextName,
    root: PathBuf,
    /// One lock per task written since the store was opened.
    locks: Mutex<HashMap<TaskId, Arc<Mutex<()>>>>,
}

impl FileStore {
    /// The store of `context` rooted at `root` (`store.path`). Nothing is created until the first
    /// write.
    #[must_use]
    pub fn new(context: ContextName, root: PathBuf) -> Self {
        Self {
            context,
            root,
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// Root of the store.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/tasks`.
    #[must_use]
    pub fn tasks_dir(&self) -> PathBuf {
        self.root.join(TASKS_DIR)
    }

    /// `<root>/tasks/<id>`.
    #[must_use]
    pub fn task_dir(&self, id: &TaskId) -> PathBuf {
        self.tasks_dir().join(id.as_str())
    }

    /// Creates `<root>/tasks` (mode `0700`) when missing.
    ///
    /// # Errors
    ///
    /// [`PortError::Io`] when it cannot be created.
    pub fn init(&self) -> Result<(), PortError> {
        let tasks = self.tasks_dir();
        DirBuilder::new()
            .recursive(true)
            .mode(TASK_DIR_MODE)
            .create(&tasks)
            .map_err(|source| io_error(&tasks, source))
    }

    /// The lock of task `id`, created on first use.
    fn lock(&self, id: &TaskId) -> Arc<Mutex<()>> {
        Arc::clone(
            lock(&self.locks)
                .entry(id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    fn not_found(&self, id: &TaskId) -> PortError {
        PortError::NotFound {
            context: self.context.clone(),
            id: id.clone(),
        }
    }

    /// Reads task `id`; the caller holds its lock when it writes afterwards.
    fn read_unlocked(&self, id: &TaskId) -> Result<StoredTask, PortError> {
        let dir = self.task_dir(id);
        let path = dir.join(TASK_FILE);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if is_missing(&error) => return Err(self.not_found(id)),
            Err(source) => return Err(io_error(&path, source)),
        };
        Ok(StoredTask {
            id: id.clone(),
            context: self.context.clone(),
            file: TaskFile::parse(bytes, id, &self.context),
            events: read_log(&dir.join(EVENTS_FILE))?,
        })
    }

    /// Writes `write` to the existing task `id`, its lock held: `task.md`, then the events.
    fn write_unlocked(&self, id: &TaskId, write: &TaskWrite) -> Result<(), PortError> {
        let dir = self.task_dir(id);
        if let Some(document) = &write.document {
            let path = dir.join(TASK_FILE);
            atomic::write_file(&path, render_task_md(document).as_bytes(), TASK_FILE_MODE)
                .map_err(|source| io_error(&path, source))?;
        }
        append_events(&dir.join(EVENTS_FILE), write)
    }
}

impl StorePort for FileStore {
    fn context(&self) -> &ContextName {
        &self.context
    }

    fn create(&self, write: &TaskWrite) -> Result<(), PortError> {
        let document = write.document.as_ref().ok_or(PortError::MissingDocument)?;
        let task = document.task(Timestamp::EPOCH);
        if task.context != self.context {
            return Err(PortError::WrongContext {
                id: task.id,
                expected: self.context.clone(),
                found: task.context,
            });
        }
        let lock = self.lock(&task.id);
        let _held = lock_guard(&lock);
        self.init()?;
        let target = self.task_dir(&task.id);
        if target.exists() {
            return Err(PortError::AlreadyExists {
                context: self.context.clone(),
                id: task.id,
            });
        }
        // The whole task is staged under a hidden name, then renamed into place: a crash leaves
        // no task or the whole task. A leftover of a previous crash is replaced.
        let staging = self.tasks_dir().join(format!(".{}.tmp", task.id));
        let created = (|| {
            match fs::remove_dir_all(&staging) {
                Ok(()) => {}
                Err(error) if is_missing(&error) => {}
                Err(error) => return Err(error),
            }
            DirBuilder::new().mode(TASK_DIR_MODE).create(&staging)?;
            write_new(
                &staging.join(TASK_FILE),
                render_task_md(document).as_bytes(),
            )?;
            let events = appended(None, &write.events)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            write_new(&staging.join(EVENTS_FILE), events.as_bytes())?;
            sync_dir(&staging)?;
            fs::rename(&staging, &target)?;
            sync_parent(&target)
        })();
        created.map_err(|source| io_error(&target, source))
    }

    fn read(&self, id: &TaskId) -> Result<StoredTask, PortError> {
        self.read_unlocked(id)
    }

    fn update<T, E: From<PortError>>(
        &self,
        id: &TaskId,
        change: impl FnOnce(StoredTask) -> Result<(TaskWrite, T), E>,
    ) -> Result<T, E> {
        let lock = self.lock(id);
        let _held = lock_guard(&lock);
        let stored = self.read_unlocked(id)?;
        let (write, value) = change(stored)?;
        self.write_unlocked(id, &write)?;
        Ok(value)
    }

    fn list(&self) -> Result<Vec<TaskId>, PortError> {
        let tasks = self.tasks_dir();
        let entries = match fs::read_dir(&tasks) {
            Ok(entries) => entries,
            Err(error) if is_missing(&error) => return Ok(Vec::new()),
            Err(source) => return Err(io_error(&tasks, source)),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| io_error(&tasks, source))?;
            let Some(id) = task_dir_id(&entry.file_name()) else {
                continue;
            };
            // Follows symbolic links: a task directory linked from elsewhere is a task.
            if entry.path().is_dir() {
                ids.push(id);
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn read_events(&self, id: &TaskId) -> Result<EventLog, PortError> {
        let dir = self.task_dir(id);
        if !dir.is_dir() {
            return Err(self.not_found(id));
        }
        read_log(&dir.join(EVENTS_FILE))
    }
}

/// The event log at `path`; empty when the file is missing. Invalid UTF-8 is replaced, and the
/// lines holding it are skipped by the parser.
fn read_log(path: &Path) -> Result<EventLog, PortError> {
    match fs::read(path) {
        Ok(bytes) => Ok(parse_events(&String::from_utf8_lossy(&bytes))),
        Err(error) if is_missing(&error) => Ok(EventLog::default()),
        Err(source) => Err(io_error(path, source)),
    }
}

/// Appends the events of `write` to the log at `path` (created when missing) and syncs it.
fn append_events(path: &Path, write: &TaskWrite) -> Result<(), PortError> {
    if write.events.is_empty() {
        return Ok(());
    }
    let error = |source| io_error(path, source);
    let existed = path.exists();
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(TASK_FILE_MODE)
        .open(path)
        .map_err(error)?;
    let last = last_byte(&mut file).map_err(error)?;
    let text = appended(last, &write.events)?;
    file.write_all(text.as_bytes()).map_err(error)?;
    file.sync_all().map_err(error)?;
    if !existed {
        sync_parent(path).map_err(error)?;
    }
    Ok(())
}

/// The last byte of `file`, `None` when it is empty.
fn last_byte(file: &mut fs::File) -> io::Result<Option<u8>> {
    if file.metadata()?.len() == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut byte = [0];
    file.read_exact(&mut byte)?;
    Ok(Some(byte[0]))
}

/// Creates the file at `path` with `contents` and syncs it.
fn write_new(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(TASK_FILE_MODE)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

fn is_missing(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

fn io_error(path: &Path, source: io::Error) -> PortError {
    PortError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// `mutex` locked; a thread that panicked holding it left consistent data (a map of locks).
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The lock of a task held; a panic during a write left the files as atomic writes do.
fn lock_guard(mutex: &Mutex<()>) -> MutexGuard<'_, ()> {
    lock(mutex)
}
