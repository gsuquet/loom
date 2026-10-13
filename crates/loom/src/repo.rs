//! `loom repo`: resolution of a `<repo>` argument, order and rendering of the index, decided
//! without I/O. `main.rs` and its modules query loomd and the filesystem.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use loom_api::{
    ContextAssignment, MaybeKnown, RepoHead, RepoState, RepoSummary, ScanResult, UNASSIGNED,
    Untracked, Upstream,
};
use thiserror::Error;

/// A `<repo>` argument, classified before the index is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoArg {
    /// No argument: the repository of the current directory.
    Here,
    /// A path: absolute, relative (starting with `.`), or `~`. Absolute and normalized.
    Path(PathBuf),
    /// `<context>/<name>`: a slug when one matches exactly, otherwise a relative path.
    SlugOrPath {
        /// The argument.
        slug: String,
        /// The argument as a path, absolute and normalized.
        path: PathBuf,
    },
    /// A bare name: the repositories with this name, otherwise a relative path.
    Name {
        /// The argument.
        name: String,
        /// The argument as a path, absolute and normalized.
        path: PathBuf,
    },
}

/// Why a `<repo>` argument names no single repository.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveError {
    /// `~` without a home directory.
    #[error("cannot expand `~` in `{0}`: HOME is unset or empty")]
    NoHome(String),
    /// Several repositories have the name, and the context does not tell them apart.
    #[error("`{name}` names several repositories; give one of:{}", candidate_lines(.candidates))]
    Ambiguous {
        /// The name given.
        name: String,
        /// `(slug, path)` of each repository with the name.
        candidates: Vec<(String, PathBuf)>,
    },
}

fn candidate_lines(candidates: &[(String, PathBuf)]) -> String {
    candidates
        .iter()
        .fold(String::new(), |mut text, (slug, path)| {
            let _ = write!(text, "\n  {slug} ({})", path.display());
            text
        })
}

/// Classifies `arg`: a path when it starts with `/`, `.` or `~`; a slug or a path when it
/// holds a `/`; a name otherwise. Relative paths are taken from `cwd`. Pure.
///
/// # Errors
///
/// [`ResolveError::NoHome`] for a `~` path without `home`.
pub fn parse_arg(
    arg: Option<&str>,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<RepoArg, ResolveError> {
    let Some(arg) = arg else {
        return Ok(RepoArg::Here);
    };
    if arg == "~" || arg.starts_with("~/") {
        let home = home.ok_or_else(|| ResolveError::NoHome(arg.to_owned()))?;
        let rest = arg.trim_start_matches('~').trim_start_matches('/');
        return Ok(RepoArg::Path(normalize(&home.join(rest))));
    }
    let path = normalize(&cwd.join(arg));
    if arg.starts_with('/') || arg.starts_with('.') {
        return Ok(RepoArg::Path(path));
    }
    if arg.contains('/') {
        return Ok(RepoArg::SlugOrPath {
            slug: arg.trim_end_matches('/').to_owned(),
            path,
        });
    }
    Ok(RepoArg::Name {
        name: arg.to_owned(),
        path,
    })
}

/// `path` without `.` components, `..` removing the previous component (as a shell's logical
/// `cd` does), and without a trailing `/`. Pure.
#[must_use]
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// What a `<repo>` argument designates once the index is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection<'a> {
    /// The repository of the current directory.
    Here,
    /// A repository of the index.
    Repo(&'a RepoSummary),
    /// A path, whose repository root is still to be found.
    Path {
        /// Absolute, normalized.
        path: PathBuf,
        /// The slug or name that matched no repository, when the argument was one.
        unmatched: Option<String>,
    },
}

/// The repository `arg` designates among `repos`. A slug matches exactly. A name matching
/// several repositories is narrowed by `contexts` in order (the first context holding one of
/// them decides). Pure.
///
/// # Errors
///
/// [`ResolveError::Ambiguous`] when no context narrows a name to one repository.
pub fn select<'a>(
    arg: &RepoArg,
    repos: &'a [RepoSummary],
    contexts: &[String],
) -> Result<Selection<'a>, ResolveError> {
    match arg {
        RepoArg::Here => Ok(Selection::Here),
        RepoArg::Path(path) => Ok(Selection::Path {
            path: path.clone(),
            unmatched: None,
        }),
        RepoArg::SlugOrPath { slug, path } => Ok(repos
            .iter()
            .find(|repo| repo.identity.slug == *slug)
            .map_or_else(
                || Selection::Path {
                    path: path.clone(),
                    unmatched: Some(slug.clone()),
                },
                Selection::Repo,
            )),
        RepoArg::Name { name, path } => {
            let named: Vec<&RepoSummary> = repos
                .iter()
                .filter(|repo| repo.identity.name == *name)
                .collect();
            match named.as_slice() {
                [] => Ok(Selection::Path {
                    path: path.clone(),
                    unmatched: Some(name.clone()),
                }),
                [one] => Ok(Selection::Repo(one)),
                several => {
                    for context in contexts {
                        let inside: Vec<&&RepoSummary> = several
                            .iter()
                            .filter(|repo| repo.identity.context.context_name() == context)
                            .collect();
                        match inside.as_slice() {
                            [] => {}
                            [one] => return Ok(Selection::Repo(one)),
                            _ => return Err(ambiguous(name, inside.into_iter().copied())),
                        }
                    }
                    Err(ambiguous(name, several.iter().copied()))
                }
            }
        }
    }
}

