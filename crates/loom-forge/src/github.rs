//! GitHub payloads (REST and GraphQL) to the model. Unknown fields are ignored.
//!
//! Every parser takes the body of a response with a success status. Error bodies (4xx, 5xx)
//! go to [`parse_error`].

use loom_api::{ForgeKind, Timestamp};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::{ErrorBody, ForgeTime, GraphqlError, ParseError, invalid, json};
use crate::model::{
    Anchor, Comment, FileChange, FileDiff, ForgeUser, Mr, MrForgeIds, MrKey, MrState, MrSummary,
    MrVersion, Sha, Side, Thread, ThreadSubject,
};

/// The login that GitHub shows for a deleted user.
const GHOST: &str = "ghost";

#[derive(Deserialize)]
struct User {
    login: String,
    id: u64,
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

#[derive(Deserialize)]
struct Repo {
    full_name: String,
}

#[derive(Deserialize)]
struct Branch {
    #[serde(rename = "ref")]
    name: String,
    sha: Sha,
    repo: Option<Repo>,
}

#[derive(Deserialize)]
struct Pull {
    node_id: String,
    id: u64,
    number: u64,
    state: String,
    title: String,
    user: Option<User>,
    body: Option<String>,
    created_at: ForgeTime,
    updated_at: ForgeTime,
    merged_at: Option<ForgeTime>,
    #[serde(default)]
    requested_reviewers: Vec<User>,
    #[serde(default)]
    labels: Vec<Label>,
    #[serde(default)]
    draft: bool,
    head: Branch,
    base: Branch,
    html_url: String,
}

/// `GET /user`.
///
/// # Errors
///
/// [`ParseError`] when the body is not a GitHub user.
pub fn parse_user(body: &[u8]) -> Result<ForgeUser, ParseError> {
    let user: User = json("GitHub user", body)?;
    Ok(ForgeUser {
        id: user.id,
        login: user.login,
    })
}

/// `GET /repos/<owner>/<repo>/pulls/<number>`.
///
/// # Errors
///
/// [`ParseError`] when the body is not a GitHub pull request, or its state or repository is
/// not valid.
pub fn parse_pull(body: &[u8]) -> Result<Mr, ParseError> {
    const PAYLOAD: &str = "GitHub pull request";
    let pull: Pull = json(PAYLOAD, body)?;
    let repo = pull
        .base
        .repo
        .ok_or_else(|| invalid(PAYLOAD, "the base has no repository"))?;
    let key = MrKey::new(ForgeKind::Github, &repo.full_name, pull.number)
        .map_err(|error| invalid(PAYLOAD, error))?;
    Ok(Mr {
        key,
        forge_ids: MrForgeIds::Github {
            node_id: pull.node_id,
            id: pull.id,
        },
        title: pull.title,
        description: pull.body.unwrap_or_default(),
        author: login(pull.user),
        state: state(PAYLOAD, &pull.state, pull.merged_at.is_some())?,
        draft: pull.draft,
        labels: pull.labels.into_iter().map(|label| label.name).collect(),
        reviewers: pull
            .requested_reviewers
            .into_iter()
            .map(|user| user.login)
            .collect(),
        base_ref: pull.base.name,
        head_ref: pull.head.name,
        version: MrVersion {
            head_sha: pull.head.sha,
            base_sha: pull.base.sha,
            merge_base: None,
            forge_version_id: None,
            created_at: None,
        },
        web_url: pull.html_url,
        created_at: pull.created_at.0,
        updated_at: pull.updated_at.0,
    })
}

fn login(user: Option<User>) -> String {
    user.map_or_else(|| GHOST.to_owned(), |user| user.login)
}

fn state(payload: &'static str, state: &str, merged: bool) -> Result<MrState, ParseError> {
    match state {
        "open" => Ok(MrState::Open),
        "closed" if merged => Ok(MrState::Merged),
        "closed" => Ok(MrState::Closed),
        other => Err(invalid(payload, format!("unknown state `{other}`"))),
    }
}

/// A page of the search of pull requests to review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPage {
    /// The number of results of the search, on every page.
    pub total_count: u64,
    /// True when GitHub stopped the search before it found every result.
    pub incomplete_results: bool,
    /// The pull requests of this page. Issues are left out.
    pub items: Vec<MrSummary>,
}

