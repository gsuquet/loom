//! The GitHub GraphQL calls: review threads, pending reviews, and the publish mutations of
//! ADR-0016 (decision 5).

use serde::Serialize;

use super::http::{Answer, Body, MAX_PAGES, rate_limit_wait};
use super::{ForgeError, GithubClient, GraphqlStep, NewThread};
use crate::error::ParseError;
use crate::model::{MrKey, Sha, Side, Thread, VerdictKind};

/// The review threads of a pull request, with their comments. Every page is read.
pub const REVIEW_THREADS: &str = "query($owner: String!, $name: String!, $number: Int!, $cursor: String) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { id headRefOid reviewThreads(first: 100, after: $cursor) { pageInfo { hasNextPage endCursor } nodes { id isResolved isOutdated path diffSide line originalLine startLine originalStartLine startDiffSide subjectType comments(first: 50) { pageInfo { hasNextPage endCursor } nodes { id author { login } body createdAt originalCommit { oid } state } } } } } } }";

/// The comments of one review thread, for a thread with more than one page of them.
pub const THREAD_COMMENTS: &str = "query($id: ID!, $cursor: String) { node(id: $id) { ... on PullRequestReviewThread { comments(first: 100, after: $cursor) { pageInfo { hasNextPage endCursor } nodes { id author { login } body createdAt originalCommit { oid } state } } } } }";

/// The pending reviews of the viewer: GitHub shows a pending review to its author only.
pub const PENDING_REVIEWS: &str = "query($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { reviews(states: PENDING, first: 100) { nodes { id state author { login } createdAt comments { totalCount } } } } } }";

/// Step 1 of a publish: a pending review with the new threads (no `event`).
pub const ADD_REVIEW: &str = "mutation($input: AddPullRequestReviewInput!) { addPullRequestReview(input: $input) { pullRequestReview { id state } } }";

/// Step 2: a reply to an existing thread, in the pending review.
pub const ADD_REPLY: &str = "mutation($input: AddPullRequestReviewThreadReplyInput!) { addPullRequestReviewThreadReply(input: $input) { comment { id } } }";

/// Steps 3 and 4: the submission of the pending review, with the verdict.
pub const SUBMIT_REVIEW: &str = "mutation($input: SubmitPullRequestReviewInput!) { submitPullRequestReview(input: $input) { pullRequestReview { id state submittedAt url } } }";

/// Step 5: the resolution of a thread.
pub const RESOLVE_THREAD: &str = "mutation($input: ResolveReviewThreadInput!) { resolveReviewThread(input: $input) { thread { id isResolved } } }";

/// A GraphQL request body.
#[derive(Serialize)]
struct Request<'a, V> {
    query: &'a str,
    variables: V,
}

#[derive(Serialize)]
struct PullVariables<'a> {
    owner: &'a str,
    name: &'a str,
    number: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<&'a str>,
}

#[derive(Serialize)]
struct NodeVariables<'a> {
    id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<&'a str>,
}

#[derive(Serialize)]
struct Input<T> {
    input: T,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AddReviewInput<'a> {
    pull_request_id: &'a str,
    #[serde(rename = "commitOID")]
    commit_oid: &'a str,
    threads: Vec<DraftThread<'a>>,
}

/// GitHub `DraftPullRequestReviewThread`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DraftThread<'a> {
    path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_side: Option<&'static str>,
    line: u32,
    side: &'static str,
    body: &'a str,
}

impl<'a> From<&NewThread<'a>> for DraftThread<'a> {
    fn from(thread: &NewThread<'a>) -> Self {
        let anchor = thread.anchor;
        let side = match anchor.side {
            Side::Old => "LEFT",
            Side::New => "RIGHT",
        };
        // One line: no start. A range: both ends on the side of the anchor.
        let range = anchor.start_line < anchor.end_line;
        Self {
            path: &anchor.path,
            start_line: range.then_some(anchor.start_line),
            start_side: range.then_some(side),
            line: anchor.end_line,
            side,
            body: thread.body,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AddReplyInput<'a> {
    pull_request_review_id: &'a str,
    pull_request_review_thread_id: &'a str,
    body: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SubmitInput<'a> {
    pull_request_review_id: &'a str,
    event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResolveInput<'a> {
    thread_id: &'a str,
}

/// The GitHub `PullRequestReviewEvent` of a verdict. Pure.
const fn event(verdict: VerdictKind) -> &'static str {
    match verdict {
        VerdictKind::Approve => "APPROVE",
        VerdictKind::RequestChanges => "REQUEST_CHANGES",
        VerdictKind::Comment => "COMMENT",
    }
}

/// The `type` of the GraphQL error of the point-based rate limit.
const RATE_LIMITED: &str = "RATE_LIMITED";