fn ambiguous<'a>(name: &str, repos: impl Iterator<Item = &'a RepoSummary>) -> ResolveError {
    let mut candidates: Vec<&RepoSummary> = repos.collect();
    candidates.sort_by(|a, b| order(a, b));
    ResolveError::Ambiguous {
        name: name.to_owned(),
        candidates: candidates
            .into_iter()
            .map(|repo| (repo.identity.slug.clone(), repo.identity.path.clone()))
            .collect(),
    }
}

/// Contexts that disambiguate a name, in order: `-c`, then the context of the current
/// directory, that is the context of its repository when the index has it, otherwise the
/// context its path gets from `match.paths`. Pure.
#[must_use]
pub fn disambiguation_contexts(
    flag: Option<&str>,
    cwd_repo: Option<&RepoSummary>,
    cwd_rule: Option<&ContextAssignment>,
) -> Vec<String> {
    let mut contexts: Vec<String> = flag.map(str::to_owned).into_iter().collect();
    let here = match (cwd_repo, cwd_rule) {
        (Some(repo), _) => Some(repo.identity.context.context_name().to_owned()),
        (None, Some(ContextAssignment::Path { context, .. })) => Some(context.to_string()),
        (
            None,
            Some(
                ContextAssignment::Override { .. }
                | ContextAssignment::Host { .. }
                | ContextAssignment::Default { .. }
                | ContextAssignment::Unassigned,
            )
            | None,
        ) => None,
    };
    if let Some(here) = here
        && !contexts.contains(&here)
    {
        contexts.push(here);
    }
    contexts
}

/// The repository of the index at `root`: the one with this path, otherwise one with the same
/// directory name whose `canonical` path is the canonical path of `root` (the same directory
/// reached through other symbolic links). `canonical` resolves symbolic links. Pure.
pub fn find_by_path<'a>(
    repos: &'a [RepoSummary],
    root: &Path,
    canonical: impl Fn(&Path) -> Option<PathBuf>,
) -> Option<&'a RepoSummary> {
    if let Some(repo) = repos.iter().find(|repo| repo.identity.path == root) {
        return Some(repo);
    }
    let target = canonical(root)?;
    repos
        .iter()
        .filter(|repo| repo.identity.path.file_name() == root.file_name())
        .find(|repo| canonical(&repo.identity.path).as_ref() == Some(&target))
}

/// Order of `loom repo ls`: by context, then name, then slug.
#[must_use]
pub fn order(a: &RepoSummary, b: &RepoSummary) -> Ordering {
    let key = |repo: &RepoSummary| {
        (
            repo.identity.context.context_name().to_owned(),
            repo.identity.name.clone(),
            repo.identity.slug.clone(),
        )
    };
    key(a).cmp(&key(b))
}

/// Sorts `repos` in the order of `loom repo ls`.
pub fn sort(repos: &mut [RepoSummary]) {
    repos.sort_by(order);
}

