//! Discovery of the repositories under the configured roots (ADR-0012).
//!
//! The only module of the crate with I/O: it lists directories and reads a few git files of
//! each repository (`.git` file, `commondir`, `config`, `HEAD`), never starting git
//! (ADR-0008).

mod git;

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, FileType};
use std::io;
use std::path::{Path, PathBuf};

use globset::{GlobSet, GlobSetBuilder};
use loom_api::RepoIdentity;
use loom_config::config::{Config, Spanned};
use loom_config::repos::{ExcludePattern, ForgeConfig, ForgeHost};
use thiserror::Error;

use crate::{ContextRules, identify};

/// Directory names never walked, besides hidden directories (ADR-0012).
pub const BUILTIN_EXCLUDES: [&str; 8] = [
    "node_modules",
    "vendor",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
];

/// Directories not walked: the built-in names, hidden directories and `[repos] exclude`.
///
/// A pattern without `/` matches a directory name at any depth (`archive`, `*-old`); a pattern
/// with `/` matches the directory path relative to its root, with `/` separators
/// (`clients/old`, `**/fixtures/repos`).
#[derive(Debug, Clone)]
pub struct Exclusions {
    names: GlobSet,
    paths: GlobSet,
}

/// The `[repos] exclude` patterns cannot be combined.
#[derive(Debug, Error)]
#[error("invalid [repos] exclude patterns: {0}")]
pub struct ExclusionsError(#[from] globset::Error);

impl Exclusions {
    /// The exclusions of `patterns` (`[repos] exclude`).
    ///
    /// # Errors
    ///
    /// [`ExclusionsError`] when the patterns cannot be compiled together.
    pub fn new(patterns: &[ExcludePattern]) -> Result<Self, ExclusionsError> {
        let mut names = GlobSetBuilder::new();
        let mut paths = GlobSetBuilder::new();
        for pattern in patterns {
            let glob = pattern.glob().clone();
            if glob.glob().contains('/') {
                paths.add(glob);
            } else {
                names.add(glob);
            }
        }
        Ok(Self {
            names: names.build()?,
            paths: paths.build()?,
        })
    }

    /// Whether the directory `name`, at `relative` below its root, is not walked.
    fn excludes(&self, name: &OsStr, relative: &Path) -> bool {
        name.as_encoded_bytes().starts_with(b".")
            || BUILTIN_EXCLUDES.iter().any(|excluded| name == *excluded)
            || self.names.is_match(name)
            || self.paths.is_match(relative)
    }
}

/// How [`scan`] walks and identifies repositories.
pub struct ScanOptions<'a> {
    /// `[repos] max_depth`: deepest level examined, the root being level 0.
    pub max_depth: u32,
    /// Directories not walked.
    pub exclusions: Exclusions,
    /// Context rules applied to each repository.
    pub rules: &'a ContextRules,
    /// `[forges]` overrides.
    pub forges: &'a BTreeMap<ForgeHost, ForgeConfig>,
    /// SSH host alias resolution (`ssh -G`), see [`crate::parse_remote_url`].
    pub resolve_alias: &'a dyn Fn(&str) -> Option<String>,
}

/// `HEAD` of a repository, as read from its git directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// `ref: refs/heads/<name>`, the branch name (the branch may have no commit yet).
    Branch(String),
    /// Another symbolic ref, by its full name.
    Symbolic(String),
    /// A detached commit id, lowercase hexadecimal.
    Detached(String),
}

/// A repository found by [`scan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedRepo {
    /// Identity, with the base slug `<context>/<name>`: [`crate::diff`] makes it unique.
    pub identity: RepoIdentity,
    /// Git directory: `<path>/.git`, or the target of the `.git` file.
    pub git_dir: PathBuf,
    /// Common directory, holding the config: the main repository's git directory for a linked
    /// worktree, the git directory otherwise.
    pub common_dir: PathBuf,
    /// `HEAD`, `None` when it cannot be read (reported in [`ScanReport::errors`]).
    pub head: Option<Head>,
}