/// `owner` and `name` of a GitHub key.
fn owner_name(key: &MrKey) -> Result<(&str, &str), ForgeError> {
    match key.forge() {
        loom_api::ForgeKind::Github => key
            .project()
            .split_once('/')
            .ok_or_else(|| ForgeError::Unexpected(format!("{key} has no owner"))),
        loom_api::ForgeKind::Gitlab => Err(ForgeError::Unexpected(format!(
            "{key} is not a GitHub pull request"
        ))),
    }
}

/// The error of a GraphQL answer with a success status: `errors` of type `RATE_LIMITED` are
/// the point-based rate limit; other `errors` are [`ForgeError::Graphql`] of `step`. Matches on
/// the `type`, never on the message.
fn graphql_error(step: GraphqlStep, answer: &Answer, error: ParseError) -> ForgeError {
    let ParseError::Graphql { errors, .. } = error else {
        return ForgeError::Parse(error.to_string());
    };
    if errors
        .iter()
        .any(|error| error.kind.as_deref() == Some(RATE_LIMITED))
    {
        let message = errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return ForgeError::RateLimited {
            status: answer.status,
            wait: rate_limit_wait(&answer.headers),
            message,
        };
    }
    ForgeError::Graphql { step, errors }
}

impl GithubClient {
    /// `POST` of a GraphQL request to the GraphQL endpoint, parsed by `parse`.
    fn graphql<V: Serialize, T>(
        &self,
        step: GraphqlStep,
        query: &str,
        variables: V,
        parse: impl FnOnce(&[u8]) -> Result<T, ParseError>,
    ) -> Result<T, ForgeError> {
        let url = self.graphql.as_deref().ok_or_else(|| {
            ForgeError::Unexpected("this GitHub host has no GraphQL endpoint".to_owned())
        })?;
        let body = serde_json::to_vec(&Request { query, variables })
            .map_err(|error| ForgeError::Unexpected(format!("GraphQL request: {error}")))?;
        let answer = self.http.send(
            "POST",
            url,
            &[],
            Some(Body {
                content_type: "application/json",
                bytes: &body,
            }),
        )?;
        if !(200..300).contains(&answer.status) {
            return Err(self.http.error(&answer));
        }
        parse(&answer.body).map_err(|error| self.redacted(graphql_error(step, &answer, error)))
    }

    /// `error` with the token redacted from every text: a body can echo it.
    fn redacted(&self, error: ForgeError) -> ForgeError {
        match error {
            ForgeError::Graphql { step, errors } => ForgeError::Graphql {
                step,
                errors: errors
                    .into_iter()
                    .map(|mut error| {
                        error.message = self.http.redact(&error.message);
                        error.path = error
                            .path
                            .iter()
                            .map(|part| self.http.redact(part))
                            .collect();
                        error
                    })
                    .collect(),
            },
            ForgeError::RateLimited {
                status,
                wait,
                message,
            } => ForgeError::RateLimited {
                status,
                wait,
                message: self.http.redact(&message),
            },
            ForgeError::Parse(text) => ForgeError::Parse(self.http.redact(&text)),
            // Built from an error status, already redacted by `Http::error`, or by loom.
            error @ (ForgeError::Unauthorized { .. }
            | ForgeError::SsoRequired { .. }
            | ForgeError::Forbidden { .. }
            | ForgeError::NotFound { .. }
            | ForgeError::Unprocessable { .. }
            | ForgeError::Status { .. }
            | ForgeError::Transport(_)
            | ForgeError::Unexpected(_)) => error,
        }
    }

    pub(super) fn graphql_review_threads(&self, key: &MrKey) -> Result<ReviewThreads, ForgeError> {
        let (owner, name) = owner_name(key)?;
        let mut cursor: Option<String> = None;
        let mut all: Option<ReviewThreads> = None;
        for _ in 0..MAX_PAGES {
            let page = self.graphql(
                GraphqlStep::ReviewThreads,
                REVIEW_THREADS,
                PullVariables {
                    owner,
                    name,
                    number: key.number(),
                    cursor: cursor.as_deref(),
                },
                crate::github::parse_review_threads,
            )?;
            let mut threads = page.threads;
            for thread in &mut threads {
                if page.threads_with_more_comments.contains(&thread.id) {
                    thread.comments = self.thread_comments(&thread.id)?;
                }
            }
            let mut joined = all.take().unwrap_or_else(|| ReviewThreads {
                pull_request_id: page.pull_request_id,
                head_sha: page.head_sha,
                threads: Vec::new(),
            });
            joined.threads.extend(threads);
            match page.next_cursor {
                Some(next) => {
                    cursor = Some(next);
                    all = Some(joined);
                }
                None => return Ok(joined),
            }
        }
        Err(ForgeError::Unexpected(format!(
            "more than {MAX_PAGES} pages of review threads"
        )))
    }