/// Whether the repository has no context.
#[must_use]
pub const fn is_unassigned(repo: &RepoSummary) -> bool {
    matches!(repo.identity.context, ContextAssignment::Unassigned)
}

/// `loom repo ls --format paths`: one absolute path per line.
#[must_use]
pub fn render_paths(repos: &[RepoSummary]) -> String {
    repos.iter().fold(String::new(), |mut text, repo| {
        let _ = writeln!(text, "{}", repo.identity.path.display());
        text
    })
}

/// `loom repo ls`: a header, then one row per repository: a `!` marking an unassigned
/// repository, slug, `HEAD`, a summary of the state, path. Columns are aligned; the path is
/// last. Nothing when there is no repository.
#[must_use]
pub fn render_table(repos: &[RepoSummary]) -> String {
    if repos.is_empty() {
        return String::new();
    }
    let rows: Vec<[String; 5]> = std::iter::once([
        " ".to_owned(),
        "SLUG".to_owned(),
        "HEAD".to_owned(),
        "STATE".to_owned(),
        "PATH".to_owned(),
    ])
    .chain(repos.iter().map(|repo| {
        [
            if is_unassigned(repo) { "!" } else { " " }.to_owned(),
            repo.identity.slug.clone(),
            repo.state
                .as_ref()
                .map_or_else(|| "?".to_owned(), |state| head_text(&state.head)),
            state_summary(repo.state.as_ref()),
            repo.identity.path.display().to_string(),
        ]
    }))
    .collect();
    let width = |column: usize| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1), width(2), width(3)];
    rows.iter().fold(String::new(), |mut text, row| {
        let _ = writeln!(
            text,
            "{:<w0$} {:<w1$}  {:<w2$}  {:<w3$}  {}",
            row[0],
            row[1],
            row[2],
            row[3],
            row[4],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
        );
        text
    })
}

/// `HEAD` in one word or two: the branch, `<branch> (unborn)`, `detached <commit>`, or
/// `unknown`.
#[must_use]
pub fn head_text(head: &MaybeKnown<RepoHead>) -> String {
    match head {
        MaybeKnown::Known(RepoHead::Branch { name }) => name.clone(),
        MaybeKnown::Known(RepoHead::Unborn { name }) => format!("{name} (unborn)"),
        MaybeKnown::Known(RepoHead::Detached { commit }) => format!("detached {commit}"),
        MaybeKnown::Unknown => "unknown".to_owned(),
    }
}

/// The state in a few words: operation in progress, changed and untracked files, ahead and
/// behind the upstream; `clean` when there is nothing to report, `not read yet` without a
/// state, `unknown` when none of them is known. An unknown field is reported as unknown,
/// never as clean.
#[must_use]
pub fn state_summary(state: Option<&RepoState>) -> String {
    let Some(state) = state else {
        return "not read yet".to_owned();
    };
    if matches!(
        (
            &state.operation,
            &state.dirty,
            &state.untracked,
            &state.upstream
        ),
        (
            MaybeKnown::Unknown,
            MaybeKnown::Unknown,
            MaybeKnown::Unknown,
            MaybeKnown::Unknown
        )
    ) {
        return "unknown".to_owned();
    }
    let mut parts: Vec<String> = Vec::new();
    match &state.operation {
        MaybeKnown::Known(Some(operation)) => parts.push(operation.to_string()),
        MaybeKnown::Known(None) => {}
        MaybeKnown::Unknown => parts.push("operation unknown".to_owned()),
    }
    match state.dirty {
        MaybeKnown::Known(0) => {}
        MaybeKnown::Known(count) => parts.push(format!("{count} changed")),
        MaybeKnown::Unknown => parts.push("changes unknown".to_owned()),
    }
    match state.untracked {
        MaybeKnown::Known(Untracked { count: 0, .. }) => {}
        MaybeKnown::Known(untracked) => {
            parts.push(format!("{} untracked", untracked_count(untracked)));
        }
        MaybeKnown::Unknown => parts.push("untracked unknown".to_owned()),
    }
    match &state.upstream {
        MaybeKnown::Known(Some(upstream)) => {
            for (word, count) in [("ahead", upstream.ahead), ("behind", upstream.behind)] {
                match count {
                    MaybeKnown::Known(0) => {}
                    MaybeKnown::Known(count) => parts.push(format!("{word} {count}")),
                    MaybeKnown::Unknown => parts.push(format!("{word} unknown")),
                }
            }
        }
        MaybeKnown::Known(None) => {}
        MaybeKnown::Unknown => parts.push("upstream unknown".to_owned()),
    }
    if parts.is_empty() {
        "clean".to_owned()
    } else {
        parts.join(", ")
    }
}

