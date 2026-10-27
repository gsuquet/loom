//! GitLab REST payloads to the model. Unknown fields are ignored.
//!
//! Every parser takes the body of a response with a success status. Error bodies (4xx, 5xx)
//! go to [`parse_error`].

use loom_api::ForgeKind;
use serde::Deserialize;

use crate::error::{ErrorBody, ForgeTime, ParseError, invalid, json};
use crate::model::{
    Anchor, Comment, FileChange, FileDiff, ForgeUser, Mr, MrForgeIds, MrKey, MrState, MrSummary,
    MrVersion, Sha, Side, Thread, ThreadSubject,
};

#[derive(Deserialize)]
struct User {
    id: u64,
    username: String,
}

/// `GET /user`.
///
/// # Errors
///
/// [`ParseError`] when the body is not a GitLab user.
pub fn parse_user(body: &[u8]) -> Result<ForgeUser, ParseError> {
    let user: User = json("GitLab user", body)?;
    Ok(ForgeUser {
        id: user.id,
        login: user.username,
    })
}

/// A personal access token (`GET /personal_access_tokens/self`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInfo {
    /// The scopes, such as `api` or `read_api`.
    pub scopes: Vec<String>,
    /// False when the token is revoked or expired.
    pub active: bool,
    /// The last day of the token (`YYYY-MM-DD`), when it expires.
    pub expires_at: Option<String>,
}

#[derive(Deserialize)]
struct Token {
    scopes: Vec<String>,
    active: bool,
    expires_at: Option<String>,
}

/// `GET /personal_access_tokens/self`.
///
/// # Errors
///
/// [`ParseError`] when the body is not a GitLab personal access token.
pub fn parse_token(body: &[u8]) -> Result<TokenInfo, ParseError> {
    let token: Token = json("GitLab personal access token", body)?;
    Ok(TokenInfo {
        scopes: token.scopes,
        active: token.active,
        expires_at: token.expires_at,
    })
}

#[derive(Deserialize)]
struct References {
    full: String,
}

#[derive(Deserialize)]
#[expect(clippy::struct_field_names, reason = "the names of the GitLab API")]
struct DiffRefs {
    base_sha: Sha,
    head_sha: Sha,
    start_sha: Sha,
}

#[derive(Deserialize)]
struct MergeRequest {
    id: u64,
    iid: u64,
    project_id: u64,
    title: String,
    description: Option<String>,
    state: String,
    created_at: ForgeTime,
    updated_at: ForgeTime,
    author: Option<User>,
    #[serde(default)]
    reviewers: Vec<User>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    work_in_progress: bool,
    target_branch: String,
    source_branch: String,
    sha: Option<Sha>,
    diff_refs: Option<DiffRefs>,
    references: References,
    web_url: String,
}

fn key(payload: &'static str, mr: &MergeRequest) -> Result<MrKey, ParseError> {
    let key = MrKey::parse(&mr.references.full).map_err(|error| invalid(payload, error))?;
    if key.forge() != ForgeKind::Gitlab || key.number() != mr.iid {
        return Err(invalid(
            payload,
            format!(
                "reference `{}` is not merge request !{}",
                mr.references.full, mr.iid
            ),
        ));
    }
    Ok(key)
}

fn state(payload: &'static str, state: &str) -> Result<MrState, ParseError> {
    match state {
        // `locked`: open, while GitLab merges it.
        "opened" | "locked" => Ok(MrState::Open),
        "merged" => Ok(MrState::Merged),
        "closed" => Ok(MrState::Closed),
        other => Err(invalid(payload, format!("unknown state `{other}`"))),
    }
}

/// The username of a deleted user, as GitLab shows it.
const GHOST: &str = "ghost";

fn username(user: Option<User>) -> String {
    user.map_or_else(|| GHOST.to_owned(), |user| user.username)
}

/// `GET /projects/<id>/merge_requests/<iid>`.
///
/// # Errors
///
/// [`ParseError`] when the body is not a GitLab merge request, its reference or state is not
/// valid, or it has no `diff_refs` yet (GitLab computes them after a push).
pub fn parse_merge_request(body: &[u8]) -> Result<Mr, ParseError> {
    const PAYLOAD: &str = "GitLab merge request";
    let mr: MergeRequest = json(PAYLOAD, body)?;
    let key = key(PAYLOAD, &mr)?;
    let state = state(PAYLOAD, &mr.state)?;
    let refs = mr
        .diff_refs
        .ok_or_else(|| invalid(PAYLOAD, "no diff_refs"))?;
    Ok(Mr {
        key,
        forge_ids: MrForgeIds::Gitlab {
            id: mr.id,
            project_id: mr.project_id,
        },
        title: mr.title,
        description: mr.description.unwrap_or_default(),
        author: username(mr.author),
        state,
        draft: mr.draft || mr.work_in_progress,
        labels: mr.labels,
        reviewers: mr.reviewers.into_iter().map(|user| user.username).collect(),
        base_ref: mr.target_branch,
        head_ref: mr.source_branch,
        version: MrVersion {
            head_sha: refs.head_sha,
            base_sha: refs.start_sha,
            merge_base: Some(refs.base_sha),
            forge_version_id: None,
            created_at: None,
        },
        web_url: mr.web_url,
        created_at: mr.created_at.0,
        updated_at: mr.updated_at.0,
    })
}

