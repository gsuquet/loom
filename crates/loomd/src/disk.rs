//! Disk usage of the loom home: where each entry is counted, the totals, the free space and
//! when a measure is reused, decided without I/O. The walk is in `main.rs` (`disk_usage`).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::time::{Duration, Instant, SystemTime};

use loom_api::{ContextDiskUsage, DiskUsage, Timestamp};

/// How long a measure is reused before the next status request measures again.
pub const CACHE_TTL: Duration = Duration::from_mins(5);

/// Unit of `st_blocks` (POSIX `stat`: 512-byte blocks, on Linux and macOS alike).
pub const BLOCK_BYTES: u64 = 512;

/// Part of the loom home an entry is counted in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bucket {
    /// `versions/`.
    Versions,
    /// `index.db` and `index.db-shm`.
    Index,
    /// `index.db-wal`.
    IndexWal,
    /// `log/`.
    Logs,
    /// `state/`.
    State,
    /// A directory under `contexts/`, and everything under it.
    Context(String),
    /// Anything else.
    Other,
}

/// Bucket of an entry of the loom home.
///
/// `top` is the first component of its path relative to the home, `context` the second one
/// when `top` is `contexts` and that second component is a directory. `contexts/` itself and the files directly in it are [`Bucket::Other`].
#[must_use]
pub fn bucket(top: &OsStr, context: Option<&OsStr>) -> Bucket {
    match (top.to_str(), context) {
        (Some("versions"), _) => Bucket::Versions,
        (Some("index.db" | "index.db-shm"), _) => Bucket::Index,
        (Some("index.db-wal"), _) => Bucket::IndexWal,
        (Some("log"), _) => Bucket::Logs,
        (Some("state"), _) => Bucket::State,
        (Some("contexts"), Some(name)) => Bucket::Context(name.to_string_lossy().into_owned()),
        _ => Bucket::Other,
    }
}

/// Allocated bytes of an entry from its `st_blocks`.
#[must_use]
pub const fn allocated(blocks: u64) -> u64 {
    blocks.saturating_mul(BLOCK_BYTES)
}

/// Bytes an unprivileged process can still write, from `statvfs`: `f_bavail` blocks of
/// `f_frsize` bytes.
#[must_use]
pub const fn available(bavail: u64, frsize: u64) -> u64 {
    bavail.saturating_mul(frsize)
}

/// Allocated bytes per bucket, added up during a walk.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    versions: u64,
    index: u64,
    index_wal: u64,
    logs: u64,
    state: u64,
    other: u64,
    contexts: BTreeMap<String, u64>,
}

impl Tally {
    /// Counts `bytes` in `bucket`.
    pub fn add(&mut self, bucket: Bucket, bytes: u64) {
        let slot = match bucket {
            Bucket::Versions => &mut self.versions,
            Bucket::Index => &mut self.index,
            Bucket::IndexWal => &mut self.index_wal,
            Bucket::Logs => &mut self.logs,
            Bucket::State => &mut self.state,
            Bucket::Other => &mut self.other,
            Bucket::Context(name) => self.contexts.entry(name).or_default(),
        };
        *slot = slot.saturating_add(bytes);
    }

    /// The usage: `total_bytes` adds up every bucket, `index_bytes` includes the WAL, contexts
    /// and kept versions are sorted by name.
    #[must_use]
    pub fn finish(
        self,
        computed_at: Timestamp,
        mut kept_versions: Vec<String>,
        available_bytes: Option<u64>,
        warnings: Vec<String>,
    ) -> DiskUsage {
        kept_versions.sort();
        let index_bytes = self.index.saturating_add(self.index_wal);
        let contexts: Vec<ContextDiskUsage> = self
            .contexts
            .into_iter()
            .map(|(name, bytes)| ContextDiskUsage { name, bytes })
            .collect();
        let total_bytes = [
            self.versions,
            index_bytes,
            self.logs,
            self.state,
            self.other,
        ]
        .into_iter()
        .chain(contexts.iter().map(|context| context.bytes))
        .fold(0, u64::saturating_add);
        DiskUsage {
            computed_at,
            total_bytes,
            versions_bytes: self.versions,
            index_bytes,
            index_wal_bytes: self.index_wal,
            logs_bytes: self.logs,
            state_bytes: self.state,
            other_bytes: self.other,
            contexts,
            kept_versions,
            available_bytes,
            warnings,
        }
    }
}

/// When a measure was taken, and what `versions/` looked like then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    /// Instant of the measure.
    pub measured: Instant,
    /// Modification time of `versions/` at the measure; `None` when it could not be read.
    pub versions_modified: Option<SystemTime>,
}

/// Whether a measure taken at `stamp` can be returned at `now`.
///
/// It must be younger than [`CACHE_TTL`], and `versions/` not changed since (an upgrade, a rollback or a pruning adds or removes a
/// directory in it, which changes its modification time).
#[must_use]
pub fn is_fresh(stamp: &Stamp, now: Instant, versions_modified: Option<SystemTime>) -> bool {
    now.saturating_duration_since(stamp.measured) < CACHE_TTL
        && stamp.versions_modified == versions_modified
}

#[cfg(test)]
mod tests {
    use super::{Bucket, CACHE_TTL, Stamp, Tally, allocated, available, bucket, is_fresh};
    use loom_api::{ContextDiskUsage, Timestamp};
    use std::ffi::OsStr;
    use std::time::{Duration, Instant, SystemTime};

    fn os(text: &str) -> &OsStr {
        OsStr::new(text)
    }