    /// Every submitted comment of the thread `id`, from its first page.
    fn thread_comments(&self, id: &str) -> Result<Vec<crate::model::Comment>, ForgeError> {
        let mut cursor: Option<String> = None;
        let mut comments = Vec::new();
        for _ in 0..MAX_PAGES {
            let page = self.graphql(
                GraphqlStep::ThreadComments,
                THREAD_COMMENTS,
                NodeVariables {
                    id,
                    cursor: cursor.as_deref(),
                },
                crate::github::parse_thread_comments,
            )?;
            comments.extend(page.comments);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(comments),
            }
        }
        Err(ForgeError::Unexpected(format!(
            "more than {MAX_PAGES} pages of comments in thread {id}"
        )))
    }

    pub(super) fn graphql_pending_reviews(
        &self,
        key: &MrKey,
    ) -> Result<Vec<crate::github::PendingReview>, ForgeError> {
        let (owner, name) = owner_name(key)?;
        self.graphql(
            GraphqlStep::PendingReviews,
            PENDING_REVIEWS,
            PullVariables {
                owner,
                name,
                number: key.number(),
                cursor: None,
            },
            crate::github::parse_pending_reviews,
        )
    }

    pub(super) fn graphql_add_review(
        &self,
        pull_request_id: &str,
        commit: &Sha,
        threads: &[NewThread<'_>],
    ) -> Result<String, ForgeError> {
        self.graphql(
            GraphqlStep::AddReview,
            ADD_REVIEW,
            Input {
                input: AddReviewInput {
                    pull_request_id,
                    commit_oid: commit.as_str(),
                    threads: threads.iter().map(DraftThread::from).collect(),
                },
            },
            crate::github::parse_add_review,
        )
    }

    pub(super) fn graphql_add_reply(
        &self,
        review_id: &str,
        thread_id: &str,
        body: &str,
    ) -> Result<String, ForgeError> {
        self.graphql(
            GraphqlStep::AddReply,
            ADD_REPLY,
            Input {
                input: AddReplyInput {
                    pull_request_review_id: review_id,
                    pull_request_review_thread_id: thread_id,
                    body,
                },
            },
            crate::github::parse_add_reply,
        )
    }

    pub(super) fn graphql_submit_review(
        &self,
        review_id: &str,
        verdict: VerdictKind,
        body: Option<&str>,
    ) -> Result<crate::github::SubmittedReview, ForgeError> {
        self.graphql(
            GraphqlStep::SubmitReview,
            SUBMIT_REVIEW,
            Input {
                input: SubmitInput {
                    pull_request_review_id: review_id,
                    event: event(verdict),
                    body,
                },
            },
            crate::github::parse_submit_review,
        )
    }

    pub(super) fn graphql_resolve_thread(&self, thread_id: &str) -> Result<String, ForgeError> {
        self.graphql(
            GraphqlStep::ResolveThread,
            RESOLVE_THREAD,
            Input {
                input: ResolveInput { thread_id },
            },
            crate::github::parse_resolve_thread,
        )
    }
}

/// The review threads of a pull request (GraphQL), every page read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThreads {
    /// The GraphQL node ID of the pull request.
    pub pull_request_id: String,
    /// The current head SHA of the pull request.
    pub head_sha: Sha,
    /// The threads, in the order of GitHub, each with all its submitted comments.
    pub threads: Vec<Thread>,
}

#[cfg(test)]
mod tests {
    use super::{DraftThread, event};
    use crate::client::NewThread;
    use crate::model::{Anchor, Sha, Side, VerdictKind};

    fn anchor(side: Side, start_line: u32, end_line: u32) -> Anchor {
        Anchor {
            path: "src/auth/jwt.rs".to_owned(),
            side,
            start_line,
            end_line,
            sha: Sha::try_from("84280c47b6aa128c645b1fbea54aded8b32e8463".to_owned()).unwrap(),
        }
    }

    fn draft(anchor: &Anchor) -> serde_json::Value {
        serde_json::to_value(DraftThread::from(&NewThread { anchor, body: "b" })).unwrap()
    }

    #[test]
    fn draft_threads_of_lines_and_ranges() {
        assert_eq!(
            draft(&anchor(Side::New, 14, 14)),
            serde_json::json!({"path": "src/auth/jwt.rs", "line": 14, "side": "RIGHT", "body": "b"})
        );
        assert_eq!(
            draft(&anchor(Side::Old, 9, 11)),
            serde_json::json!({
                "path": "src/auth/jwt.rs", "startLine": 9, "startSide": "LEFT",
                "line": 11, "side": "LEFT", "body": "b"
            })
        );
    }

    #[test]
    fn verdict_events() {
        assert_eq!(event(VerdictKind::Approve), "APPROVE");
        assert_eq!(event(VerdictKind::RequestChanges), "REQUEST_CHANGES");
        assert_eq!(event(VerdictKind::Comment), "COMMENT");
    }
}
