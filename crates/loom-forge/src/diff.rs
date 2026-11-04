//! The diff of an MR version, per file (ADR-0016, decision 4). Pure.
//!
//! A hash of each file's normalized patch, "since last review", and the patch of a version made
//! from the forge's per-file patches when there is no clone.

use std::fmt::Write as _;

use sha2::{Digest as _, Sha256};

use crate::model::{FileChange, FileDiff};

/// The normalized hash of the diff of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHash {
    /// The path in the head.
    pub path: String,
    /// The SHA-256 of the normalized patch, in lowercase hexadecimal digits.
    pub hash: String,
}

/// The part of a patch for one file.
#[derive(Default)]
struct Section<'a> {
    header: Vec<&'a [u8]>,
    /// The lines of its hunks, without the `@@` lines.
    hunks: Vec<&'a [u8]>,
    in_hunks: bool,
}

const DIFF_GIT: &[u8] = b"diff --git ";
/// The header lines that describe a change of their own: the rest of a header (`---`, `+++`,
/// `similarity index`) repeats the path, or follows from the hunks.
const CHANGE_LINES: [&[u8]; 10] = [
    b"old mode ",
    b"new mode ",
    b"new file mode ",
    b"deleted file mode ",
    b"rename from ",
    b"rename to ",
    b"copy from ",
    b"copy to ",
    b"Binary files ",
    NO_PATCH_PREFIX,
];
/// What a reduced patch says of a file that the forge gives no patch for.
const NO_PATCH_PREFIX: &[u8] = b"# ";

/// The sections of `patch`. A line of a hunk starts with `+`, `-`, a space or `\`, so a line
/// that starts with `diff --git ` always starts a file, and `--- x` in a hunk is a removed line.
fn sections(patch: &[u8]) -> Vec<Section<'_>> {
    let mut sections: Vec<Section<'_>> = Vec::new();
    let mut lines: Vec<&[u8]> = patch.split(|byte| *byte == b'\n').collect();
    // The piece after the last newline. Not `lines()`: a `\r` is part of a changed line.
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    for line in lines {
        if line.starts_with(DIFF_GIT) {
            sections.push(Section {
                header: vec![line],
                ..Section::default()
            });
            continue;
        }
        let Some(section) = sections.last_mut() else {
            continue;
        };
        if line.starts_with(b"@@") {
            section.in_hunks = true;
        } else if section.in_hunks {
            section.hunks.push(line);
        } else {
            section.header.push(line);
        }
    }
    sections
}

