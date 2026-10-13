//! Reading a repository's git files without starting git (ADR-0008): the `.git` file pointer,
//! `commondir`, the remotes of the config and `HEAD`. Parsing is pure; the readers only open
//! these few files, never the object database.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gix_config::file::Metadata;
use gix_config::file::init::Options;
use gix_config::{File, Source};

use super::Head;

/// Git directory of a `.git` file: the path after `gitdir:`, relative to `dot_git_dir` (the
/// directory holding the `.git` file) when not absolute. Pure.
pub(super) fn parse_gitdir_file(content: &str, dot_git_dir: &Path) -> Result<PathBuf, String> {
    let first = content.lines().next().unwrap_or_default();
    let target = first
        .strip_prefix("gitdir:")
        .map(str::trim)
        .ok_or_else(|| "expected `gitdir: <path>` on the first line".to_owned())?;
    if target.is_empty() {
        return Err("`gitdir:` is empty".to_owned());
    }
    Ok(dot_git_dir.join(target))
}

/// The common directory of `git_dir`: the content of its `commondir` file, relative to
/// `git_dir` when not absolute, for a linked worktree; `git_dir` itself otherwise. Pure.
pub(super) fn common_dir(git_dir: &Path, commondir_file: Option<&str>) -> PathBuf {
    commondir_file
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map_or_else(|| git_dir.to_path_buf(), |path| git_dir.join(path))
}

/// `HEAD` of a git directory: a branch for `ref: refs/heads/<name>`, any other symbolic ref by
/// its full name, or a detached commit id (40 or 64 hexadecimal digits). Pure.
pub(super) fn parse_head(content: &str) -> Option<Head> {
    let content = content.trim_end();
    if let Some(target) = content.strip_prefix("ref:") {
        let target = target.trim();
        if target.is_empty() {
            return None;
        }
        return Some(target.strip_prefix("refs/heads/").map_or_else(
            || Head::Symbolic(target.to_owned()),
            |branch| Head::Branch(branch.to_owned()),
        ));
    }
    let is_id = matches!(content.len(), 40 | 64) && content.bytes().all(|b| b.is_ascii_hexdigit());
    is_id.then(|| Head::Detached(content.to_ascii_lowercase()))
}

/// Name and URL of every `[remote "<name>"]` with a `url`, in configuration order. A remote
/// spread over several sections keeps the position and the first `url` of its first one,
/// the URL git fetches from. Includes (`[include]`) are not followed. Pure.
pub fn parse_remotes(config: &[u8]) -> Result<Vec<(String, String)>, String> {
    let file =
        File::from_bytes_no_includes(config, Metadata::from(Source::Local), Options::default())
            .map_err(|error| error.to_string())?;
    let mut remotes: Vec<(String, String)> = Vec::new();
    for section in file.sections_by_name("remote").into_iter().flatten() {
        let Some(name) = section.header().subsection_name() else {
            continue;
        };
        let name = name.to_string();
        if remotes.iter().any(|(known, _)| *known == name) {
            continue;
        }
        if let Some(url) = section.values("url").first() {
            remotes.push((name, url.to_string()));
        }
    }
    Ok(remotes)
}

/// Content of `path`, `None` when it does not exist.
pub fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{Head, common_dir, parse_gitdir_file, parse_head, parse_remotes};
    use std::path::{Path, PathBuf};

    #[test]
    fn gitdir_file_is_relative_to_its_directory() {
        let dir = Path::new("/r/sub");
        assert_eq!(
            parse_gitdir_file("gitdir: ../.git/modules/sub\n", dir),
            Ok(PathBuf::from("/r/sub/../.git/modules/sub"))
        );
        assert_eq!(
            parse_gitdir_file("gitdir: /r/.git/worktrees/wt", dir),
            Ok(PathBuf::from("/r/.git/worktrees/wt"))
        );
        assert!(parse_gitdir_file("gitdir:   \n", dir).is_err());
        assert!(parse_gitdir_file("", dir).is_err());
        assert!(parse_gitdir_file("ref: refs/heads/main", dir).is_err());
    }

    #[test]
    fn common_dir_follows_commondir() {
        let git_dir = Path::new("/r/.git/worktrees/wt");
        assert_eq!(common_dir(git_dir, None), git_dir);
        assert_eq!(common_dir(git_dir, Some("  \n")), git_dir);
        assert_eq!(
            common_dir(git_dir, Some("../..\n")),
            Path::new("/r/.git/worktrees/wt/../..")
        );
        assert_eq!(
            common_dir(git_dir, Some("/elsewhere")),
            Path::new("/elsewhere")
        );
    }

    #[test]
    fn head_cases() {
        let id = "0123456789abcdef0123456789abcdef01234567";
        for (content, expected) in [
            (
                "ref: refs/heads/main\n",
                Some(Head::Branch("main".to_owned())),
            ),
            (
                "ref:refs/heads/feat/x",
                Some(Head::Branch("feat/x".to_owned())),
            ),
            (
                "ref: refs/remotes/o/x\n",
                Some(Head::Symbolic("refs/remotes/o/x".to_owned())),
            ),
            (&format!("{id}\n"), Some(Head::Detached(id.to_owned()))),
            (
                &id.to_ascii_uppercase(),
                Some(Head::Detached(id.to_owned())),
            ),
            ("ref: \n", None),
            ("0123", None),
            ("", None),
        ] {
            assert_eq!(parse_head(content), expected, "{content:?}");
        }
    }

    #[test]
    fn remotes_in_configuration_order() {
        let config = br#"
[core]
    bare = false
[remote "upstream"]
    url = https://github.com/other/loom.git
    fetch = +refs/heads/*:refs/remotes/upstream/*
[remote "origin"]
    url = git@github.com:gsuquet/loom.git
    url = git@backup.example:loom.git
[remote "push-only"]
    pushurl = git@github.com:gsuquet/loom.git
[remote "upstream"]
    url = https://ignored.example/loom.git
[remote]
    url = https://ignored.example/no-name.git
[branch "main"]
    remote = origin
"#;
        assert_eq!(
            parse_remotes(config),
            Ok(vec![
                (
                    "upstream".to_owned(),
                    "https://github.com/other/loom.git".to_owned()
                ),
                (
                    "origin".to_owned(),
                    "git@github.com:gsuquet/loom.git".to_owned()
                ),
            ])
        );
        assert_eq!(parse_remotes(b""), Ok(vec![]));
        assert!(parse_remotes(b"[remote \"x\"\n url = y").is_err());
    }
}