    #[test]
    fn top_level_entries_have_their_bucket() {
        assert_eq!(bucket(os("versions"), None), Bucket::Versions);
        assert_eq!(bucket(os("index.db"), None), Bucket::Index);
        assert_eq!(bucket(os("index.db-shm"), None), Bucket::Index);
        assert_eq!(bucket(os("index.db-wal"), None), Bucket::IndexWal);
        assert_eq!(bucket(os("log"), None), Bucket::Logs);
        assert_eq!(bucket(os("state"), None), Bucket::State);
        for other in [
            "config.toml",
            "run",
            "staging",
            "knowledge",
            "index.db.bak",
            "x",
        ] {
            assert_eq!(bucket(os(other), None), Bucket::Other, "{other}");
        }
    }

    #[test]
    fn context_stores_are_the_directories_under_contexts() {
        assert_eq!(
            bucket(os("contexts"), Some(os("personal"))),
            Bucket::Context("personal".to_owned())
        );
        // `contexts/` itself, and a file directly in it.
        assert_eq!(bucket(os("contexts"), None), Bucket::Other);
        // Only `contexts` has stores.
        assert_eq!(bucket(os("log"), Some(os("personal"))), Bucket::Logs);
    }

    #[test]
    fn allocated_bytes_are_512_byte_blocks() {
        assert_eq!(allocated(0), 0);
        assert_eq!(allocated(8), 4_096);
        assert_eq!(allocated(u64::MAX), u64::MAX);
    }

    #[test]
    fn available_bytes_are_free_blocks_for_unprivileged_users() {
        assert_eq!(available(1_000, 4_096), 4_096_000);
        assert_eq!(available(0, 4_096), 0);
        assert_eq!(available(u64::MAX, 4_096), u64::MAX);
    }

    #[test]
    fn totals_add_every_bucket_once() {
        let mut tally = Tally::default();
        tally.add(Bucket::Versions, 30);
        tally.add(Bucket::Versions, 5);
        tally.add(Bucket::Index, 4);
        tally.add(Bucket::IndexWal, 2);
        tally.add(Bucket::Logs, 1);
        tally.add(Bucket::State, 3);
        tally.add(Bucket::Other, 7);
        tally.add(Bucket::Context("work".to_owned()), 10);
        tally.add(Bucket::Context("personal".to_owned()), 20);
        tally.add(Bucket::Context("work".to_owned()), 1);
        let at = Timestamp::from_unix_secs(1_000).unwrap();
        let usage = tally.finish(
            at,
            vec!["0.2.0".to_owned(), "0.1.0".to_owned()],
            Some(9),
            vec!["w".to_owned()],
        );
        assert_eq!(usage.computed_at, at);
        assert_eq!(usage.versions_bytes, 35);
        assert_eq!(usage.index_bytes, 6, "the WAL is part of the index");
        assert_eq!(usage.index_wal_bytes, 2);
        assert_eq!(usage.logs_bytes, 1);
        assert_eq!(usage.state_bytes, 3);
        assert_eq!(usage.other_bytes, 7);
        assert_eq!(
            usage.contexts,
            vec![
                ContextDiskUsage {
                    name: "personal".to_owned(),
                    bytes: 20
                },
                ContextDiskUsage {
                    name: "work".to_owned(),
                    bytes: 11
                },
            ]
        );
        assert_eq!(usage.total_bytes, 35 + 6 + 1 + 3 + 7 + 31);
        assert_eq!(usage.kept_versions, ["0.1.0", "0.2.0"]);
        assert_eq!(usage.available_bytes, Some(9));
        assert_eq!(usage.warnings, ["w"]);
    }

    #[test]
    fn an_empty_home_is_zero_without_contexts() {
        let usage = Tally::default().finish(Timestamp::EPOCH, vec![], None, vec![]);
        assert_eq!(usage.total_bytes, 0);
        assert_eq!(usage.contexts, []);
        assert_eq!(usage.available_bytes, None);
    }

    #[test]
    fn totals_saturate() {
        let mut tally = Tally::default();
        tally.add(Bucket::Versions, u64::MAX);
        tally.add(Bucket::Versions, 1);
        tally.add(Bucket::Other, 1);
        let usage = tally.finish(Timestamp::EPOCH, vec![], None, vec![]);
        assert_eq!(usage.versions_bytes, u64::MAX);
        assert_eq!(usage.total_bytes, u64::MAX);
    }

    #[test]
    fn a_measure_is_reused_for_five_minutes() {
        let measured = Instant::now();
        let modified = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(10));
        let stamp = Stamp {
            measured,
            versions_modified: modified,
        };
        assert!(is_fresh(&stamp, measured, modified));
        assert!(is_fresh(
            &stamp,
            measured + Duration::from_secs(299),
            modified
        ));
        assert!(!is_fresh(&stamp, measured + CACHE_TTL, modified));
        // A clock that goes back never makes a measure stale.
        assert!(is_fresh(
            &Stamp {
                measured: measured + Duration::from_secs(1),
                ..stamp
            },
            measured,
            modified
        ));
    }

    #[test]
    fn a_change_of_the_kept_versions_invalidates_the_measure() {
        let measured = Instant::now();
        let before = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(10));
        let after = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(20));
        let stamp = Stamp {
            measured,
            versions_modified: before,
        };
        assert!(!is_fresh(&stamp, measured, after));
        // `versions/` created or deleted.
        assert!(!is_fresh(&stamp, measured, None));
        assert!(!is_fresh(
            &Stamp {
                versions_modified: None,
                ..stamp
            },
            measured,
            before
        ));
    }
}
