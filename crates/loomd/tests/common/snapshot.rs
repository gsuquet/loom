//! Everything local work is made of in a repository, read with git before and after an
//! operation, and the comparison of two such snapshots against the differences an update is
//! allowed to make.
//!
//! A snapshot holds every ref with its object, `HEAD` (symbolic or detached), the index, every
//! file of the working tree (tracked, untracked and ignored, `.git` excluded at every depth),
//! the stash list, the markers of an operation in progress in the git directory, and the
//! configuration files. `FETCH_HEAD`, `ORIG_HEAD` and the reflogs are left out: an update may
//! change them. File contents are compared by git blob id (`git hash-object --no-filters`, so
//! that no filter driver ever runs); a symbolic link is compared by its target.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Debug, Write as _};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Stdio;

use super::{git, git_command, git_run};

/// Files and directories of the git directory that mark an operation in progress.
const MARKERS: [&str; 16] = [
    "rebase-merge",
    "rebase-apply",
    "MERGE_HEAD",
    "MERGE_MSG",
    "MERGE_MODE",
    "MERGE_RR",
    "AUTO_MERGE",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "sequencer",
    "BISECT_LOG",
    "BISECT_START",
    "BISECT_TERMS",
    "BISECT_EXPECTED_REV",
    "BISECT_ANCESTORS_OK",
    "BISECT_NAMES",
];

/// Mode of a gitlink (a submodule's commit) in a tree.
const GITLINK: &str = "160000";

/// A file of a tree or of the working tree: mode and blob id.
#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    pub mode: String,
    pub id: String,
}

impl Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.mode, self.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// `refs/heads/<branch>` when `HEAD` is symbolic, `detached` otherwise.
    pub head: String,
    /// Commit of `HEAD`, `None` on an unborn branch.
    pub head_id: Option<String>,
    /// Every ref (`git for-each-ref`): its object, and its target when symbolic.
    pub refs: BTreeMap<String, String>,
    /// `git ls-files --stage`, one entry per line (conflict stages included).
    pub index: BTreeSet<String>,
    /// The tree of `HEAD`, recursively: blobs and gitlinks.
    pub head_tree: BTreeMap<String, Entry>,
    /// Every file of the working tree, `.git` excluded.
    pub files: BTreeMap<String, Entry>,
    /// `git stash list`, with commit ids.
    pub stash: Vec<String>,
    /// Markers of an operation in progress: path in the git directory, blob id of its content.
    pub markers: BTreeMap<String, String>,
    /// `config` of the common git directory and `config.worktree`, when present.
    pub config: BTreeMap<String, String>,
    /// Whether `FETCH_HEAD` exists: a fetch ran.
    pub fetch_head: bool,
}

impl Snapshot {
    /// The snapshot of the working tree `repo`, read with git run with `HOME` set to `home`.
    pub fn take(home: &Path, repo: &Path) -> Self {
        let symbolic = git_run(home, repo, &["symbolic-ref", "-q", "HEAD"]);
        let head = if symbolic.status.success() {
            String::from_utf8(symbolic.stdout)
                .unwrap()
                .trim()
                .to_owned()
        } else {
            "detached".to_owned()
        };
        let head_id = git_run(
            home,
            repo,
            &["rev-parse", "-q", "--verify", "HEAD^{commit}"],
        );
        let head_id = head_id
            .status
            .success()
            .then(|| String::from_utf8(head_id.stdout).unwrap().trim().to_owned());
        let refs = git(
            home,
            repo,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname) %(symref)",
            ],
        )
        .lines()
        .map(|line| {
            let mut parts = line.splitn(3, ' ');
            let name = parts.next().unwrap().to_owned();
            let id = parts.next().unwrap_or_default();
            let value = match parts.next().unwrap_or_default() {
                "" => id.to_owned(),
                target => format!("{id} -> {target}"),
            };
            (name, value)
        })
        .collect();
        let index = git(home, repo, &["ls-files", "--stage", "-z"])
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .map(str::to_owned)
            .collect();
        let head_tree = head_id
            .as_ref()
            .map(|_| tree(home, repo, "HEAD"))
            .unwrap_or_default();
        let git_dir = git(home, repo, &["rev-parse", "--absolute-git-dir"]);
        let git_dir = Path::new(git_dir.trim());
        let common_dir = git(home, repo, &["rev-parse", "--git-common-dir"]);
        let common_dir = repo.join(common_dir.trim());
        Self {
            head,
            head_id,
            refs,
            index,
            head_tree,
            files: work_tree(home, repo),
            stash: git(home, repo, &["stash", "list", "--format=%H %gs"])
                .lines()
                .map(str::to_owned)
                .collect(),
            markers: markers(home, git_dir),
            config: [
                ("config", common_dir.join("config")),
                ("config.worktree", git_dir.join("config.worktree")),
            ]
            .into_iter()
            .filter_map(|(name, path)| {
                let bytes = fs::read(path).ok()?;
                Some((
                    name.to_owned(),
                    String::from_utf8_lossy(&bytes).into_owned(),
                ))
            })
            .collect(),
            fetch_head: git_dir.join("FETCH_HEAD").exists(),
        }
    }

    /// The commit of `name` (a full ref name), without the target of a symbolic ref.
    pub fn commit_of(&self, name: &str) -> Option<&str> {
        self.refs
            .get(name)
            .and_then(|value| value.split(' ').next())
    }
}