#[derive(Deserialize)]
struct Search {
    total_count: u64,
    incomplete_results: bool,
    items: Vec<SearchItem>,
}

#[derive(Deserialize)]
struct SearchItem {
    repository_url: String,
    html_url: String,
    number: u64,
    title: String,
    user: Option<User>,
    state: String,
    #[serde(default)]
    draft: bool,
    updated_at: ForgeTime,
    pull_request: Option<SearchPull>,
}

#[derive(Deserialize)]
struct SearchPull {
    merged_at: Option<ForgeTime>,
}

/// `GET /search/issues?q=is:pr+…+review-requested:@me`.
///
/// # Errors
///
/// [`ParseError`] when the body is not a GitHub search result, or an item has an invalid
/// repository or state.
pub fn parse_search(body: &[u8]) -> Result<SearchPage, ParseError> {
    const PAYLOAD: &str = "GitHub search";
    let search: Search = json(PAYLOAD, body)?;
    let mut items = Vec::with_capacity(search.items.len());
    for item in search.items {
        let Some(pull) = item.pull_request else {
            continue;
        };
        // `https://api.github.com/repos/<owner>/<repo>`, or `/api/v3/repos/…` on GitHub
        // Enterprise Server: the repository is the last two segments.
        let mut segments = item.repository_url.rsplit('/');
        let (Some(repo), Some(owner)) = (segments.next(), segments.next()) else {
            return Err(invalid(PAYLOAD, "a repository URL has no owner"));
        };
        let key = MrKey::new(ForgeKind::Github, &format!("{owner}/{repo}"), item.number)
            .map_err(|error| invalid(PAYLOAD, error))?;
        items.push(MrSummary {
            key,
            title: item.title,
            author: login(item.user),
            state: state(PAYLOAD, &item.state, pull.merged_at.is_some())?,
            draft: item.draft,
            head_sha: None,
            web_url: item.html_url,
            updated_at: item.updated_at.0,
        });
    }
    Ok(SearchPage {
        total_count: search.total_count,
        incomplete_results: search.incomplete_results,
        items,
    })
}

#[derive(Deserialize)]
struct File {
    filename: String,
    previous_filename: Option<String>,
    status: String,
    #[serde(default)]
    changes: u64,
    patch: Option<String>,
}

/// `GET /repos/<owner>/<repo>/pulls/<number>/files`.
///
/// GitHub gives no patch for a binary file and for a file whose diff is too large: a file
/// with changed lines and no patch is `too_large`.
///
/// # Errors
///
/// [`ParseError`] when the body is not a list of GitHub pull request files, or a status is
/// unknown.
pub fn parse_files(body: &[u8]) -> Result<Vec<FileDiff>, ParseError> {
    const PAYLOAD: &str = "GitHub pull request files";
    let files: Vec<File> = json(PAYLOAD, body)?;
    files
        .into_iter()
        .map(|file| {
            let change = match file.status.as_str() {
                "added" => FileChange::Added,
                "removed" => FileChange::Deleted,
                "modified" => FileChange::Modified,
                "renamed" => FileChange::Renamed,
                "copied" => FileChange::Copied,
                "changed" | "unchanged" => FileChange::Changed,
                other => return Err(invalid(PAYLOAD, format!("unknown file status `{other}`"))),
            };
            let patch = file.patch.filter(|patch| !patch.is_empty());
            Ok(FileDiff {
                too_large: patch.is_none() && file.changes > 0,
                path: file.filename,
                old_path: file.previous_filename,
                change,
                patch,
                generated: false,
            })
        })
        .collect()
}

