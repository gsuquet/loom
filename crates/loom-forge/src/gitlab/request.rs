//! The bodies of the GitLab REST writes of a publish: draft notes, `bulk_publish` and
//! `approve`. Pure.
//!
//! A position is in file line numbers, never diff positions. A line range carries a GitLab line
//! code for each end: `<sha1 of the path>_<old line>_<new line>`.

use serde::Serialize;

use crate::model::{MrVersion, Sha, Verdict, VerdictKind};

/// The three SHAs of a GitLab diff position: the `diff_refs` of the merge request, or the SHAs
/// of one of its versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRefs {
    /// The merge base of the head and the target branch (GitLab `base_sha`).
    pub base_sha: Sha,
    /// The tip of the target branch of the version (GitLab `start_sha`).
    pub start_sha: Sha,
    /// The head commit of the version.
    pub head_sha: Sha,
}

impl DiffRefs {
    /// The refs of `version`. `None` when the version has no merge base: GitLab always gives
    /// one, so this is a version of another forge.
    #[must_use]
    pub fn of(version: &MrVersion) -> Option<Self> {
        Some(Self {
            base_sha: version.merge_base.clone()?,
            start_sha: version.base_sha.clone(),
            head_sha: version.head_sha.clone(),
        })
    }
}

/// What a line of a hunk is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// A line that the MR adds (`+`): GitLab sends its `new_line` only.
    Added,
    /// A line that the MR removes (`-`): GitLab sends its `old_line` only.
    Removed,
    /// An unchanged line (context): GitLab sends `old_line` and `new_line`.
    Unchanged,
}

/// A line of a hunk with its numbers on both sides, as GitLab counts them.
///
/// An [`Anchor`](crate::model::Anchor) has the line of one side only. GitLab needs both for an
/// unchanged line, and both for the line code of any line: the caller reads them from the hunk
/// of the patch. For an added line, `old` is the number of the next line of the old file at
/// that point of the hunk; for a removed line, `new` is the number of the next line of the new
/// file. GitLab counts its line codes so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffLine {
    /// What the line is.
    pub kind: LineKind,
    /// The line in the old file (see the type for an added line), from 1.
    pub old: u32,
    /// The line in the new file (see the type for a removed line), from 1.
    pub new: u32,
}

impl DiffLine {
    /// The `old_line` that GitLab gets: `None` for an added line.
    const fn old_line(self) -> Option<u32> {
        match self.kind {
            LineKind::Added => None,
            LineKind::Removed | LineKind::Unchanged => Some(self.old),
        }
    }

    /// The `new_line` that GitLab gets: `None` for a removed line.
    const fn new_line(self) -> Option<u32> {
        match self.kind {
            LineKind::Removed => None,
            LineKind::Added | LineKind::Unchanged => Some(self.new),
        }
    }

    /// The `type` of an end of a `line_range`: `new` for an added line, otherwise `old`.
    const fn range_type(self) -> &'static str {
        match self.kind {
            LineKind::Added => "new",
            LineKind::Removed | LineKind::Unchanged => "old",
        }
    }
}