/// The hash of a section: its hunks, and what its header says of the change. The blobs of the
/// `index` line count only for a file without hunks: they alone tell two binary diffs apart.
fn section_hash(section: &Section<'_>) -> String {
    let mut hasher = Sha256::new();
    let mut add = |line: &[u8]| {
        hasher.update(line);
        hasher.update(b"\n");
    };
    for line in &section.header {
        let index = section.hunks.is_empty() && line.starts_with(b"index ");
        if index || CHANGE_LINES.iter().any(|prefix| line.starts_with(prefix)) {
            add(line);
        }
    }
    for line in &section.hunks {
        // Some tools strip the space of an empty context line.
        add(if line.is_empty() { b" " } else { line });
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        // Writing to a `String` does not fail.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// A path as git writes it: C-quoted when it has special bytes.
fn unquote(path: &[u8]) -> Vec<u8> {
    let Some(inner) = path
        .strip_prefix(b"\"")
        .and_then(|rest| rest.strip_suffix(b"\""))
    else {
        return path.to_vec();
    };
    let mut out = Vec::with_capacity(inner.len());
    let mut bytes = inner.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        match bytes.next() {
            Some(b'a') => out.push(0x07),
            Some(b'b') => out.push(0x08),
            Some(b't') => out.push(b'\t'),
            Some(b'n') => out.push(b'\n'),
            Some(b'v') => out.push(0x0b),
            Some(b'f') => out.push(0x0c),
            Some(b'r') => out.push(b'\r'),
            Some(first @ b'0'..=b'7') => {
                let value = [Some(first), bytes.next(), bytes.next()]
                    .into_iter()
                    .flatten()
                    .fold(0_u8, |value, digit| {
                        value.wrapping_mul(8).wrapping_add(digit.wrapping_sub(b'0'))
                    });
                out.push(value);
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// The path after `prefix` in `line`: without the tab that git adds after a path with a space,
/// and without the `a/` or `b/` of its side.
fn path_after(line: &[u8], prefix: &[u8], side: &[u8]) -> Option<Vec<u8>> {
    let rest = line.strip_prefix(prefix)?;
    let rest = rest.strip_suffix(b"\t").unwrap_or(rest);
    if rest == b"/dev/null" {
        return None;
    }
    let path = unquote(rest);
    Some(
        path.strip_prefix(side)
            .map_or_else(|| path.clone(), <[u8]>::to_vec),
    )
}

/// The head path in `a/<path> b/<path>`, the rest of a `diff --git` line of a file without
/// `---`, `+++` or rename lines: both paths are the same, so a space in them is no problem.
fn path_of_diff_git(line: &[u8]) -> Vec<u8> {
    let rest = line.strip_prefix(DIFF_GIT).unwrap_or(line);
    if rest.ends_with(b"\"") {
        // `"a/<path>" "b/<path>"`: the second string starts after the first closing quote.
        let second = rest
            .windows(3)
            .position(|window| window == b"\" \"")
            .map_or(rest, |at| &rest[at + 2..]);
        let path = unquote(second);
        return path
            .strip_prefix(b"b/")
            .map_or_else(|| path.clone(), <[u8]>::to_vec);
    }
    // `a/` + path + ` b/` + path.
    let half = rest.len().saturating_sub(5) / 2;
    let start = half + 5;
    if rest.len() >= start && rest.get(2..2 + half) == rest.get(start..) {
        return rest[start..].to_vec();
    }
    rest.windows(3)
        .rposition(|window| window == b" b/")
        .map_or_else(|| rest.to_vec(), |at| rest[at + 3..].to_vec())
}

/// The path of a section in the head.
fn section_path(section: &Section<'_>) -> String {
    let find = |prefix: &[u8], side: &[u8]| {
        section
            .header
            .iter()
            .find_map(|line| path_after(line, prefix, side))
    };
    let path = find(b"rename to ", b"")
        .or_else(|| find(b"copy to ", b""))
        .or_else(|| find(b"+++ ", b"b/"))
        .or_else(|| find(b"--- ", b"a/"))
        .or_else(|| section.header.first().map(|line| path_of_diff_git(line)))
        .unwrap_or_default();
    String::from_utf8_lossy(&path).into_owned()
}

/// The hash of each file of `patch`, in the order of the patch. Pure.
///
/// It is the SHA-256 of the `+`, `-`, context and `\ No newline` lines of the file, without the
/// `index` line, the `@@` lines and the section text of a hunk. So a rebase that only moves the
/// lines of a file keeps its hash. A file without hunks (binary, a new mode, a rename alone) is
/// compared by what its header says.
#[must_use]
pub fn file_hashes(patch: &[u8]) -> Vec<FileHash> {
    sections(patch)
        .iter()
        .map(|section| FileHash {
            path: section_path(section),
            hash: section_hash(section),
        })
        .collect()
}

/// The files whose diff differs between the patches `reviewed` and `head`, sorted. Pure.
///
/// A file counts when it changed, is new, or left the MR. A plain
/// `git diff <old head> <new head>` is wrong after a rebase: it also shows what came from the
/// base branch.
#[must_use]
pub fn since_last_review(reviewed: &[u8], head: &[u8]) -> Vec<String> {
    let reviewed = file_hashes(reviewed);
    let head = file_hashes(head);
    let mut paths: Vec<String> = head
        .iter()
        .filter(|file| !reviewed.contains(file))
        .chain(
            reviewed
                .iter()
                .filter(|file| !head.iter().any(|other| other.path == file.path)),
        )
        .map(|file| file.path.clone())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// The patch of a version from the per-file patches of the forge. Pure.
///
/// One `diff --git` section per file, read by [`file_hashes`] as the same diff from git. A file
/// without a patch from the forge (binary or too large) has a `# ` line that says so.
#[must_use]
pub fn reduced_patch(files: &[FileDiff]) -> Vec<u8> {
    let mut out = String::new();
    for file in files {
        let path = &file.path;
        let old = file.old_path.as_deref().unwrap_or(path);
        let _ = writeln!(out, "diff --git a/{old} b/{path}");
        match file.change {
            FileChange::Renamed => {
                let _ = writeln!(out, "rename from {old}\nrename to {path}");
            }
            FileChange::Copied => {
                let _ = writeln!(out, "copy from {old}\ncopy to {path}");
            }
            FileChange::Added
            | FileChange::Modified
            | FileChange::Deleted
            | FileChange::Changed => {}
        }
        if let Some(body) = file.patch.as_deref().filter(|body| !body.is_empty()) {
            let from = match file.change {
                FileChange::Added => "/dev/null".to_owned(),
                FileChange::Modified
                | FileChange::Deleted
                | FileChange::Renamed
                | FileChange::Copied
                | FileChange::Changed => format!("a/{old}"),
            };
            let to = match file.change {
                FileChange::Deleted => "/dev/null".to_owned(),
                FileChange::Added
                | FileChange::Modified
                | FileChange::Renamed
                | FileChange::Copied
                | FileChange::Changed => format!("b/{path}"),
            };
            let _ = writeln!(out, "--- {from}\n+++ {to}");
            out.push_str(body);
            if !body.ends_with('\n') {
                out.push('\n');
            }
        } else {
            let why = if file.too_large {
                ": the file is too large"
            } else {
                ""
            };
            let _ = writeln!(out, "# {path}: the forge gives no patch{why}");
        }
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests;