/// The state of a GitHub review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewState {
    /// Not submitted: visible only to its author.
    Pending,
    /// Submitted without a decision.
    Commented,
    /// Submitted with an approval.
    Approved,
    /// Submitted with a request for changes.
    ChangesRequested,
    /// An approval or a request for changes that was dismissed.
    Dismissed,
}

/// A review of a pull request (`GET /repos/<owner>/<repo>/pulls/<number>/reviews`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    /// The GraphQL node ID.
    pub id: String,
    /// The login of the author.
    pub author: String,
    /// The state.
    pub state: ReviewState,
    /// The summary, in Markdown. Empty when the review has none.
    pub body: String,
    /// The head SHA that the review is on.
    pub commit_sha: Option<Sha>,
    /// The time of the submission; `None` for a pending review.
    pub submitted_at: Option<Timestamp>,
}

#[derive(Deserialize)]
struct RestReview {
    node_id: String,
    user: Option<User>,
    state: ReviewState,
    body: Option<String>,
    commit_id: Option<Sha>,
    submitted_at: Option<ForgeTime>,
}

/// `GET /repos/<owner>/<repo>/pulls/<number>/reviews`.
///
/// # Errors
///
/// [`ParseError`] when the body is not a list of GitHub reviews.
pub fn parse_reviews(body: &[u8]) -> Result<Vec<Review>, ParseError> {
    let reviews: Vec<RestReview> = json("GitHub reviews", body)?;
    Ok(reviews
        .into_iter()
        .map(|review| Review {
            id: review.node_id,
            author: login(review.user),
            state: review.state,
            body: review.body.unwrap_or_default(),
            commit_sha: review.commit_id,
            submitted_at: review.submitted_at.map(|time| time.0),
        })
        .collect())
}

#[derive(Deserialize)]
struct IssueComment {
    node_id: String,
    user: Option<User>,
    body: Option<String>,
    created_at: ForgeTime,
}

/// `GET /repos/<owner>/<repo>/issues/<number>/comments`: the general comments, one thread
/// each.
///
/// # Errors
///
/// [`ParseError`] when the body is not a list of GitHub issue comments.
pub fn parse_issue_comments(body: &[u8]) -> Result<Vec<Thread>, ParseError> {
    let comments: Vec<IssueComment> = json("GitHub issue comments", body)?;
    Ok(comments
        .into_iter()
        .map(|comment| {
            general(
                comment.node_id,
                login(comment.user),
                comment.body,
                comment.created_at.0,
            )
        })
        .collect())
}

fn general(id: String, author: String, body: Option<String>, created_at: Timestamp) -> Thread {
    Thread {
        id: id.clone(),
        subject: ThreadSubject::General,
        outdated: false,
        resolved: false,
        comments: vec![Comment {
            id,
            author,
            body: body.unwrap_or_default(),
            created_at,
        }],
    }
}

/// Every thread of a pull request: its review threads, its general comments, and the
/// summaries of its submitted reviews (a review without a summary has no thread). Sorted by
/// [`crate::sort_threads`].
#[must_use]
pub fn threads(
    review_threads: Vec<Thread>,
    general_comments: Vec<Thread>,
    reviews: &[Review],
) -> Vec<Thread> {
    let summaries = reviews
        .iter()
        .filter(|review| review.state != ReviewState::Pending && !review.body.is_empty())
        .map(|review| {
            general(
                review.id.clone(),
                review.author.clone(),
                Some(review.body.clone()),
                review.submitted_at.unwrap_or(Timestamp::EPOCH),
            )
        });
    let mut all: Vec<Thread> = review_threads
        .into_iter()
        .chain(general_comments)
        .chain(summaries)
        .collect();
    crate::sort_threads(&mut all);
    all
}

#[derive(Deserialize)]
struct Response<T> {
    data: Option<T>,
    errors: Option<Vec<GraphqlError>>,
}