/// A path the scan could not read. The scan goes on with the other paths.
#[derive(Debug, Error)]
pub enum ScanError {
    /// A root exists but cannot be resolved or is not a directory.
    #[error("cannot scan root {path}: {source}")]
    Root {
        /// The root, as configured.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// A directory cannot be listed (permission denied, removed during the scan…).
    #[error("cannot read directory {path}: {source}")]
    ReadDir {
        /// The directory.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// A repository's `.git` cannot be followed: unreadable, no `gitdir:` line, or a missing
    /// target. The repository is not reported.
    #[error("invalid .git in {path}: {reason}")]
    GitDir {
        /// The repository.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// A repository's config cannot be read or parsed. The repository is not reported.
    #[error("cannot read the git config {config} of {path}: {reason}")]
    Config {
        /// The repository.
        path: PathBuf,
        /// The config file.
        config: PathBuf,
        /// Why.
        reason: String,
    },
    /// A repository's `HEAD` cannot be read or parsed. The repository is reported without it.
    #[error("cannot read HEAD of {path}: {reason}")]
    Head {
        /// The repository.
        path: PathBuf,
        /// Why.
        reason: String,
    },
}

impl ScanError {
    /// The root, directory or repository the error is about.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Root { path, .. }
            | Self::ReadDir { path, .. }
            | Self::GitDir { path, .. }
            | Self::Config { path, .. }
            | Self::Head { path, .. } => path,
        }
    }

    /// Whether what is below [`Self::path`] is unknown, so that the repositories known there
    /// must be kept. Only an unreadable `HEAD` still reports its repository.
    #[must_use]
    pub const fn hides_repos(&self) -> bool {
        match self {
            Self::Root { .. }
            | Self::ReadDir { .. }
            | Self::GitDir { .. }
            | Self::Config { .. } => true,
            Self::Head { .. } => false,
        }
    }
}

/// Result of [`scan`]: repositories and errors, each sorted by path.
#[derive(Debug, Default)]
pub struct ScanReport {
    /// Repositories found.
    pub repos: Vec<ScannedRepo>,
    /// Paths that could not be read.
    pub errors: Vec<ScanError>,
}

/// Finds the repositories under `roots` (absolute, `~` expanded), never aborting on an error.
///
/// Each root is walked to `options.max_depth` levels (the root is level 0). A directory with a
/// `.git` directory or file is a repository and is not descended into. Excluded directories
/// (see [`Exclusions`]) and symbolic links are not walked. Roots that do not exist are
/// skipped. A repository under two overlapping roots is reported once, under the first root,
/// with its path as found under that root (symbolic links kept).
#[must_use]
pub fn scan(roots: &[PathBuf], options: &ScanOptions<'_>) -> ScanReport {
    let mut report = ScanReport::default();
    let mut walked_roots = HashSet::new();
    let mut found = HashSet::new();
    for root in roots {
        let canonical = match fs::canonicalize(root).and_then(|canonical| {
            if fs::metadata(&canonical)?.is_dir() {
                Ok(canonical)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "not a directory",
                ))
            }
        }) {
            Ok(canonical) => canonical,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                report.errors.push(ScanError::Root {
                    path: root.clone(),
                    source,
                });
                continue;
            }
        };
        if walked_roots.insert(canonical.clone()) {
            walk(root, &canonical, options, &mut found, &mut report);
        }
    }
    report
        .repos
        .sort_by(|a, b| a.identity.path.cmp(&b.identity.path));
    report.errors.sort_by(|a, b| a.path().cmp(b.path()));
    report
}