/// The blobs and gitlinks of `rev`'s tree, recursively.
fn tree(home: &Path, repo: &Path, rev: &str) -> BTreeMap<String, Entry> {
    git(home, repo, &["ls-tree", "-r", "-z", rev])
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (meta, path) = entry.split_once('\t').unwrap();
            let mut meta = meta.split(' ');
            let mode = meta.next().unwrap().to_owned();
            let _kind = meta.next().unwrap();
            let id = meta.next().unwrap().to_owned();
            (path.to_owned(), Entry { mode, id })
        })
        .collect()
}

/// Blob ids of `paths` (relative to `dir` or absolute), contents taken as they are on disk.
fn hash_files(home: &Path, dir: &Path, paths: &[String]) -> Vec<String> {
    if paths.is_empty() {
        return Vec::new();
    }
    let mut child = git_command(home, dir)
        .args(["hash-object", "--no-filters", "--stdin-paths"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for path in paths {
        writeln!(input, "{path}").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git hash-object in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let ids: Vec<String> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(ids.len(), paths.len());
    ids
}

/// Every file under `repo`, `.git` excluded at every depth, symbolic links not followed.
fn work_tree(home: &Path, repo: &Path) -> BTreeMap<String, Entry> {
    let mut regular = Vec::new();
    let mut files = BTreeMap::new();
    walk(repo, repo, &mut |path, relative, metadata| {
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(path).unwrap();
            files.insert(
                relative,
                Entry {
                    mode: "120000".to_owned(),
                    id: format!("link to {}", target.display()),
                },
            );
        } else {
            let mode = if metadata.permissions().mode() & 0o111 == 0 {
                "100644"
            } else {
                "100755"
            };
            regular.push((relative, mode));
        }
    });
    let paths: Vec<String> = regular.iter().map(|(path, _)| path.clone()).collect();
    for ((path, mode), id) in regular.into_iter().zip(hash_files(home, repo, &paths)) {
        files.insert(
            path,
            Entry {
                mode: mode.to_owned(),
                id,
            },
        );
    }
    files
}

/// Calls `visit` with each file (not directory) under `dir`, with its path relative to `root`.
fn walk(root: &Path, dir: &Path, visit: &mut dyn FnMut(&Path, String, &fs::Metadata)) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).unwrap();
        if metadata.is_dir() {
            walk(root, &path, visit);
        } else {
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            visit(&path, relative, &metadata);
        }
    }
}

/// The markers of an operation in progress in `git_dir`, with the blob id of each file.
fn markers(home: &Path, git_dir: &Path) -> BTreeMap<String, String> {
    let mut found = Vec::new();
    for name in MARKERS {
        let path = git_dir.join(name);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            walk(git_dir, &path, &mut |_, relative, _| found.push(relative));
            // An empty directory still marks the operation.
            found.push(format!("{name}/"));
        } else {
            found.push(name.to_owned());
        }
    }
    let files: Vec<String> = found
        .iter()
        .filter(|path| !path.ends_with('/'))
        .map(|path| git_dir.join(path).to_string_lossy().into_owned())
        .collect();
    let mut ids = hash_files(home, git_dir, &files).into_iter();
    found
        .into_iter()
        .map(|path| {
            let id = if path.ends_with('/') {
                "directory".to_owned()
            } else {
                ids.next().unwrap()
            };
            (path, id)
        })
        .collect()
}

/// A fast-forward an update may make: `branch` (`HEAD`'s, full name) to the commit of
/// `upstream` (full name of a remote-tracking ref) as read after the update.
#[derive(Debug, Clone, Copy)]
pub struct FastForward<'a> {
    pub branch: &'a str,
    pub upstream: &'a str,
}

/// The differences an update may make, and only these.
#[derive(Debug, Clone, Copy, Default)]
pub struct Allowed<'a> {
    /// Remote-tracking refs may change and `FETCH_HEAD` appear.
    pub fetch: bool,
    /// The branch `HEAD` is on may be fast-forwarded: the branch, `HEAD`, the index and the
    /// tracked files then match the upstream commit exactly, and every other file is kept.
    pub fast_forward: Option<FastForward<'a>>,
}