/// The `data` of a GraphQL response; its `errors` as [`ParseError::Graphql`].
fn graphql<T: DeserializeOwned>(payload: &'static str, body: &[u8]) -> Result<T, ParseError> {
    // Errors first: with them, `data` is often partial or null.
    let errors: Response<serde::de::IgnoredAny> = json(payload, body)?;
    if let Some(errors) = errors.errors.filter(|errors| !errors.is_empty()) {
        return Err(ParseError::Graphql { payload, errors });
    }
    // Parsed again from the bytes, so a parse error gives its line and column.
    let response: Response<T> = json(payload, body)?;
    response
        .data
        .ok_or_else(|| invalid(payload, "no data and no errors"))
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
struct Commit {
    oid: Sha,
}

#[derive(Deserialize)]
struct PageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsData {
    repository: Option<ThreadsRepository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsRepository {
    pull_request: Option<ThreadsPull>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsPull {
    id: String,
    head_ref_oid: Sha,
    review_threads: Connection<ReviewThread>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection<T> {
    page_info: PageInfo,
    nodes: Vec<T>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum DiffSide {
    Left,
    Right,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum SubjectType {
    Line,
    File,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewThread {
    id: String,
    is_resolved: bool,
    is_outdated: bool,
    path: String,
    diff_side: DiffSide,
    line: Option<u32>,
    original_line: Option<u32>,
    start_line: Option<u32>,
    original_start_line: Option<u32>,
    start_diff_side: Option<DiffSide>,
    /// Missing on older GitHub Enterprise Server versions: a line thread then.
    subject_type: Option<SubjectType>,
    comments: Connection<ThreadComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadComment {
    id: String,
    author: Option<Login>,
    body: String,
    created_at: ForgeTime,
    original_commit: Option<Commit>,
    state: CommentState,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum CommentState {
    Pending,
    Submitted,
}

/// A page of the review threads of a pull request (GraphQL `reviewThreads`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThreadsPage {
    /// The GraphQL node ID of the pull request.
    pub pull_request_id: String,
    /// The current head SHA of the pull request.
    pub head_sha: Sha,
    /// The threads of this page, in the order of GitHub.
    pub threads: Vec<Thread>,
    /// The cursor of the next page; `None` on the last page.
    pub next_cursor: Option<String>,
    /// The IDs of the threads that have more comments than this page gives.
    pub threads_with_more_comments: Vec<String>,
}

/// GraphQL `repository.pullRequest.reviewThreads`, with the fields of
/// `fixtures/forge/github/graphql-review-threads.json`.
///
/// Comments of my pending review are left out: they are not visible to others, and a thread
/// that has only such comments is left out too. A thread that is not outdated is anchored on
/// its current lines and on the current head; an outdated thread on its original lines and on
/// the commit of its first comment.
///
/// # Errors
///
/// [`ParseError`] when the body is not such a response, has GraphQL errors, or has no pull
/// request.
pub fn parse_review_threads(body: &[u8]) -> Result<ReviewThreadsPage, ParseError> {
    const PAYLOAD: &str = "GitHub review threads";
    let data: ThreadsData = graphql(PAYLOAD, body)?;
    let pull = data
        .repository
        .and_then(|repository| repository.pull_request)
        .ok_or_else(|| invalid(PAYLOAD, "no pull request"))?;
    let page = pull.review_threads.page_info;
    let mut threads = Vec::new();
    let mut threads_with_more_comments = Vec::new();
    for thread in pull.review_threads.nodes {
        let more_comments = thread.comments.page_info.has_next_page;
        if let Some(thread) = review_thread(thread, &pull.head_ref_oid) {
            if more_comments {
                threads_with_more_comments.push(thread.id.clone());
            }
            threads.push(thread);
        }
    }
    Ok(ReviewThreadsPage {
        pull_request_id: pull.id,
        head_sha: pull.head_ref_oid,
        threads,
        next_cursor: page.end_cursor.filter(|_| page.has_next_page),
        threads_with_more_comments,
    })
}

/// A page of the comments of one review thread (GraphQL `node(id:)` on a
/// `PullRequestReviewThread`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadCommentsPage {
    /// The submitted comments of this page, in the order of GitHub. Comments of my pending
    /// review are left out.
    pub comments: Vec<Comment>,
    /// The cursor of the next page; `None` on the last page.
    pub next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct ThreadNodeData {
    node: Option<ThreadNode>,
}

#[derive(Deserialize)]
struct ThreadNode {
    comments: Connection<ThreadComment>,
}

/// GraphQL `node(id:) { ... on PullRequestReviewThread { comments } }`, with the comment
/// fields of `parse_review_threads`.
///
/// # Errors
///
/// [`ParseError`] when the body is not such a response, has GraphQL errors, or has no thread.
pub fn parse_thread_comments(body: &[u8]) -> Result<ThreadCommentsPage, ParseError> {
    const PAYLOAD: &str = "GitHub review thread comments";
    let data: ThreadNodeData = graphql(PAYLOAD, body)?;
    let comments = data
        .node
        .ok_or_else(|| invalid(PAYLOAD, "no review thread"))?
        .comments;
    let page = comments.page_info;
    Ok(ThreadCommentsPage {
        comments: submitted(comments.nodes),
        next_cursor: page.end_cursor.filter(|_| page.has_next_page),
    })
}

/// The submitted comments of a thread: comments of my pending review are visible to me only.
fn submitted(comments: Vec<ThreadComment>) -> Vec<Comment> {
    comments
        .into_iter()
        .filter(|comment| comment.state == CommentState::Submitted)
        .map(|comment| Comment {
            id: comment.id,
            author: comment
                .author
                .map_or_else(|| GHOST.to_owned(), |author| author.login),
            body: comment.body,
            created_at: comment.created_at.0,
        })
        .collect()
}

/// A thread with no submitted comment on its first page is left out, even when it has more
/// pages: its first comment is of my pending review, so nobody else can see it or reply to it.
fn review_thread(thread: ReviewThread, head: &Sha) -> Option<Thread> {
    let original_sha = thread
        .comments
        .nodes
        .first()
        .and_then(|comment| comment.original_commit.as_ref())
        .map(|commit| commit.oid.clone());
    let comments = submitted(thread.comments.nodes);
    if comments.is_empty() {
        return None;
    }
    let (end, start, sha) = if thread.is_outdated {
        (
            thread.original_line,
            thread.original_start_line,
            original_sha,
        )
    } else {
        (thread.line, thread.start_line, Some(head.clone()))
    };
    let file_sha = sha.clone().unwrap_or_else(|| head.clone());
    // A range that starts on the other side is kept on its last line.
    let start = start.filter(|_| {
        thread
            .start_diff_side
            .is_none_or(|side| side == thread.diff_side)
    });
    let subject = match (thread.subject_type.unwrap_or(SubjectType::Line), end, sha) {
        (SubjectType::Line, Some(end), Some(sha)) => ThreadSubject::Lines {
            anchor: Anchor {
                path: thread.path,
                side: match thread.diff_side {
                    DiffSide::Left => Side::Old,
                    DiffSide::Right => Side::New,
                },
                start_line: crate::model::range_start(start, end),
                end_line: end,
                sha,
            },
        },
        // A line thread without its lines (GitHub lost them) is kept on its file.
        (SubjectType::Line | SubjectType::File, _, _) => ThreadSubject::File {
            path: thread.path,
            sha: file_sha,
        },
    };
    Some(Thread {
        id: thread.id,
        subject,
        outdated: thread.is_outdated,
        resolved: thread.is_resolved,
        comments,
    })
}

/// A pending review of the viewer (GraphQL `reviews(states: PENDING)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReview {
    /// The GraphQL node ID.
    pub id: String,
    /// The login of the author.
    pub author: String,
    /// The time when it was started.
    pub created_at: Timestamp,
    /// The number of its comments.
    pub comment_count: u64,
    /// The commit that the review is on, when the response gives it.
    pub commit_sha: Option<Sha>,
    /// Its comments (first 100), when the response gives them: a resume of a publish matches
    /// them with its intents.
    pub comments: Vec<PendingComment>,
}

/// A comment of a pending review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingComment {
    /// The GraphQL node ID.
    pub id: String,
    /// The body, in Markdown.
    pub body: String,
    /// The path of the file.
    pub path: String,
    /// The last line; `None` for a comment on a file or on lines GitHub lost.
    pub line: Option<u32>,
    /// The first line of a range; `None` for one line.
    pub start_line: Option<u32>,
    /// The ID of the comment that it replies to, for a reply to an existing thread.
    pub reply_to: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingData {
    repository: Option<PendingRepository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingRepository {
    pull_request: Option<PendingPull>,
}

#[derive(Deserialize)]
struct PendingPull {
    reviews: Nodes<PendingNode>,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingNode {
    id: String,
    state: ReviewState,
    author: Option<Login>,
    created_at: ForgeTime,
    commit: Option<Commit>,
    comments: PendingComments,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingComments {
    total_count: u64,
    #[serde(default)]
    nodes: Vec<PendingCommentNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingCommentNode {
    id: String,
    body: String,
    path: String,
    line: Option<u32>,
    start_line: Option<u32>,
    reply_to: Option<Id>,
}

/// GraphQL `repository.pullRequest.reviews(states: PENDING)`. Reviews in another state are
/// left out.
///
/// # Errors
///
/// [`ParseError`] when the body is not such a response, has GraphQL errors, or has no pull
/// request.
pub fn parse_pending_reviews(body: &[u8]) -> Result<Vec<PendingReview>, ParseError> {
    const PAYLOAD: &str = "GitHub pending reviews";
    let data: PendingData = graphql(PAYLOAD, body)?;
    let pull = data
        .repository
        .and_then(|repository| repository.pull_request)
        .ok_or_else(|| invalid(PAYLOAD, "no pull request"))?;
    Ok(pull
        .reviews
        .nodes
        .into_iter()
        .filter(|review| review.state == ReviewState::Pending)
        .map(|review| PendingReview {
            id: review.id,
            author: review
                .author
                .map_or_else(|| GHOST.to_owned(), |author| author.login),
            created_at: review.created_at.0,
            comment_count: review.comments.total_count,
            commit_sha: review.commit.map(|commit| commit.oid),
            comments: review
                .comments
                .nodes
                .into_iter()
                .map(|comment| PendingComment {
                    id: comment.id,
                    body: comment.body,
                    path: comment.path,
                    line: comment.line,
                    start_line: comment.start_line,
                    reply_to: comment.reply_to.map(|reply_to| reply_to.id),
                })
                .collect(),
        })
        .collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddReviewData {
    add_pull_request_review: Option<ReviewPayload>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewPayload {
    pull_request_review: ReviewNode,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewNode {
    id: String,
    state: ReviewState,
    submitted_at: Option<ForgeTime>,
    url: Option<String>,
}

/// GraphQL `addPullRequestReview` without `event`: the ID of the new pending review.
///
/// # Errors
///
/// [`ParseError::Graphql`] when GitHub refuses the review (one pending review per user, a
/// line outside the diff); another [`ParseError`] when the body is not such a response or the
/// review is not pending.
pub fn parse_add_review(body: &[u8]) -> Result<String, ParseError> {
    const PAYLOAD: &str = "GitHub addPullRequestReview";
    let data: AddReviewData = graphql(PAYLOAD, body)?;
    let review = data
        .add_pull_request_review
        .ok_or_else(|| invalid(PAYLOAD, "no review"))?
        .pull_request_review;
    if review.state != ReviewState::Pending {
        return Err(invalid(PAYLOAD, "the review is not pending"));
    }
    Ok(review.id)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddReplyData {
    add_pull_request_review_thread_reply: Option<ReplyPayload>,
}

#[derive(Deserialize)]
struct ReplyPayload {
    comment: Id,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

/// GraphQL `addPullRequestReviewThreadReply`: the ID of the new comment.
///
/// # Errors
///
/// [`ParseError`] when the body is not such a response or has GraphQL errors.
pub fn parse_add_reply(body: &[u8]) -> Result<String, ParseError> {
    const PAYLOAD: &str = "GitHub addPullRequestReviewThreadReply";
    let data: AddReplyData = graphql(PAYLOAD, body)?;
    Ok(data
        .add_pull_request_review_thread_reply
        .ok_or_else(|| invalid(PAYLOAD, "no comment"))?
        .comment
        .id)
}

/// A submitted review (GraphQL `submitPullRequestReview`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedReview {
    /// The GraphQL node ID.
    pub id: String,
    /// The state after the submission.
    pub state: ReviewState,
    /// The time of the submission.
    pub submitted_at: Timestamp,
    /// The page of the review.
    pub url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmitData {
    submit_pull_request_review: Option<ReviewPayload>,
}

/// GraphQL `submitPullRequestReview`.
///
/// # Errors
///
/// [`ParseError`] when the body is not such a response, has GraphQL errors, or the review is
/// still pending.
pub fn parse_submit_review(body: &[u8]) -> Result<SubmittedReview, ParseError> {
    const PAYLOAD: &str = "GitHub submitPullRequestReview";
    let data: SubmitData = graphql(PAYLOAD, body)?;
    let review = data
        .submit_pull_request_review
        .ok_or_else(|| invalid(PAYLOAD, "no review"))?
        .pull_request_review;
    let (Some(submitted_at), Some(url)) = (review.submitted_at, review.url) else {
        return Err(invalid(PAYLOAD, "the review has no submission time or URL"));
    };
    if review.state == ReviewState::Pending {
        return Err(invalid(PAYLOAD, "the review is still pending"));
    }
    Ok(SubmittedReview {
        id: review.id,
        state: review.state,
        submitted_at: submitted_at.0,
        url,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveData {
    resolve_review_thread: Option<ResolvePayload>,
}

#[derive(Deserialize)]
struct ResolvePayload {
    thread: ResolvedNode,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolvedNode {
    id: String,
    is_resolved: bool,
}

/// GraphQL `resolveReviewThread`: the ID of the thread, which is resolved.
///
/// # Errors
///
/// [`ParseError`] when the body is not such a response, has GraphQL errors, or the thread is
/// not resolved.
pub fn parse_resolve_thread(body: &[u8]) -> Result<String, ParseError> {
    const PAYLOAD: &str = "GitHub resolveReviewThread";
    let data: ResolveData = graphql(PAYLOAD, body)?;
    let thread = data
        .resolve_review_thread
        .ok_or_else(|| invalid(PAYLOAD, "no thread"))?
        .thread;
    if !thread.is_resolved {
        return Err(invalid(PAYLOAD, "the thread is not resolved"));
    }
    Ok(thread.id)
}

#[derive(Deserialize)]
struct RestError {
    message: String,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

/// The body of a REST error response (401, 403, 404, 422…).
///
/// # Errors
///
/// [`ParseError`] when the body is not a GitHub error.
pub fn parse_error(body: &[u8]) -> Result<ErrorBody, ParseError> {
    let error: RestError = json("GitHub error", body)?;
    Ok(ErrorBody {
        message: error.message,
        details: error
            .errors
            .into_iter()
            .map(|detail| match detail {
                serde_json::Value::String(text) => text,
                serde_json::Value::Object(ref fields) => fields
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| detail.to_string(), str::to_owned),
                other => other.to_string(),
            })
            .collect(),
    })
}