/// Directories [`scan`] walks for `config`: `[repos] roots`, then the `match.paths` of every
/// context, with `~` expanded to `home`, each once, in that order. Pure.
#[must_use]
pub fn scan_roots(config: &Config, home: &Path) -> Vec<PathBuf> {
    let paths = config.repos.roots.iter().chain(
        config
            .contexts
            .values()
            .flat_map(|context| context.matching.paths.iter().map(Spanned::get_ref)),
    );
    let mut roots: Vec<PathBuf> = Vec::new();
    for path in paths {
        let path = path.expand(home);
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    roots
}

/// [`scan()`] of `roots`, plus the registered repositories at `registered` (absolute repository
/// roots) the walk does not report, read in place: those deeper than `max_depth`, excluded,
/// or outside the roots.
///
/// Returns the report and the roots to give [`crate::diff`]: `roots` and every registered
/// path. A registered repository still on disk is thus re-identified (its context follows the
/// rules), one gone from disk is removed, and one that cannot be read is kept.
#[must_use]
pub fn scan_with_registered(
    roots: &[PathBuf],
    registered: &[PathBuf],
    options: &ScanOptions<'_>,
) -> (ScanReport, Vec<PathBuf>) {
    let mut report = scan(roots, options);
    let reported: HashSet<PathBuf> = report
        .repos
        .iter()
        .map(|repo| repo.identity.path.clone())
        .collect();
    let missing: Vec<PathBuf> = registered
        .iter()
        .filter(|path| !reported.contains(*path))
        .cloned()
        .collect();
    let extra = read_repos(&missing, options);
    report.repos.extend(extra.repos);
    report.errors.extend(extra.errors);
    report
        .repos
        .sort_by(|a, b| a.identity.path.cmp(&b.identity.path));
    report.errors.sort_by(|a, b| a.path().cmp(b.path()));
    let mut scanned = roots.to_vec();
    scanned.extend(registered.iter().cloned());
    (report, scanned)
}

/// Reads the repositories whose roots are `paths`, without walking.
///
/// A path without a `.git` entry (or that does not exist) is absent from the report; one
/// whose `.git` cannot be examined is reported as an error. Repositories and errors are sorted
/// by path.
#[must_use]
pub fn read_repos(paths: &[PathBuf], options: &ScanOptions<'_>) -> ScanReport {
    let mut report = ScanReport::default();
    for path in paths {
        match fs::symlink_metadata(path.join(".git")) {
            Ok(metadata) => read_repo(path, metadata.file_type(), options, &mut report),
            Err(error) if is_absent(&error) => {}
            Err(error) => report.errors.push(ScanError::GitDir {
                path: path.clone(),
                reason: error.to_string(),
            }),
        }
    }
    report
        .repos
        .sort_by(|a, b| a.identity.path.cmp(&b.identity.path));
    report.errors.sort_by(|a, b| a.path().cmp(b.path()));
    report
}

/// The root of the repository holding `path`.
///
/// That is `path` itself or its nearest ancestor with a `.git` entry (directory, file or
/// symbolic link, as [`scan()`] detects them); `None` when no ancestor has one. Ancestors are
/// taken lexically: symbolic links in `path` are kept.
///
/// # Errors
///
/// The error of the first `.git` lookup that fails for another reason than absence, such as a
/// permission denied.
pub fn find_root(path: &Path) -> io::Result<Option<PathBuf>> {
    for dir in path.ancestors() {
        match fs::symlink_metadata(dir.join(".git")) {
            Ok(_) => return Ok(Some(dir.to_path_buf())),
            Err(error) if is_absent(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

/// Whether `error` means that the path does not exist: missing, or a component that is not a
/// directory.
fn is_absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// A directory to examine: its path under the configured root, relative to the root, level.
struct Pending {
    path: PathBuf,
    relative: PathBuf,
    depth: u32,
}

/// Walks one root. `found` holds the canonical paths of the repositories already reported.
fn walk(
    root: &Path,
    canonical_root: &Path,
    options: &ScanOptions<'_>,
    found: &mut HashSet<PathBuf>,
    report: &mut ScanReport,
) {
    let mut pending = vec![Pending {
        path: root.to_path_buf(),
        relative: PathBuf::new(),
        depth: 0,
    }];
    while let Some(dir) = pending.pop() {
        let listing = match list(&dir.path) {
            Ok(listing) => listing,
            Err(source) => {
                report.errors.push(ScanError::ReadDir {
                    path: dir.path,
                    source,
                });
                continue;
            }
        };
        if let Some(dot_git) = listing.dot_git {
            // Below the root, no component is a symbolic link: this path is canonical.
            if found.insert(canonical_root.join(&dir.relative)) {
                read_repo(&dir.path, dot_git, options, report);
            }
            continue;
        }
        if dir.depth >= options.max_depth {
            continue;
        }
        for name in listing.subdirs {
            let relative = dir.relative.join(&name);
            if options.exclusions.excludes(&name, &relative) {
                continue;
            }
            pending.push(Pending {
                path: dir.path.join(&name),
                relative,
                depth: dir.depth + 1,
            });
        }
    }
}

/// What a directory holds: its `.git` entry and its subdirectories (symbolic links excluded).
struct Listing {
    dot_git: Option<FileType>,
    subdirs: Vec<std::ffi::OsString>,
}

fn list(dir: &Path) -> io::Result<Listing> {
    let mut listing = Listing {
        dot_git: None,
        subdirs: Vec::new(),
    };
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        // The type comes from the directory entry: no `stat`, symbolic links not followed.
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        if name == ".git" {
            listing.dot_git = Some(file_type);
        } else if file_type.is_dir() {
            listing.subdirs.push(name);
        }
    }
    Ok(listing)
}

/// Reads the repository at `path` and adds it, or the error that hides it, to `report`.
fn read_repo(path: &Path, dot_git: FileType, options: &ScanOptions<'_>, report: &mut ScanReport) {
    let git_dir = match git_dir(path, dot_git) {
        Ok(git_dir) => git_dir,
        Err(reason) => {
            report.errors.push(ScanError::GitDir {
                path: path.to_path_buf(),
                reason,
            });
            return;
        }
    };
    let commondir_file = match git::read_optional(&git_dir.join("commondir")) {
        Ok(content) => content.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        Err(error) => {
            report.errors.push(ScanError::GitDir {
                path: path.to_path_buf(),
                reason: format!("cannot read commondir: {error}"),
            });
            return;
        }
    };
    let common_dir = git::common_dir(&git_dir, commondir_file.as_deref());
    let config = common_dir.join("config");
    // A git directory without config is valid for git: no remote.
    let remotes = git::read_optional(&config)
        .map_err(|error| error.to_string())
        .and_then(|bytes| bytes.map_or_else(|| Ok(Vec::new()), |bytes| git::parse_remotes(&bytes)));
    let remotes = match remotes {
        Ok(remotes) => remotes,
        Err(reason) => {
            report.errors.push(ScanError::Config {
                path: path.to_path_buf(),
                config,
                reason,
            });
            return;
        }
    };
    let head = fs::read(git_dir.join("HEAD"))
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            git::parse_head(&String::from_utf8_lossy(&bytes))
                .ok_or_else(|| "neither `ref: <name>` nor a commit id".to_owned())
        });
    let head = match head {
        Ok(head) => Some(head),
        Err(reason) => {
            report.errors.push(ScanError::Head {
                path: path.to_path_buf(),
                reason,
            });
            None
        }
    };
    let identity = identify(
        path,
        &remotes,
        options.rules,
        options.forges,
        options.resolve_alias,
    );
    report.repos.push(ScannedRepo {
        identity,
        git_dir,
        common_dir,
        head,
    });
}

/// Git directory of the repository at `path` whose `.git` entry has the type `dot_git`.
fn git_dir(path: &Path, dot_git: FileType) -> Result<PathBuf, String> {
    let dot_git_path = path.join(".git");
    // A `.git` symbolic link is resolved: it names the git directory, it is not walked.
    let is_dir = if dot_git.is_symlink() {
        fs::metadata(&dot_git_path)
            .map_err(|error| format!("broken symbolic link: {error}"))?
            .is_dir()
    } else {
        dot_git.is_dir()
    };
    if is_dir {
        return Ok(dot_git_path);
    }
    let content = fs::read(&dot_git_path).map_err(|error| error.to_string())?;
    let git_dir = git::parse_gitdir_file(&String::from_utf8_lossy(&content), path)?;
    match fs::metadata(&git_dir) {
        Ok(metadata) if metadata.is_dir() => Ok(git_dir),
        Ok(_) => Err(format!("gitdir {} is not a directory", git_dir.display())),
        Err(error) => Err(format!("gitdir {}: {error}", git_dir.display())),
    }
}