/// Every difference between `before` and `after` that `allowed` does not permit, one line
/// each, precise enough to see what was lost. Empty when the update kept everything.
pub fn compare(before: &Snapshot, after: &Snapshot, allowed: Allowed<'_>) -> Vec<String> {
    let mut diffs = Vec::new();
    if before.head != after.head {
        diffs.push(format!("HEAD: {} -> {}", before.head, after.head));
    }
    // Fast-forwarded: `HEAD` stayed on the branch, and its commit changed.
    let moved = allowed
        .fast_forward
        .filter(|ff| {
            before.head == ff.branch && after.head == ff.branch && before.head_id != after.head_id
        })
        .and_then(|ff| after.commit_of(ff.upstream).map(|id| (ff, id)));
    diff_refs(before, after, allowed.fetch, moved, &mut diffs);
    let expected_head = moved.map_or(before.head_id.as_deref(), |(_, id)| Some(id));
    if after.head_id.as_deref() != expected_head {
        diffs.push(format!(
            "HEAD commit: {:?} -> {:?}, expected {expected_head:?}",
            before.head_id, after.head_id
        ));
    }
    if !allowed.fetch && before.fetch_head != after.fetch_head {
        diffs.push(format!(
            "FETCH_HEAD: {} -> {}",
            exists(before.fetch_head),
            exists(after.fetch_head)
        ));
    }
    if moved.is_some() {
        diff_fast_forwarded(before, after, &mut diffs);
    } else {
        diff_sets("index", &before.index, &after.index, &mut diffs);
        diff_maps("work tree file", &before.files, &after.files, &mut diffs);
    }
    if before.stash != after.stash {
        diffs.push(format!("stash: {:?} -> {:?}", before.stash, after.stash));
    }
    diff_maps(
        "operation marker",
        &before.markers,
        &after.markers,
        &mut diffs,
    );
    diff_maps("config file", &before.config, &after.config, &mut diffs);
    diffs
}

/// Refs that changed, but for remote-tracking refs when `fetch`, and the fast-forwarded branch
/// when it points at the upstream commit.
fn diff_refs(
    before: &Snapshot,
    after: &Snapshot,
    fetch: bool,
    moved: Option<(FastForward<'_>, &str)>,
    diffs: &mut Vec<String>,
) {
    let names: BTreeSet<&String> = before.refs.keys().chain(after.refs.keys()).collect();
    for name in names {
        if fetch && name.starts_with("refs/remotes/") {
            continue;
        }
        let (old, new) = (before.refs.get(name), after.refs.get(name));
        match moved {
            Some((ff, id)) if name == ff.branch => {
                if after.commit_of(name) != Some(id) {
                    diffs.push(format!(
                        "ref {name}: moved to {new:?}, not to {} ({id})",
                        ff.upstream
                    ));
                }
            }
            _ if old != new => diffs.push(format!("ref {name}: {old:?} -> {new:?}")),
            _ => {}
        }
    }
}

/// The index and the work tree after a fast-forward: the tracked content is exactly the new
/// tree, which the fast-forward may only replace when it was exactly the old one, and every
/// other file is kept.
fn diff_fast_forwarded(before: &Snapshot, after: &Snapshot, diffs: &mut Vec<String>) {
    let index_of = |tree: &BTreeMap<String, Entry>| -> BTreeSet<String> {
        tree.iter()
            .map(|(path, entry)| format!("{} {} 0\t{path}", entry.mode, entry.id))
            .collect()
    };
    diff_sets(
        "index before the fast-forward, against HEAD",
        &index_of(&before.head_tree),
        &before.index,
        diffs,
    );
    diff_sets(
        "index after the fast-forward, against the upstream commit",
        &index_of(&after.head_tree),
        &after.index,
        diffs,
    );
    let blobs = |tree: &BTreeMap<String, Entry>| -> BTreeMap<String, Entry> {
        tree.iter()
            .filter(|(_, entry)| entry.mode != GITLINK)
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect()
    };
    let tracked_before = blobs(&before.head_tree);
    let (on_disk_before, mut expected): (BTreeMap<_, _>, BTreeMap<_, _>) = before
        .files
        .iter()
        .map(|(path, entry)| (path.clone(), entry.clone()))
        .partition(|(path, _)| tracked_before.contains_key(path));
    diff_maps(
        "tracked file before the fast-forward, against HEAD",
        &tracked_before,
        &on_disk_before,
        diffs,
    );
    expected.extend(blobs(&after.head_tree));
    diff_maps("work tree file", &expected, &after.files, diffs);
}

const fn exists(present: bool) -> &'static str {
    if present { "present" } else { "absent" }
}

/// One line per key added, removed or changed.
fn diff_maps<V: PartialEq + Debug>(
    label: &str,
    before: &BTreeMap<String, V>,
    after: &BTreeMap<String, V>,
    diffs: &mut Vec<String>,
) {
    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for key in keys {
        match (before.get(key), after.get(key)) {
            (Some(old), Some(new)) if old != new => {
                diffs.push(format!("{label} {key}: {old:?} -> {new:?}"));
            }
            (Some(old), None) => diffs.push(format!("{label} {key}: removed (was {old:?})")),
            (None, Some(new)) => diffs.push(format!("{label} {key}: added ({new:?})")),
            _ => {}
        }
    }
}

/// One line per element only in `before` (`-`) or only in `after` (`+`).
fn diff_sets(
    label: &str,
    before: &BTreeSet<String>,
    after: &BTreeSet<String>,
    diffs: &mut Vec<String>,
) {
    let mut lines = String::new();
    for removed in before.difference(after) {
        let _ = write!(lines, "\n    - {removed}");
    }
    for added in after.difference(before) {
        let _ = write!(lines, "\n    + {added}");
    }
    if !lines.is_empty() {
        diffs.push(format!("{label}:{lines}"));
    }
}
