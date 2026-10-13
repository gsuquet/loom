//! Disk usage of the loom home: the walk, the free space query and the cache of the status
//! endpoint. Where each entry is counted and when a measure is reused is decided in
//! `loomd::disk`.

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Instant, SystemTime};

use loom_api::{DiskUsage, Timestamp};
use loom_config::layout::Layout;
use loomd::disk::{Bucket, Stamp, Tally, allocated, available, bucket, is_fresh};
use tracing::warn;

/// The last measure, reused while [`is_fresh`] says so.
#[derive(Default)]
pub struct DiskCache {
    last: Mutex<Option<(Stamp, DiskUsage)>>,
}

impl DiskCache {
    /// The usage of the loom home of `layout`: the last measure while it is fresh, otherwise a
    /// new one, taken on a blocking thread. `None` only when that thread failed.
    pub async fn get(&self, layout: &Layout, now: Timestamp) -> Option<DiskUsage> {
        let versions_modified = modified(&layout.versions_dir);
        if let Some((stamp, usage)) = self
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            && is_fresh(stamp, Instant::now(), versions_modified)
        {
            return Some(usage.clone());
        }
        let home = layout.home.clone();
        let versions_dir = layout.versions_dir.clone();
        let measured = tokio::task::spawn_blocking(move || {
            // Taken before the walk: a change during the walk makes the next request measure
            // again.
            let stamp = Stamp {
                measured: Instant::now(),
                versions_modified: modified(&versions_dir),
            };
            (stamp, measure(&home, now))
        })
        .await;
        match measured {
            Ok((stamp, usage)) => {
                *self.last.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((stamp, usage.clone()));
                Some(usage)
            }
            Err(error) => {
                warn!(%error, "cannot measure the disk usage of the loom home");
                None
            }
        }
    }
}

/// Modification time of `path`, `None` when it cannot be read (missing included).
fn modified(path: &Path) -> Option<SystemTime> {
    fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

/// Walks the loom home `home` and queries the free space of its volume. Never fails: what
/// cannot be read counts as 0 and gives a warning, also logged.
pub fn measure(home: &Path, now: Timestamp) -> DiskUsage {
    let mut walk = Walk::default();
    let mut kept_versions = Vec::new();
    // The home itself may be a symbolic link (the user's choice of location): followed once.
    match fs::metadata(home) {
        Ok(metadata) => {
            let bytes = walk.size(&metadata);
            walk.tally.add(Bucket::Other, bytes);
        }
        Err(error) => walk.cannot_read(home, &error),
    }
    for (path, metadata) in walk.children(home) {
        let Some(name) = path.file_name().map(ToOwned::to_owned) else {
            continue;
        };
        if name == "contexts" && metadata.is_dir() {
            let bytes = walk.size(&metadata);
            walk.tally.add(Bucket::Other, bytes);
            for (store, store_metadata) in walk.children(&path) {
                let context = store_metadata
                    .is_dir()
                    .then(|| store.file_name().map(ToOwned::to_owned))
                    .flatten();
                let bytes = walk.subtree(store, &store_metadata);
                walk.tally.add(bucket(&name, context.as_deref()), bytes);
            }
            continue;
        }
        if name == "versions" && metadata.is_dir() {
            kept_versions = walk
                .children(&path)
                .into_iter()
                .filter(|(_, metadata)| metadata.is_dir())
                .filter_map(|(path, _)| Some(path.file_name()?.to_string_lossy().into_owned()))
                .collect();
        }
        let bytes = walk.subtree(path, &metadata);
        walk.tally.add(bucket(&name, None), bytes);
    }
    let available_bytes = match rustix::fs::statvfs(home) {
        Ok(stat) => Some(available(stat.f_bavail, stat.f_frsize)),
        Err(error) => {
            walk.warn(format!(
                "cannot query the free space of the volume of {}: {}",
                home.display(),
                io::Error::from(error)
            ));
            None
        }
    };
    walk.tally
        .finish(now, kept_versions, available_bytes, walk.warnings)
}

/// State of one walk.
#[derive(Default)]
struct Walk {
    tally: Tally,
    /// `(device, inode)` of the files with several links already counted.
    seen: HashSet<(u64, u64)>,
    warnings: Vec<String>,
}

impl Walk {
    fn warn(&mut self, message: String) {
        warn!("{message}");
        self.warnings.push(message);
    }

    fn cannot_read(&mut self, path: &Path, error: &io::Error) {
        self.warn(format!("cannot read {}: {error}", path.display()));
    }

    /// Allocated bytes of one entry (not followed when it is a symbolic link); a file with
    /// several hard links counts once.
    fn size(&mut self, metadata: &Metadata) -> u64 {
        if !metadata.is_dir()
            && metadata.nlink() > 1
            && !self.seen.insert((metadata.dev(), metadata.ino()))
        {
            return 0;
        }
        allocated(metadata.blocks())
    }

    /// Entries of the directory `dir`, with their metadata read without following symbolic
    /// links. An unreadable directory or entry is skipped with a warning.
    fn children(&mut self, dir: &Path) -> Vec<(PathBuf, Metadata)> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                self.cannot_read(dir, &error);
                return Vec::new();
            }
        };
        let mut children = Vec::new();
        for entry in entries {
            match entry.and_then(|entry| Ok((entry.path(), entry.metadata()?))) {
                Ok(child) => children.push(child),
                // Deleted since the listing (a log rotated, a temporary file renamed).
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => self.cannot_read(dir, &error),
            }
        }
        children
    }

    /// Allocated bytes of `path` and, when it is a directory (not a symbolic link to one),
    /// of everything under it.
    fn subtree(&mut self, path: PathBuf, metadata: &Metadata) -> u64 {
        let mut total = self.size(metadata);
        if !metadata.is_dir() {
            return total;
        }
        let mut stack = vec![path];
        while let Some(dir) = stack.pop() {
            for (child, metadata) in self.children(&dir) {
                total = total.saturating_add(self.size(&metadata));
                // `Metadata` of a symbolic link: `is_dir` is false, so links are never followed.
                if metadata.is_dir() {
                    stack.push(child);
                }
            }
        }
        total
    }
}
