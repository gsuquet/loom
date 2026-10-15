//! The files adapter of [`StorePort`]: one directory per task under the context store,
//! `<store>/tasks/<id>/{task.md,events.jsonl}`, moved to `<store>/archive/<yyyy>/<id>/` when
//! archived (ADR-0013).
//!
//! - `task.md` is replaced atomically (temporary file, sync, rename, sync of the directory).
//! - `events.jsonl` only grows: lines are appended and synced.
//! - A creation builds the whole task directory under a hidden name and renames it into place.
//! - Every write of a task holds the task's lock, for the whole read, change and write.
//! - The archive moves a task directory with one rename: a crash leaves it in `tasks/` or in
//!   the archive, never in both or in neither.
//! - A task is looked for in `tasks/` first, then in the archive years in order: the first
//!   directory found is the task.

use std::collections::HashMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use loom_core::{ContextName, EventLog, TaskId, Timestamp, parse_events, render_task_md};

use crate::atomic::{self, sync_dir, sync_parent};
use crate::port::{
    PortError, StorePort, StoredTask, TaskFile, TaskLocation, TaskWrite, appended,
    archive_year_dir, task_dir_id,
};

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
/// `archive/`, in each context store: one directory per year of closure.
pub const ARCHIVE_DIR: &str = "archive";

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

    /// `<root>/archive`.
    #[must_use]
    pub fn archive_dir(&self) -> PathBuf {
        self.root.join(ARCHIVE_DIR)
    }

    /// The directory of task `id` at `location`: `<root>/tasks/<id>` or
    /// `<root>/archive/<yyyy>/<id>`.
    #[must_use]
    pub fn dir_at(&self, id: &TaskId, location: TaskLocation) -> PathBuf {
        match location {
            TaskLocation::Active => self.task_dir(id),
            TaskLocation::Archived(year) => self
                .archive_dir()
                .join(format!("{year:04}"))
                .join(id.as_str()),
        }
    }

    /// The years of the archive, sorted; none when there is no archive.
    fn archive_years(&self) -> Result<Vec<u16>, PortError> {
        let archive = self.archive_dir();
        let entries = match fs::read_dir(&archive) {
            Ok(entries) => entries,
            Err(error) if is_missing(&error) => return Ok(Vec::new()),
            Err(source) => return Err(io_error(&archive, source)),
        };
        let mut years = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| io_error(&archive, source))?;
            if let Some(year) = archive_year_dir(&entry.file_name())
                && entry.path().is_dir()
            {
                years.push(year);
            }
        }
        years.sort_unstable();
        Ok(years)
    }

    /// Where task `id` is: `tasks/` first, then the archive years in order. `None` when it is
    /// in neither.
    fn locate(&self, id: &TaskId) -> Result<Option<TaskLocation>, PortError> {
        if self.task_dir(id).is_dir() {
            return Ok(Some(TaskLocation::Active));
        }
        Ok(self
            .archive_years()?
            .into_iter()
            .map(TaskLocation::Archived)
            .find(|location| self.dir_at(id, *location).is_dir()))
    }

    /// Creates `<root>/tasks` and `<root>/archive` (mode `0700`) when missing.
    ///
    /// # Errors
    ///
    /// [`PortError::Io`] when one cannot be created.
    pub fn init(&self) -> Result<(), PortError> {
        for dir in [self.tasks_dir(), self.archive_dir()] {
            DirBuilder::new()
                .recursive(true)
                .mode(TASK_DIR_MODE)
                .create(&dir)
                .map_err(|source| io_error(&dir, source))?;
        }
        Ok(())
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
        let location = self.locate(id)?.ok_or_else(|| self.not_found(id))?;
        let dir = self.dir_at(id, location);
        let path = dir.join(TASK_FILE);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if is_missing(&error) => return Err(self.not_found(id)),
            Err(source) => return Err(io_error(&path, source)),
        };
        Ok(StoredTask {
            id: id.clone(),
            context: self.context.clone(),
            location,
            file: TaskFile::parse(bytes, id, &self.context),
            events: read_log(&dir.join(EVENTS_FILE))?,
        })
    }

    /// Writes `write` to the existing task `id` at `location`, its lock held: `task.md`, then
    /// the events.
    fn write_unlocked(
        &self,
        id: &TaskId,
        location: TaskLocation,
        write: &TaskWrite,
    ) -> Result<(), PortError> {
        let dir = self.dir_at(id, location);
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
        if target.exists() || self.locate(&task.id)?.is_some() {
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
        let location = stored.location;
        let (write, value) = change(stored)?;
        self.write_unlocked(id, location, &write)?;
        Ok(value)
    }

    fn list(&self) -> Result<Vec<TaskId>, PortError> {
        let mut ids = task_dirs(&self.tasks_dir())?;
        for year in self.archive_years()? {
            let dir = self.archive_dir().join(format!("{year:04}"));
            ids.extend(task_dirs(&dir)?);
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    fn read_events(&self, id: &TaskId) -> Result<EventLog, PortError> {
        let location = self.locate(id)?.ok_or_else(|| self.not_found(id))?;
        read_log(&self.dir_at(id, location).join(EVENTS_FILE))
    }

    fn archive(&self, id: &TaskId, year: u16) -> Result<(), PortError> {
        let lock = self.lock(id);
        let _held = lock_guard(&lock);
        match self.locate(id)? {
            None => return Err(self.not_found(id)),
            Some(TaskLocation::Archived(year)) => {
                return Err(PortError::Archived {
                    context: self.context.clone(),
                    id: id.clone(),
                    year,
                });
            }
            Some(TaskLocation::Active) => {}
        }
        let source = self.task_dir(id);
        let target = self.dir_at(id, TaskLocation::Archived(year));
        // `rename` replaces an empty directory: a task already there is never overwritten. The
        // check and the rename are two steps; the task lock covers loomd, not a hand `mv` in
        // between. TODO: `renameat2(RENAME_NOREPLACE)` on Linux and `renamex_np(RENAME_EXCL)`
        // on macOS make it one step.
        if target.exists() {
            return Err(PortError::AlreadyExists {
                context: self.context.clone(),
                id: id.clone(),
            });
        }
        let error = |source| io_error(&target, source);
        let year_dir = target.parent().unwrap_or(&target);
        DirBuilder::new()
            .recursive(true)
            .mode(TASK_DIR_MODE)
            .create(year_dir)
            .map_err(error)?;
        // The new directories survive a crash before the rename does.
        sync_parent(year_dir).map_err(error)?;
        sync_parent(&self.archive_dir()).map_err(error)?;
        fs::rename(&source, &target).map_err(error)?;
        sync_dir(year_dir).map_err(error)?;
        sync_parent(&source).map_err(error)
    }
}

/// The task directories of `dir`, unsorted; none when it is missing.
fn task_dirs(dir: &Path) -> Result<Vec<TaskId>, PortError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if is_missing(&error) => return Ok(Vec::new()),
        Err(source) => return Err(io_error(dir, source)),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| io_error(dir, source))?;
        let Some(id) = task_dir_id(&entry.file_name()) else {
            continue;
        };
        // Follows symbolic links: a task directory linked from elsewhere is a task.
        if entry.path().is_dir() {
            ids.push(id);
        }
    }
    Ok(ids)
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