/// `GET /merge_requests?scope=reviews_for_me&…`, one page.
///
/// # Errors
///
/// [`ParseError`] when the body is not a list of GitLab merge requests, or one has an invalid
/// reference or state.
pub fn parse_merge_requests(body: &[u8]) -> Result<Vec<MrSummary>, ParseError> {
    const PAYLOAD: &str = "GitLab merge requests";
    let mrs: Vec<MergeRequest> = json(PAYLOAD, body)?;
    mrs.into_iter()
        .map(|mr| {
            Ok(MrSummary {
                key: key(PAYLOAD, &mr)?,
                state: state(PAYLOAD, &mr.state)?,
                title: mr.title,
                author: username(mr.author),
                draft: mr.draft || mr.work_in_progress,
                head_sha: mr.sha,
                web_url: mr.web_url,
                updated_at: mr.updated_at.0,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct Version {
    id: u64,
    head_commit_sha: Sha,
    base_commit_sha: Sha,
    start_commit_sha: Sha,
    created_at: ForgeTime,
}

/// `GET /projects/<id>/merge_requests/<iid>/versions`, newest first as GitLab gives them.
///
/// # Errors
///
/// [`ParseError`] when the body is not a list of GitLab merge request versions.
pub fn parse_versions(body: &[u8]) -> Result<Vec<MrVersion>, ParseError> {
    let versions: Vec<Version> = json("GitLab merge request versions", body)?;
    Ok(versions
        .into_iter()
        .map(|version| MrVersion {
            head_sha: version.head_commit_sha,
            base_sha: version.start_commit_sha,
            merge_base: Some(version.base_commit_sha),
            forge_version_id: Some(version.id),
            created_at: Some(version.created_at.0),
        })
        .collect())
}

#[derive(Deserialize)]
#[expect(
    clippy::struct_field_names,
    clippy::struct_excessive_bools,
    reason = "the fields of the GitLab API"
)]
struct Diff {
    diff: String,
    new_path: String,
    old_path: String,
    #[serde(default)]
    a_mode: String,
    #[serde(default)]
    b_mode: String,
    #[serde(default)]
    new_file: bool,
    #[serde(default)]
    renamed_file: bool,
    #[serde(default)]
    deleted_file: bool,
    #[serde(default)]
    too_large: bool,
    #[serde(default)]
    collapsed: bool,
    #[serde(default)]
    generated_file: bool,
}

/// `GET /projects/<id>/merge_requests/<iid>/diffs`, one page.
///
/// The patch has no final newline, as on GitHub. A file that GitLab marks `too_large` or
/// `collapsed` has no patch and is `too_large`.
///
/// # Errors
///
/// [`ParseError`] when the body is not a list of GitLab diffs.
pub fn parse_diffs(body: &[u8]) -> Result<Vec<FileDiff>, ParseError> {
    let diffs: Vec<Diff> = json("GitLab merge request diffs", body)?;
    Ok(diffs
        .into_iter()
        .map(|diff| {
            let cut = diff.too_large || diff.collapsed;
            let text = diff.diff.strip_suffix('\n').unwrap_or(&diff.diff);
            let patch = (!cut && !text.is_empty()).then(|| text.to_owned());
            let change = if diff.new_file {
                FileChange::Added
            } else if diff.deleted_file {
                FileChange::Deleted
            } else if diff.renamed_file {
                FileChange::Renamed
            } else if patch.is_none() && !cut && diff.a_mode != diff.b_mode {
                FileChange::Changed
            } else {
                FileChange::Modified
            };
            FileDiff {
                old_path: diff.renamed_file.then_some(diff.old_path),
                path: diff.new_path,
                change,
                patch,
                too_large: cut,
                generated: diff.generated_file,
            }
        })
        .collect())
}

#[derive(Deserialize)]
struct LinePoint {
    old_line: Option<u32>,
    new_line: Option<u32>,
}

#[derive(Deserialize)]
/// The end of a range is the line of the position itself.
struct LineRange {
    start: LinePoint,
}

#[derive(Deserialize)]
#[expect(clippy::struct_field_names, reason = "the names of the GitLab API")]
struct Position {
    head_sha: Option<Sha>,
    old_path: Option<String>,
    new_path: Option<String>,
    position_type: Option<String>,
    old_line: Option<u32>,
    new_line: Option<u32>,
    line_range: Option<LineRange>,
}

/// The subject of a position; `None` when the position names no file (all fields null).
fn subject(position: Position) -> Option<ThreadSubject> {
    let sha = position.head_sha?;
    if position.position_type.as_deref() == Some("text") {
        // An unchanged line has both lines: it is on the new side, as on GitHub.
        let side = if position.new_line.is_some() {
            Side::New
        } else {
            Side::Old
        };
        let line = |point: &LinePoint| match side {
            Side::New => point.new_line,
            Side::Old => point.old_line,
        };
        // The path in the head on both sides, as on GitHub.
        let path = position
            .new_path
            .clone()
            .or_else(|| position.old_path.clone());
        let end = line(&LinePoint {
            old_line: position.old_line,
            new_line: position.new_line,
        });
        if let (Some(end), Some(path)) = (end, path) {
            // A range that starts on the other side has no start line on this side: it is
            // kept on its last line.
            let start = position
                .line_range
                .as_ref()
                .and_then(|range| line(&range.start));
            return Some(ThreadSubject::Lines {
                anchor: Anchor {
                    path,
                    side,
                    start_line: crate::model::range_start(start, end),
                    end_line: end,
                    sha,
                },
            });
        }
    }
    // A file or image position, or a text position without its lines.
    let path = position.new_path.or(position.old_path)?;
    Some(ThreadSubject::File { path, sha })
}

#[derive(Deserialize)]
struct Discussion {
    id: String,
    notes: Vec<Note>,
}

#[derive(Deserialize)]
struct Note {
    id: u64,
    body: String,
    author: Option<User>,
    created_at: ForgeTime,
    #[serde(default)]
    system: bool,
    #[serde(default)]
    resolvable: bool,
    #[serde(default)]
    resolved: bool,
    position: Option<Position>,
}

/// `GET /projects/<id>/merge_requests/<iid>/discussions`, one page.
///
/// System notes ("added 1 commit") are dropped, and a discussion with only system notes is
/// dropped. A discussion on lines is outdated when its position is not on `head`, the current
/// head SHA: GitLab moves the position of a discussion to the new version when its lines are
/// still in the diff. Two limits of that rule: until GitLab has moved the positions after a
/// push (a background job), every discussion on lines reads as outdated; a discussion on one
/// commit of the MR (commit view) is always outdated. A discussion is resolved when all its
/// resolvable notes are resolved; a general discussion can be resolvable too.
///
/// # Errors
///
/// [`ParseError`] when the body is not a list of GitLab discussions.
pub fn parse_discussions(body: &[u8], head: &Sha) -> Result<Vec<Thread>, ParseError> {
    let discussions: Vec<Discussion> = json("GitLab discussions", body)?;
    Ok(discussions
        .into_iter()
        .filter_map(|discussion| {
            let mut notes: Vec<Note> = discussion
                .notes
                .into_iter()
                .filter(|note| !note.system)
                .collect();
            if notes.is_empty() {
                return None;
            }
            let resolvable: Vec<&Note> = notes.iter().filter(|note| note.resolvable).collect();
            let resolved = !resolvable.is_empty() && resolvable.iter().all(|note| note.resolved);
            let subject = notes
                .first_mut()
                .and_then(|note| note.position.take())
                .and_then(subject)
                .unwrap_or(ThreadSubject::General);
            let outdated = match &subject {
                ThreadSubject::General => false,
                ThreadSubject::File { sha, .. }
                | ThreadSubject::Lines {
                    anchor: Anchor { sha, .. },
                } => sha != head,
            };
            Some(Thread {
                id: discussion.id,
                subject,
                outdated,
                resolved,
                comments: notes
                    .into_iter()
                    .map(|note| Comment {
                        id: note.id.to_string(),
                        author: username(note.author),
                        body: note.body,
                        created_at: note.created_at.0,
                    })
                    .collect(),
            })
        })
        .collect())
}

/// A draft note of a merge request: visible only to its author until `bulk_publish`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftNote {
    /// The ID of the draft note.
    pub id: u64,
    /// The ID of the author.
    pub author_id: u64,
    /// The body, in Markdown.
    pub body: String,
    /// The discussion it replies to; `None` for a new thread.
    pub discussion_id: Option<String>,
    /// True when the publication resolves the discussion.
    pub resolve_discussion: bool,
    /// What it is on; `None` for a general comment or a reply.
    pub subject: Option<ThreadSubject>,
}

#[derive(Deserialize)]
struct RawDraftNote {
    id: u64,
    author_id: u64,
    note: String,
    discussion_id: Option<String>,
    #[serde(default)]
    resolve_discussion: bool,
    position: Option<Position>,
}

impl From<RawDraftNote> for DraftNote {
    fn from(note: RawDraftNote) -> Self {
        Self {
            id: note.id,
            author_id: note.author_id,
            body: note.note,
            discussion_id: note.discussion_id,
            resolve_discussion: note.resolve_discussion,
            subject: note.position.and_then(subject),
        }
    }
}

/// `GET /projects/<id>/merge_requests/<iid>/draft_notes`.
///
/// # Errors
///
/// [`ParseError`] when the body is not a list of GitLab draft notes.
pub fn parse_draft_notes(body: &[u8]) -> Result<Vec<DraftNote>, ParseError> {
    let notes: Vec<RawDraftNote> = json("GitLab draft notes", body)?;
    Ok(notes.into_iter().map(DraftNote::from).collect())
}

/// `POST /projects/<id>/merge_requests/<iid>/draft_notes`: the new draft note.
///
/// # Errors
///
/// [`ParseError`] when the body is not a GitLab draft note.
pub fn parse_draft_note(body: &[u8]) -> Result<DraftNote, ParseError> {
    let note: RawDraftNote = json("GitLab draft note", body)?;
    Ok(note.into())
}

/// The approvals of a merge request (`POST approve`, `GET approvals`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approvals {
    /// True when the approval rules of the merge request are met.
    pub approved: bool,
    /// True when the user of the token approved the merge request.
    pub user_has_approved: bool,
    /// The usernames of the users that approved.
    pub approved_by: Vec<String>,
}

#[derive(Deserialize)]
struct RawApprovals {
    #[serde(default)]
    approved: bool,
    user_has_approved: bool,
    #[serde(default)]
    approved_by: Vec<ApprovedBy>,
}

#[derive(Deserialize)]
struct ApprovedBy {
    user: User,
}

/// `POST /projects/<id>/merge_requests/<iid>/approve` and `GET …/approvals`.
///
/// # Errors
///
/// [`ParseError`] when the body is not a GitLab approval state.
pub fn parse_approvals(body: &[u8]) -> Result<Approvals, ParseError> {
    let approvals: RawApprovals = json("GitLab approvals", body)?;
    Ok(Approvals {
        approved: approvals.approved,
        user_has_approved: approvals.user_has_approved,
        approved_by: approvals
            .approved_by
            .into_iter()
            .map(|approval| approval.user.username)
            .collect(),
    })
}

#[derive(Deserialize)]
struct RestError {
    message: Option<serde_json::Value>,
    error: Option<String>,
    error_description: Option<String>,
}

/// The body of an error response (400, 401, 403, 404, 409…). GitLab gives `message` as a text
/// or as an object of lists (validation errors), or `error` and `error_description` (OAuth
/// scope errors).
///
/// # Errors
///
/// [`ParseError`] when the body is not a GitLab error.
pub fn parse_error(body: &[u8]) -> Result<ErrorBody, ParseError> {
    const PAYLOAD: &str = "GitLab error";
    let RestError {
        message,
        error,
        error_description,
    } = json(PAYLOAD, body)?;
    match (message, error) {
        (Some(serde_json::Value::String(message)), _) => Ok(ErrorBody {
            message,
            details: Vec::new(),
        }),
        (Some(serde_json::Value::Object(fields)), _) => {
            let details = fields
                .into_iter()
                .flat_map(|(field, value)| {
                    let messages = match value {
                        serde_json::Value::Array(items) => items,
                        other => vec![other],
                    };
                    messages.into_iter().map(move |message| match message {
                        serde_json::Value::String(text) => format!("{field}: {text}"),
                        other => format!("{field}: {other}"),
                    })
                })
                .collect::<Vec<_>>();
            Ok(ErrorBody {
                message: details.first().cloned().unwrap_or_default(),
                details,
            })
        }
        (_, Some(error)) => Ok(ErrorBody {
            message: error,
            details: error_description.into_iter().collect(),
        }),
        (Some(serde_json::Value::Array(items)), None) if !items.is_empty() => {
            let details: Vec<String> = items
                .into_iter()
                .map(|item| match item {
                    serde_json::Value::String(text) => text,
                    other => other.to_string(),
                })
                .collect();
            Ok(ErrorBody {
                message: details.first().cloned().unwrap_or_default(),
                details,
            })
        }
        (Some(_) | None, None) => Err(invalid(PAYLOAD, "no message and no error")),
    }
}
