//! The queue entries of a poll, written on the task thread: the decisions are in
//! `loomd::review_queue`. A file is written only when its bytes change.

use std::fs;
use std::io;
use std::path::Path;

use loom_api::{ContextName, ReviewQueue};
use loom_forge::model::{Mr, MrKey, MrState};
use loom_store::IndexedTask;
use loom_store::files::ARCHIVE_DIR;
use loom_store::port::TaskLocation;
use loomd::review_queue::{
    ArchivedSeen, EntryDecision, EntrySeen, QueueEntry, decide_entry, entry_bytes, entry_name,
    entry_of, queue_dir, read_entry_bytes,
};
use loomd::review_sync::{MrBatch, MrUpdate, find_review, mirror_dir, next_verdict, url_host};
use tracing::{info, warn};

use super::{Previous, ReviewsApplied, Worker, can_create, read_previous, write_mirror};

impl Worker {
    /// The queue entry of one MR, after its Review task: adds, keeps or removes it. A failed
    /// read comes with no update, so it changes no entry.
    pub(super) fn sync_entry(
        &self,
        batch: &MrBatch,
        rows: &[IndexedTask],
        update: &MrUpdate,
        applied: &mut ReviewsApplied,
        updated: &mut usize,
    ) -> Result<(), String> {
        let context = &batch.context;
        let root = self
            .stores
            .get(context)
            .ok_or_else(|| format!("no store for context `{context}`"))?;
        let review = find_review(rows, context, &batch.host, &update.key);
        let tracked = update.start || matches!(review, Some((_, _, TaskLocation::Active)));
        let stored = read_entry(root, &update.key);
        // Two forge hosts can have the same key, and their entries the same file name.
        let foreign = stored
            .as_ref()
            .is_some_and(|entry| url_host(&entry.web_url).as_deref() != Some(batch.host.as_str()));
        let stored = stored.filter(|_| !foreign);
        if tracked && stored.is_none() {
            return Ok(());
        }
        let archived = matches!(review, Some((_, _, TaskLocation::Archived(_))));
        let previous = match review {
            Some((id, _, TaskLocation::Archived(year))) => read_previous(&mirror_dir(
                &root
                    .join(ARCHIVE_DIR)
                    .join(format!("{year:04}"))
                    .join(id.as_str()),
                &update.key,
            )),
            Some((_, _, TaskLocation::Active)) | None => Previous::default(),
        };
        let read = update.mr.as_ref();
        let known = read.or(previous.mr.as_ref());
        // A 304, and neither an entry nor a mirror: nothing changed.
        let Some(seen) = entry_seen(update, &previous, stored.as_ref(), tracked, archived) else {
            return Ok(());
        };
        let decision = decide_entry(&seen);
        let name = entry_name(&update.key);
        let dir = queue_dir(root);
        let write = |mr: &Mr| -> Result<bool, String> {
            can_create(root, rows, batch, mr)?;
            let bytes =
                entry_bytes(&entry_of(mr, stored.as_ref())).map_err(|error| error.to_string())?;
            write_mirror(&dir, &[(name.as_str(), bytes.as_slice())])
                .map_err(|error| format!("cannot write the entry {}: {error}", dir.display()))
        };
        match decision {
            EntryDecision::Absent => {}
            EntryDecision::Add => {
                if foreign {
                    return Err(format!(
                        "the queue has an entry of this key on another forge host than {}: no entry",
                        batch.host
                    ));
                }
                let mr = known.ok_or_else(|| "no read of the MR for its entry".to_owned())?;
                write(mr)?;
                info!(%context, key = %update.key, "MR added to the queue");
                applied.added.push(update.key.to_string());
            }
            EntryDecision::Keep => {
                if let Some(mr) = read
                    && write(mr)?
                {
                    *updated += 1;
                }
            }
            EntryDecision::Remove => {
                match fs::remove_file(dir.join(&name)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!("cannot delete the entry {name}: {error}"));
                    }
                }
                info!(%context, key = %update.key, "MR removed from the queue");
                applied.removed.push(update.key.to_string());
            }
        }
        Ok(())
    }

    /// The entries of the queue of `context`, or of every context, sorted.
    pub(in crate::tasks) fn review_queue(&self, context: Option<&ContextName>) -> ReviewQueue {
        let mut entries = Vec::new();
        for (name, root) in &self.stores {
            if context.is_none_or(|context| context == name) {
                entries.extend(read_queue(root).iter().map(|entry| entry.to_api(name)));
            }
        }
        ReviewQueue { entries }
    }
}

/// What a poll knows of the MR of `update` for its entry: from its read, else from its `stored`
/// entry, else from the `previous` mirror of an archived task. `None` without any of them.
fn entry_seen(
    update: &MrUpdate,
    previous: &Previous,
    stored: Option<&QueueEntry>,
    tracked: bool,
    archived: bool,
) -> Option<EntrySeen> {
    let of_mr = |mr: &Mr| (mr.state, mr.version.head_sha.clone());
    let (state, head) = update
        .mr
        .as_ref()
        .map(of_mr)
        // An entry is of an MR that was open at its last read.
        .or_else(|| stored.map(|entry| (MrState::Open, entry.head_sha.clone())))
        .or_else(|| previous.mr.as_ref().map(of_mr))?;
    let recorded = previous.state.as_ref();
    let verdict = update.verdict.clone().map_or_else(
        || recorded.and_then(|state| state.verdict.clone()),
        |latest| {
            let shown = recorded.is_some_and(|state| state.shown);
            next_verdict(
                recorded.and_then(|state| state.verdict.as_ref()),
                shown,
                latest,
            )
            .0
        },
    );
    // Unknown (an incomplete search): as at the last poll.
    let requested = update
        .requested
        .or_else(|| stored.map(|_| true))
        .or_else(|| recorded.map(|state| state.requested))
        .unwrap_or(false);
    Some(EntrySeen {
        tracked,
        has_entry: stored.is_some(),
        state,
        verdict_on_head: verdict.is_some_and(|verdict| verdict.sha == head),
        head,
        requested,
        own: update.own,
        archived: archived.then(|| ArchivedSeen {
            head: previous.mr.as_ref().map(|mr| mr.version.head_sha.clone()),
            requested: recorded.map(|state| state.requested),
        }),
    })
}

/// The entry of `key` in the store at `root`; what cannot be read is missing.
#[must_use]
pub fn read_entry(root: &Path, key: &MrKey) -> Option<QueueEntry> {
    read_entry_bytes(&fs::read(queue_dir(root).join(entry_name(key))).ok()?)
}

/// The entries of the store at `root`, sorted by key. A file that cannot be read is logged and
/// left out.
#[must_use]
pub fn read_queue(root: &Path) -> Vec<QueueEntry> {
    let dir = queue_dir(root);
    let Ok(files) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut entries: Vec<QueueEntry> = files
        .flatten()
        .filter(|file| file.file_name().to_string_lossy().ends_with(".json"))
        .filter_map(|file| {
            let entry = fs::read(file.path())
                .ok()
                .and_then(|bytes| read_entry_bytes(&bytes));
            if entry.is_none() {
                warn!(path = %file.path().display(), "cannot read a queue entry, left out");
            }
            entry
        })
        .collect();
    entries.sort_by_key(|entry| entry.key.to_string());
    entries
}