fn untracked_count(untracked: Untracked) -> String {
    if untracked.capped {
        format!("{}+", untracked.count)
    } else {
        untracked.count.to_string()
    }
}

/// The context and the rule that chose it: `personal (match.paths "~/perso")`.
#[must_use]
pub fn context_text(assignment: &ContextAssignment) -> String {
    match assignment {
        ContextAssignment::Override { context } => {
            format!("{context} ([repos.\"<path>\"] context)")
        }
        ContextAssignment::Host { context, pattern } => {
            format!("{context} (match.hosts \"{pattern}\")")
        }
        ContextAssignment::Path { context, prefix } => {
            format!("{context} (match.paths \"{prefix}\")")
        }
        ContextAssignment::Default { context } => format!("{context} (default_context)"),
        ContextAssignment::Unassigned => format!("{UNASSIGNED} (no rule matches)"),
    }
}

/// `loom repo show`: one `field: value` line per field of the repository and of its state,
/// named as in the JSON output. Unknown state fields read `unknown`.
#[must_use]
pub fn render_card(repo: &RepoSummary) -> String {
    let identity = &repo.identity;
    let mut fields: Vec<(&str, String)> = vec![
        ("slug", identity.slug.clone()),
        ("path", identity.path.display().to_string()),
        ("context", context_text(&identity.context)),
    ];
    if identity.remotes.is_empty() {
        fields.push(("remotes", "none".to_owned()));
    }
    for (index, remote) in identity.remotes.iter().enumerate() {
        let primary = if identity.primary_remote.as_deref() == Some(remote.name.as_str()) {
            " (primary)"
        } else {
            ""
        };
        let name = if index == 0 { "remotes" } else { "" };
        fields.push((name, format!("{} {}{primary}", remote.name, remote.url)));
    }
    fields.push((
        "forge",
        identity
            .forge
            .map_or_else(|| "none".to_owned(), |forge| forge.to_string()),
    ));
    fields.push((
        "registered",
        if repo.registered { "yes" } else { "no" }.to_owned(),
    ));
    fields.push((
        "last_used",
        repo.last_used
            .map_or_else(|| "never".to_owned(), |at| at.to_string()),
    ));
    match &repo.state {
        None => fields.push(("state", "not read yet".to_owned())),
        Some(state) => fields.extend(state_fields(state)),
    }
    fields
        .iter()
        .fold(String::new(), |mut text, (name, value)| {
            let label = if name.is_empty() {
                String::new()
            } else {
                format!("{name}:")
            };
            let _ = writeln!(text, "{label:<16}{value}");
            text
        })
}

fn state_fields(state: &RepoState) -> Vec<(&'static str, String)> {
    let known = |value: Option<String>| value.unwrap_or_else(|| "unknown".to_owned());
    let mut fields = vec![
        ("head", head_text(&state.head)),
        (
            "dirty",
            known(state.dirty.known().map(|count| count.to_string())),
        ),
        (
            "untracked",
            known(state.untracked.known().map(untracked_count)),
        ),
        (
            "operation",
            known(state.operation.known().map(|operation| {
                operation.map_or_else(|| "none".to_owned(), |operation| operation.to_string())
            })),
        ),
        (
            "upstream",
            known(state.upstream.as_known().map(|upstream| {
                upstream
                    .as_ref()
                    .map_or_else(|| "none".to_owned(), upstream_text)
            })),
        ),
        (
            "default_branch",
            known(
                state
                    .default_branch
                    .as_known()
                    .map(|branch| branch.clone().unwrap_or_else(|| "none".to_owned())),
            ),
        ),
        (
            "last_commit",
            known(
                state
                    .last_commit
                    .known()
                    .map(|at| at.map_or_else(|| "none".to_owned(), |at| at.to_string())),
            ),
        ),
        (
            "last_fetch",
            known(
                state
                    .last_fetch
                    .known()
                    .map(|at| at.map_or_else(|| "never".to_owned(), |at| at.to_string())),
            ),
        ),
        ("refreshed_at", state.refreshed_at.to_string()),
    ];
    if let Some(error) = &state.error {
        fields.push(("error", error.clone()));
    }
    fields
}