/// The GitLab line code of a line: `<sha1 of path>_<old>_<new>`, the SHA-1 in lowercase
/// hexadecimal. `path` is the path in the head. Pure.
#[must_use]
pub fn line_code(path: &str, old: u32, new: u32) -> String {
    // The plain SHA-1, as GitLab computes it. A detected collision attack still gives it: a
    // path is not a secret, and the line code must match GitLab's.
    let digest = sha1dc::digest(path.as_bytes()).unwrap_or_else(|collision| collision.digest());
    format!("{digest:x}_{old}_{new}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RangeEnd {
    line_code: String,
    #[serde(rename = "type")]
    kind: &'static str,
    old_line: Option<u32>,
    new_line: Option<u32>,
}

impl RangeEnd {
    fn new(path: &str, line: DiffLine) -> Self {
        Self {
            line_code: line_code(path, line.old, line.new),
            kind: line.range_type(),
            old_line: line.old_line(),
            new_line: line.new_line(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct LineRange {
    start: RangeEnd,
    end: RangeEnd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[expect(clippy::struct_field_names, reason = "the names of the GitLab API")]
struct Position {
    base_sha: Sha,
    start_sha: Sha,
    head_sha: Sha,
    old_path: String,
    new_path: String,
    position_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_range: Option<LineRange>,
}

/// The body of `POST /projects/<id>/merge_requests/<iid>/draft_notes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DraftNoteBody {
    note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<Position>,
    #[serde(skip_serializing_if = "Option::is_none")]
    in_reply_to_discussion_id: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    resolve_discussion: bool,
}

impl DraftNoteBody {
    /// A general comment on the merge request.
    #[must_use]
    pub fn general(note: &str) -> Self {
        Self {
            note: note.to_owned(),
            position: None,
            in_reply_to_discussion_id: None,
            resolve_discussion: false,
        }
    }

    /// A comment on lines `start` to `end` of one hunk of a file, in the version of `refs`.
    /// `old_path` and `new_path` are the paths of the file in the base and in the head (the
    /// same path when the file is not renamed). One line when `start` equals `end`; otherwise a
    /// `line_range` with the line code of each end, and the position on `end`.
    #[must_use]
    pub fn on_lines(
        note: &str,
        refs: &DiffRefs,
        old_path: &str,
        new_path: &str,
        start: DiffLine,
        end: DiffLine,
    ) -> Self {
        let line_range = (start != end).then(|| LineRange {
            start: RangeEnd::new(new_path, start),
            end: RangeEnd::new(new_path, end),
        });
        Self {
            note: note.to_owned(),
            position: Some(Position {
                base_sha: refs.base_sha.clone(),
                start_sha: refs.start_sha.clone(),
                head_sha: refs.head_sha.clone(),
                old_path: old_path.to_owned(),
                new_path: new_path.to_owned(),
                position_type: "text",
                old_line: end.old_line(),
                new_line: end.new_line(),
                line_range,
            }),
            in_reply_to_discussion_id: None,
            resolve_discussion: false,
        }
    }

    /// A reply to the discussion `discussion_id`; with `resolve`, its publication resolves the
    /// discussion.
    #[must_use]
    pub fn reply(note: &str, discussion_id: &str, resolve: bool) -> Self {
        Self {
            note: note.to_owned(),
            position: None,
            in_reply_to_discussion_id: Some(discussion_id.to_owned()),
            resolve_discussion: resolve,
        }
    }
}

/// The state of a reviewer that `bulk_publish` sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerState {
    /// The reviewer asks for changes.
    RequestedChanges,
    /// The reviewer commented without a decision.
    Reviewed,
}

/// The body of `POST /projects/<id>/merge_requests/<iid>/draft_notes/bulk_publish`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BulkPublishBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reviewer_state: Option<ReviewerState>,
}

impl BulkPublishBody {
    /// A publication with an optional summary `note` and reviewer state.
    #[must_use]
    pub fn new(note: Option<&str>, reviewer_state: Option<ReviewerState>) -> Self {
        Self {
            note: note.map(str::to_owned),
            reviewer_state,
        }
    }

    /// The publication of a review with `verdict`: its body as the summary note, and
    /// `requested_changes` or `reviewed` as the reviewer state. An approval sets no reviewer
    /// state: `approve` follows (design: Publish, step 4). No verdict publishes the draft notes
    /// only.
    #[must_use]
    pub fn for_verdict(verdict: Option<&Verdict>) -> Self {
        let Some(verdict) = verdict else {
            return Self::new(None, None);
        };
        let state = match verdict.kind {
            VerdictKind::Approve => None,
            VerdictKind::RequestChanges => Some(ReviewerState::RequestedChanges),
            VerdictKind::Comment => Some(ReviewerState::Reviewed),
        };
        Self::new(verdict.body.as_deref(), state)
    }
}

/// The body of `POST /projects/<id>/merge_requests/<iid>/approve`: the head that the approval
/// is for. GitLab answers 409 when it is not the head any more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApproveBody {
    sha: Sha,
}

impl ApproveBody {
    /// The approval of the head `sha`.
    #[must_use]
    pub const fn new(sha: Sha) -> Self {
        Self { sha }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        ApproveBody, BulkPublishBody, DiffLine, DiffRefs, DraftNoteBody, LineKind, ReviewerState,
        line_code,
    };
    use crate::model::{MrVersion, Sha, Verdict, VerdictKind};

    const M2: &str = "ee047f5164ae63069c7237b6f24c66d34f9eff26";
    const V2: &str = "84280c47b6aa128c645b1fbea54aded8b32e8463";
    const JWT: &str = "src/auth/jwt.rs";

    fn sha(text: &str) -> Sha {
        Sha::try_from(text.to_owned()).unwrap()
    }

    fn refs() -> DiffRefs {
        DiffRefs {
            base_sha: sha(M2),
            start_sha: sha(M2),
            head_sha: sha(V2),
        }
    }

    fn json_of(body: &impl serde::Serialize) -> Value {
        serde_json::to_value(body).unwrap()
    }

    fn position(lines: &Value) -> Value {
        let mut position = json!({
            "base_sha": M2,
            "start_sha": M2,
            "head_sha": V2,
            "old_path": JWT,
            "new_path": JWT,
            "position_type": "text",
        });
        for (key, value) in lines.as_object().unwrap() {
            position[key] = value.clone();
        }
        position
    }

    const fn line(kind: LineKind, old: u32, new: u32) -> DiffLine {
        DiffLine { kind, old, new }
    }

    #[test]
    fn line_codes_hash_the_path() {
        assert_eq!(
            line_code(JWT, 9, 9),
            "d6731b5918e52205f2871b9c60cea4aed6807584_9_9"
        );
        assert_eq!(
            line_code("", 1, 2),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709_1_2"
        );
    }

    #[test]
    fn refs_of_a_version_need_its_merge_base() {
        let mut version = MrVersion {
            head_sha: sha(V2),
            base_sha: sha(M2),
            merge_base: Some(sha(M2)),
            forge_version_id: Some(202),
            created_at: None,
        };
        assert_eq!(DiffRefs::of(&version), Some(refs()));
        version.merge_base = None;
        assert_eq!(DiffRefs::of(&version), None);
    }

    #[test]
    fn an_added_line_sends_its_new_line_only() {
        let added = line(LineKind::Added, 12, 14);
        let body = DraftNoteBody::on_lines("x", &refs(), JWT, JWT, added, added);
        assert_eq!(
            json_of(&body),
            json!({"note": "x", "position": position(&json!({"new_line": 14}))})
        );
    }

    #[test]
    fn a_removed_line_sends_its_old_line_only() {
        let removed = line(LineKind::Removed, 10, 12);
        let body = DraftNoteBody::on_lines("x", &refs(), "old.rs", JWT, removed, removed);
        let mut expected = position(&json!({"old_line": 10}));
        expected["old_path"] = "old.rs".into();
        assert_eq!(json_of(&body), json!({"note": "x", "position": expected}));
    }

    #[test]
    fn an_unchanged_line_sends_both_lines() {
        let context = line(LineKind::Unchanged, 13, 16);
        let body = DraftNoteBody::on_lines("x", &refs(), JWT, JWT, context, context);
        assert_eq!(
            json_of(&body),
            json!({"note": "x", "position": position(&json!({"old_line": 13, "new_line": 16}))})
        );
    }

    #[test]
    fn a_range_has_the_line_codes_and_types_of_its_ends() {
        let start = line(LineKind::Added, 9, 10);
        let end = line(LineKind::Removed, 11, 12);
        let body = DraftNoteBody::on_lines("x", &refs(), JWT, JWT, start, end);
        let hash = "d6731b5918e52205f2871b9c60cea4aed6807584";
        assert_eq!(
            json_of(&body)["position"],
            position(&json!({
                "old_line": 11,
                "line_range": {
                    "start": {"line_code": format!("{hash}_9_10"), "type": "new", "old_line": null, "new_line": 10},
                    "end": {"line_code": format!("{hash}_11_12"), "type": "old", "old_line": 11, "new_line": null},
                },
            }))
        );
    }

    #[test]
    fn general_comments_and_replies_have_no_position() {
        assert_eq!(
            json_of(&DraftNoteBody::general("hi")),
            json!({"note": "hi"})
        );
        assert_eq!(
            json_of(&DraftNoteBody::reply("ok", "abc", true)),
            json!({"note": "ok", "in_reply_to_discussion_id": "abc", "resolve_discussion": true})
        );
        assert_eq!(
            json_of(&DraftNoteBody::reply("ok", "abc", false)),
            json!({"note": "ok", "in_reply_to_discussion_id": "abc"})
        );
    }

    #[test]
    fn bulk_publish_bodies_of_each_verdict() {
        let verdict = |kind, body: Option<&str>| Verdict {
            kind,
            body: body.map(str::to_owned),
        };
        assert_eq!(
            json_of(&BulkPublishBody::for_verdict(Some(&verdict(
                VerdictKind::RequestChanges,
                Some("Fix it.")
            )))),
            json!({"note": "Fix it.", "reviewer_state": "requested_changes"})
        );
        assert_eq!(
            json_of(&BulkPublishBody::for_verdict(Some(&verdict(
                VerdictKind::Comment,
                None
            )))),
            json!({"reviewer_state": "reviewed"})
        );
        assert_eq!(
            json_of(&BulkPublishBody::for_verdict(Some(&verdict(
                VerdictKind::Approve,
                Some("LGTM")
            )))),
            json!({"note": "LGTM"})
        );
        assert_eq!(json_of(&BulkPublishBody::for_verdict(None)), json!({}));
        assert_eq!(
            json_of(&BulkPublishBody::new(None, Some(ReviewerState::Reviewed))),
            json!({"reviewer_state": "reviewed"})
        );
    }

    #[test]
    fn approve_sends_the_head() {
        assert_eq!(json_of(&ApproveBody::new(sha(V2))), json!({"sha": V2}));
    }
}