fn upstream_text(upstream: &Upstream) -> String {
    let count = |count: MaybeKnown<u64>| {
        count
            .known()
            .map_or_else(|| "unknown".to_owned(), |count| count.to_string())
    };
    format!(
        "{}, ahead {}, behind {}",
        upstream.name,
        count(upstream.ahead),
        count(upstream.behind)
    )
}

/// `loom repo scan`: the counts, then one line per path that could not be read.
#[must_use]
pub fn render_scan(result: &ScanResult, dry_run: bool) -> String {
    let mut text = if dry_run {
        format!(
            "would add {}, update {}, remove {}\n",
            result.added, result.updated, result.removed
        )
    } else {
        format!(
            "{} added, {} updated, {} removed in {} ms\n",
            result.added, result.updated, result.removed, result.duration_ms
        )
    };
    for issue in &result.errors {
        let _ = writeln!(
            text,
            "cannot read {}: {}",
            issue.path.display(),
            issue.message
        );
    }
    text
}

/// The registered repositories a rebuild drops for good: those outside every root (a rebuild
/// only walks the roots). Pure.
#[must_use]
pub fn lost_on_rebuild<'a>(repos: &'a [RepoSummary], roots: &[PathBuf]) -> Vec<&'a RepoSummary> {
    repos
        .iter()
        .filter(|repo| {
            repo.registered
                && !roots
                    .iter()
                    .any(|root| repo.identity.path.starts_with(root))
        })
        .collect()
}

/// A slug part as one URL path segment: every byte outside the unreserved set
/// (`A-Z a-z 0-9 - . _ ~`) percent-encoded.
#[must_use]
pub fn encode_segment(segment: &str) -> String {
    segment.bytes().fold(String::new(), |mut text, byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            text.push(char::from(byte));
        } else {
            let _ = write!(text, "%{byte:02X}");
        }
        text
    })
}

/// API path of the repository with `slug` (`<context>/<name>`).
#[must_use]
pub fn repo_api_path(slug: &str) -> String {
    let (context, name) = slug.split_once('/').unwrap_or((slug, ""));
    format!(
        "/v1/repos/{}/{}",
        encode_segment(context),
        encode_segment(name)
    )
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use loom_api::{
        ContextAssignment, MaybeKnown, RepoHead, RepoIdentity, RepoOperation, RepoState,
        RepoSummary, ScanIssue, ScanResult, Timestamp, Untracked, Upstream,
    };

    use super::{
        RepoArg, ResolveError, Selection, disambiguation_contexts, encode_segment, find_by_path,
        lost_on_rebuild, normalize, parse_arg, render_card, render_paths, render_scan,
        render_table, repo_api_path, select, sort, state_summary,
    };

    const CWD: &str = "/home/me/src";

    fn assignment(context: &str) -> ContextAssignment {
        if context == "unassigned" {
            ContextAssignment::Unassigned
        } else {
            ContextAssignment::Path {
                context: context.parse().unwrap(),
                prefix: format!("~/{context}"),
            }
        }
    }

    fn repo(slug: &str, path: &str) -> RepoSummary {
        let (context, name) = slug.split_once('/').unwrap();
        // `loom-2` is a second `loom`.
        let name = name.split_once("-2").map_or(name, |(base, _)| base);
        RepoSummary {
            identity: RepoIdentity {
                path: PathBuf::from(path),
                name: name.to_owned(),
                slug: slug.to_owned(),
                context: assignment(context),
                remotes: vec![],
                primary_remote: None,
                forge: None,
            },
            registered: false,
            last_used: None,
            state: None,
        }
    }

    fn index() -> Vec<RepoSummary> {
        vec![
            repo("personal/loom", "/home/me/perso/loom"),
            repo("work/loom", "/home/me/work/loom"),
            repo("work/loom-2", "/home/me/work/b/loom"),
            repo("personal/dotfiles", "/home/me/perso/dotfiles"),
            repo("unassigned/tool", "/opt/tool"),
        ]
    }

    fn parse(arg: Option<&str>) -> RepoArg {
        parse_arg(arg, Path::new(CWD), Some(Path::new("/home/me"))).unwrap()
    }

    #[test]
    fn arguments_are_classified() {
        let cases: Vec<(Option<&str>, RepoArg)> = vec![
            (None, RepoArg::Here),
            (Some("/abs/x/"), RepoArg::Path("/abs/x".into())),
            (Some("."), RepoArg::Path(CWD.into())),
            (Some("./a/../b"), RepoArg::Path("/home/me/src/b".into())),
            (Some("../x"), RepoArg::Path("/home/me/x".into())),
            (
                Some(".hidden"),
                RepoArg::Path("/home/me/src/.hidden".into()),
            ),
            (Some("~"), RepoArg::Path("/home/me".into())),
            (
                Some("~/perso/loom"),
                RepoArg::Path("/home/me/perso/loom".into()),
            ),
            (
                Some("work/loom"),
                RepoArg::SlugOrPath {
                    slug: "work/loom".to_owned(),
                    path: "/home/me/src/work/loom".into(),
                },
            ),
            (
                Some("loom"),
                RepoArg::Name {
                    name: "loom".to_owned(),
                    path: "/home/me/src/loom".into(),
                },
            ),
        ];
        for (arg, expected) in cases {
            assert_eq!(parse(arg), expected, "{arg:?}");
        }
        assert_eq!(
            parse_arg(Some("~/x"), Path::new(CWD), None),
            Err(ResolveError::NoHome("~/x".to_owned()))
        );
    }

    #[test]
    fn normalize_is_lexical() {
        for (path, expected) in [
            ("/a/./b/", "/a/b"),
            ("/a/b/../c", "/a/c"),
            ("/../a", "/a"),
            ("/", "/"),
        ] {
            assert_eq!(
                normalize(Path::new(path)),
                PathBuf::from(expected),
                "{path}"
            );
        }
    }

    /// Slug of the selected repository, or what else was selected.
    fn selected(arg: &str, contexts: &[&str]) -> Result<String, ResolveError> {
        let repos = index();
        let contexts: Vec<String> = contexts.iter().map(|c| (*c).to_owned()).collect();
        select(&parse(Some(arg)), &repos, &contexts).map(|selection| match selection {
            Selection::Repo(repo) => repo.identity.slug.clone(),
            Selection::Here => "here".to_owned(),
            Selection::Path { path, unmatched } => {
                format!("path {} {unmatched:?}", path.display())
            }
        })
    }

    #[test]
    fn selection_table() {
        let ok = |text: &str| Ok(text.to_owned());
        let cases: Vec<(&str, &[&str], Result<String, ResolveError>)> = vec![
            // Exact slug.
            ("work/loom-2", &[], ok("work/loom-2")),
            ("unassigned/tool", &[], ok("unassigned/tool")),
            // A slug that matches nothing is a relative path.
            (
                "work/nope",
                &[],
                ok("path /home/me/src/work/nope Some(\"work/nope\")"),
            ),
            // Unique name.
            ("dotfiles", &[], ok("personal/dotfiles")),
            ("tool", &["work"], ok("unassigned/tool")),
            // Unknown name: a relative path.
            ("nope", &[], ok("path /home/me/src/nope Some(\"nope\")")),
            // A path is never matched against names.
            ("./loom", &[], ok("path /home/me/src/loom None")),
            // Ambiguous name narrowed by a context.
            ("loom", &["personal"], ok("personal/loom")),
            // The first context with a match decides.
            ("loom", &["other", "personal", "work"], ok("personal/loom")),
        ];
        for (arg, contexts, expected) in cases {
            assert_eq!(selected(arg, contexts), expected, "{arg} {contexts:?}");
        }
    }

    #[test]
    fn ambiguous_names_list_the_candidates() {
        let error = selected("loom", &[]).unwrap_err();
        assert_eq!(
            error,
            ResolveError::Ambiguous {
                name: "loom".to_owned(),
                candidates: vec![
                    ("personal/loom".to_owned(), "/home/me/perso/loom".into()),
                    ("work/loom".to_owned(), "/home/me/work/loom".into()),
                    ("work/loom-2".to_owned(), "/home/me/work/b/loom".into()),
                ],
            }
        );
        assert_eq!(
            error.to_string(),
            "`loom` names several repositories; give one of:\n  personal/loom (/home/me/perso/loom)\n  work/loom (/home/me/work/loom)\n  work/loom-2 (/home/me/work/b/loom)"
        );
        // Two in the context: still ambiguous, between those two only.
        let ResolveError::Ambiguous { candidates, .. } = selected("loom", &["work"]).unwrap_err()
        else {
            panic!("not ambiguous");
        };
        assert_eq!(candidates.len(), 2);
        // A context without any of them does not help.
        assert!(selected("loom", &["other"]).is_err());
    }

    #[test]
    fn context_of_the_current_directory() {
        let repos = index();
        let rule = assignment("work");
        assert_eq!(
            disambiguation_contexts(Some("personal"), Some(&repos[1]), Some(&rule)),
            ["personal", "work"]
        );
        assert_eq!(
            disambiguation_contexts(None, Some(&repos[4]), Some(&rule)),
            ["unassigned"]
        );
        assert_eq!(disambiguation_contexts(None, None, Some(&rule)), ["work"]);
        assert_eq!(
            disambiguation_contexts(Some("work"), None, Some(&rule)),
            ["work"]
        );
        // Only `match.paths` describes a directory.
        let default = ContextAssignment::Default {
            context: "personal".parse().unwrap(),
        };
        assert!(disambiguation_contexts(None, None, Some(&default)).is_empty());
        assert!(disambiguation_contexts(None, None, None).is_empty());
    }

    #[test]
    fn find_by_path_falls_back_to_canonical_paths() {
        let repos = index();
        let canonical = |path: &Path| {
            Some(PathBuf::from(
                path.to_str()
                    .unwrap()
                    .replace("/private/home/me", "/home/me"),
            ))
        };
        let found = |root: &str| {
            find_by_path(&repos, Path::new(root), canonical).map(|repo| repo.identity.slug.clone())
        };
        assert_eq!(
            found("/home/me/perso/loom").as_deref(),
            Some("personal/loom")
        );
        assert_eq!(
            found("/private/home/me/work/loom").as_deref(),
            Some("work/loom")
        );
        assert_eq!(found("/home/me/perso/other"), None);
        assert_eq!(find_by_path(&repos, Path::new("/x/loom"), |_| None), None);
    }

    #[test]
    fn listing_is_sorted_by_context_then_name() {
        let mut repos = vec![
            repo("work/b", "/w/b"),
            repo("a-b/z", "/x/z"),
            repo("unassigned/c", "/c"),
            repo("a/y", "/y"),
            repo("work/a", "/w/a"),
        ];
        sort(&mut repos);
        let slugs: Vec<&str> = repos.iter().map(|r| r.identity.slug.as_str()).collect();
        // `a` before `a-b`: by context, not by slug (`a-b/` sorts before `a/`).
        assert_eq!(slugs, ["a/y", "a-b/z", "unassigned/c", "work/a", "work/b"]);
        assert_eq!(render_paths(&repos), "/y\n/x/z\n/c\n/w/a\n/w/b\n");
    }

    fn state() -> RepoState {
        RepoState {
            refreshed_at: Timestamp::from_unix_secs(1_791_792_000).unwrap(),
            head: MaybeKnown::Known(RepoHead::Branch {
                name: "main".to_owned(),
            }),
            dirty: MaybeKnown::Known(0),
            untracked: MaybeKnown::Known(Untracked {
                count: 0,
                capped: false,
            }),
            operation: MaybeKnown::Known(None),
            upstream: MaybeKnown::Known(None),
            default_branch: MaybeKnown::Known(Some("main".to_owned())),
            last_commit: MaybeKnown::Known(None),
            last_fetch: MaybeKnown::Known(None),
            error: None,
        }
    }

    #[test]
    fn state_summaries() {
        assert_eq!(state_summary(None), "not read yet");
        assert_eq!(state_summary(Some(&state())), "clean");
        let busy = RepoState {
            operation: MaybeKnown::Known(Some(RepoOperation::Rebase)),
            dirty: MaybeKnown::Known(2),
            untracked: MaybeKnown::Known(Untracked {
                count: 1000,
                capped: true,
            }),
            upstream: MaybeKnown::Known(Some(Upstream {
                name: "origin/main".to_owned(),
                ahead: MaybeKnown::Known(1),
                behind: MaybeKnown::Unknown,
            })),
            ..state()
        };
        assert_eq!(
            state_summary(Some(&busy)),
            "rebase, 2 changed, 1000+ untracked, ahead 1, behind unknown"
        );
        let unknown = RepoState {
            head: MaybeKnown::Unknown,
            dirty: MaybeKnown::Unknown,
            untracked: MaybeKnown::Unknown,
            operation: MaybeKnown::Unknown,
            upstream: MaybeKnown::Unknown,
            ..state()
        };
        assert_eq!(state_summary(Some(&unknown)), "unknown");
        let partly = RepoState {
            operation: MaybeKnown::Known(None),
            ..unknown
        };
        assert_eq!(
            state_summary(Some(&partly)),
            "changes unknown, untracked unknown, upstream unknown"
        );
    }

    #[test]
    fn table_flags_unassigned_repositories() {
        let mut repos = vec![repo("personal/loom", "/p/loom"), repo("unassigned/x", "/x")];
        repos[0].state = Some(state());
        assert_eq!(
            render_table(&repos),
            "  SLUG           HEAD  STATE         PATH\n  \
             personal/loom  main  clean         /p/loom\n\
             ! unassigned/x   ?     not read yet  /x\n"
        );
        assert_eq!(render_table(&[]), "");
    }

    #[test]
    fn card_shows_unknown_fields_as_unknown() {
        let mut summary = repo("personal/loom", "/p/loom");
        assert!(render_card(&summary).contains("state:          not read yet\n"));
        summary.state = Some(RepoState {
            dirty: MaybeKnown::Unknown,
            upstream: MaybeKnown::Unknown,
            error: Some("dirty: time budget of 2s exceeded".to_owned()),
            ..state()
        });
        let card = render_card(&summary);
        for line in [
            "slug:           personal/loom\n",
            "context:        personal (match.paths \"~/personal\")\n",
            "remotes:        none\n",
            "registered:     no\n",
            "last_used:      never\n",
            "head:           main\n",
            "dirty:          unknown\n",
            "untracked:      0\n",
            "operation:      none\n",
            "upstream:       unknown\n",
            "default_branch: main\n",
            "last_commit:    none\n",
            "last_fetch:     never\n",
            "refreshed_at:   2026-10-12T08:00:00Z\n",
            "error:          dirty: time budget of 2s exceeded\n",
        ] {
            assert!(card.contains(line), "{line:?} not in\n{card}");
        }
    }

    #[test]
    fn scan_output() {
        let result = ScanResult {
            added: 2,
            updated: 1,
            removed: 0,
            errors: vec![ScanIssue {
                path: "/w/locked".into(),
                message: "permission denied".to_owned(),
            }],
            duration_ms: 42,
        };
        assert_eq!(
            render_scan(&result, false),
            "2 added, 1 updated, 0 removed in 42 ms\ncannot read /w/locked: permission denied\n"
        );
        assert!(render_scan(&result, true).starts_with("would add 2, update 1, remove 0\n"));
    }

    #[test]
    fn rebuild_loses_registered_repositories_outside_the_roots() {
        let mut repos = index();
        repos[0].registered = true;
        repos[4].registered = true;
        let roots = [PathBuf::from("/home/me/perso")];
        let lost: Vec<&str> = lost_on_rebuild(&repos, &roots)
            .iter()
            .map(|repo| repo.identity.slug.as_str())
            .collect();
        assert_eq!(lost, ["unassigned/tool"]);
    }

    #[test]
    fn slugs_are_percent_encoded_per_segment() {
        assert_eq!(encode_segment("a b%/é"), "a%20b%25%2F%C3%A9");
        assert_eq!(
            repo_api_path("personal/loom-2"),
            "/v1/repos/personal/loom-2"
        );
        assert_eq!(repo_api_path("work/my repo"), "/v1/repos/work/my%20repo");
    }
}
