//! The forge clients (ADR-0016, decision 7): the only module of `loom-forge` with I/O.
//!
//! Blocking calls on `ureq`; loomd runs them on its blocking pool. The decisions on a response
//! (error variant, rate limit wait, next page) are pure functions of its status and headers,
//! tested without a server.

mod github;
mod gitlab;
mod graphql;
mod http;

use std::fmt;
use std::time::Duration;

use loom_api::{ForgeKind, Timestamp};
use thiserror::Error;

use crate::error::GraphqlError;
use crate::github::{PendingReview, Review, SearchPage, SubmittedReview};
use crate::gitlab::request::{BulkPublishBody, DraftNoteBody};
use crate::gitlab::{Approvals, DraftNote, TokenInfo};
use crate::model::{
    Anchor, FileDiff, ForgeUser, Mr, MrKey, MrSummary, MrVersion, Sha, Thread, VerdictKind,
};

pub use github::GithubClient;
pub use gitlab::GitlabClient;
pub use graphql::ReviewThreads;

/// The API endpoints of a forge host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiBase {
    /// Base of the REST API, without a trailing `/`.
    pub rest: String,
    /// The GraphQL endpoint (GitHub only).
    pub graphql: Option<String>,
}

/// The API of `host`: `api.github.com` for `github.com`, `/api/v3` and `/api/graphql` on a
/// GitHub Enterprise Server, `/api/v4` on GitLab. Pure.
#[must_use]
pub fn api_base(kind: ForgeKind, host: &str) -> ApiBase {
    match kind {
        ForgeKind::Github if host == "github.com" => ApiBase {
            rest: "https://api.github.com".to_owned(),
            graphql: Some("https://api.github.com/graphql".to_owned()),
        },
        ForgeKind::Github => ApiBase {
            rest: format!("https://{host}/api/v3"),
            graphql: Some(format!("https://{host}/api/graphql")),
        },
        ForgeKind::Gitlab => ApiBase {
            rest: format!("https://{host}/api/v4"),
            graphql: None,
        },
    }
}

/// A forge token. Its `Debug` and `Display` never show the value, so that it cannot reach a
/// log, a span or an error by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// A token from its value.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    /// The value, for the `Authorization` header only.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// `text` with every occurrence of the token replaced by `<redacted>`.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        if self.0.is_empty() {
            return text.to_owned();
        }
        text.replace(&self.0, REDACTED)
    }
}

/// What replaces a token in a text.
pub const REDACTED: &str = "<redacted>";

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Token({REDACTED})")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// The authorization URL of an `x-github-sso` header (`required; url=<url>`). `None` when the
/// header gives none. Pure.
#[must_use]
pub fn sso_url(header: &str) -> Option<String> {
    header
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("url="))
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
}

/// How long to wait before the next call to a host that limits the rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitWait {
    /// Wait this long (`retry-after`).
    After(Duration),
    /// Wait until this instant (`x-ratelimit-reset`).
    Until(Timestamp),
    /// The forge gives no time: wait at least one minute, as GitHub asks.
    Unknown,
}

impl RateLimitWait {
    /// The wait from `now`: zero when the reset is past, one minute when unknown. Pure.
    #[must_use]
    pub const fn duration(self, now: Timestamp) -> Duration {
        match self {
            Self::After(wait) => wait,
            Self::Until(reset) => {
                Duration::from_secs(reset.unix_secs().saturating_sub(now.unix_secs()))
            }
            Self::Unknown => Duration::from_secs(60),
        }
    }
}

/// Why a forge call failed. No variant contains the token: every text from the forge or the
/// transport is redacted.
#[derive(Debug, Error)]
pub enum ForgeError {
    /// 400 (GitLab): the forge refused the request, such as a draft note on a line outside the
    /// diff.
    #[error("the forge refused the request (400): {message}")]
    BadRequest {
        /// The message of the forge, for humans.
        message: String,
        /// The validation errors, for humans.
        details: Vec<String>,
    },
    /// 401: the token is not valid, or expired. Resolve it again.
    #[error("the forge refused the token (401): {message}")]
    Unauthorized {
        /// The message of the forge, for humans.
        message: String,
    },
    /// 429, or 403 with no remaining requests or with `retry-after`: wait, then try again.
    #[error("the forge limits the rate of requests ({status}): {message}")]
    RateLimited {
        /// 403 or 429.
        status: u16,
        /// How long to wait.
        wait: RateLimitWait,
        /// The message of the forge, for humans.
        message: String,
    },
    /// 403 with `x-github-sso`: the organization asks for a SAML SSO authorization of the
    /// token.
    #[error("the organization asks for an SSO authorization of the token (403)")]
    SsoRequired {
        /// The URL that authorizes the token, when GitHub gives one.
        url: Option<String>,
    },
    /// 403 with the GitLab code `insufficient_scope`: the token cannot write, such as a
    /// `read_api` token.
    #[error("the token does not have the scope of this request (403): {message}")]
    InsufficientScope {
        /// The scope that the request needs, when the forge gives it, such as `api`.
        scope: Option<String>,
        /// The description of the forge, for humans.
        message: String,
    },
    /// Another 403: the token cannot do this.
    #[error("the forge refused the request (403): {message}")]
    Forbidden {
        /// The message of the forge, for humans.
        message: String,
    },
    /// 404: no such resource, or the token cannot see it.
    #[error("not found on the forge (404): {message}")]
    NotFound {
        /// The message of the forge, for humans.
        message: String,
    },
    /// 409 (GitLab): the request is not valid for the current state, such as an approval
    /// with a `sha` that is not the head of the merge request.
    #[error("the forge refused the request as in conflict (409): {message}")]
    Conflict {
        /// The message of the forge, for humans.
        message: String,
    },
    /// 422: the forge refused the request as not valid.
    #[error("the forge refused the request as not valid (422): {message}")]
    Unprocessable {
        /// The message of the forge, for humans.
        message: String,
        /// The validation errors, for humans.
        details: Vec<String>,
    },
    /// Another error status.
    #[error("the forge answered {status}: {message}")]
    Status {
        /// The HTTP status.
        status: u16,
        /// The message of the forge, for humans.
        message: String,
    },
    /// No answer, or an answer that cannot be read.
    #[error("no usable answer from the forge: {0}")]
    Transport(String),
    /// A GraphQL answer with `errors` (HTTP 200): the step failed. Code matches on the `type`
    /// of the errors and on the step, never on the messages. The messages are redacted.
    #[error("GitHub refused the {step} step: {}", list(errors))]
    Graphql {
        /// The call that failed.
        step: GraphqlStep,
        /// The errors, as GitHub gives them.
        errors: Vec<GraphqlError>,
    },
    /// A success answer whose body is not the expected payload. The text of the
    /// [`ParseError`](crate::ParseError), redacted.
    #[error("{0}")]
    Parse(String),
    /// A success answer that loom does not follow, such as a next page on another host.
    #[error("unexpected answer from the forge: {0}")]
    Unexpected(String),
}

fn list(errors: &[GraphqlError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// A GitHub GraphQL call, named in [`ForgeError::Graphql`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphqlStep {
    /// The review threads of a pull request.
    ReviewThreads,
    /// The comments of one review thread.
    ThreadComments,
    /// The pending reviews of the viewer.
    PendingReviews,
    /// `addPullRequestReview`: the pending review with the new threads.
    AddReview,
    /// `addPullRequestReviewThreadReply`.
    AddReply,
    /// `submitPullRequestReview`.
    SubmitReview,
    /// `resolveReviewThread`.
    ResolveThread,
}

impl fmt::Display for GraphqlStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ReviewThreads => "reviewThreads",
            Self::ThreadComments => "node(PullRequestReviewThread).comments",
            Self::PendingReviews => "reviews(states: PENDING)",
            Self::AddReview => "addPullRequestReview",
            Self::AddReply => "addPullRequestReviewThreadReply",
            Self::SubmitReview => "submitPullRequestReview",
            Self::ResolveThread => "resolveReviewThread",
        })
    }
}

/// A new thread of a pending review: the lines and the body as published (label,
/// decorations and suggestion included).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewThread<'a> {
    /// The lines. A range has both ends on the side of the anchor.
    pub anchor: &'a Anchor,
    /// The body, in Markdown.
    pub body: &'a str,
}

/// The answer to a conditional request (`If-None-Match`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conditional<T> {
    /// The resource changed, or no `etag` was sent.
    Modified {
        /// The resource.
        value: T,
        /// The `etag` to send next time, when the forge gives one.
        etag: Option<String>,
    },
    /// 304: the resource did not change since the `etag`. GitHub does not count it in the
    /// rate limit.
    NotModified,
}

/// The user of a token (`GET /user`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenUser {
    /// The user.
    pub user: ForgeUser,
    /// The `x-oauth-scopes` header (GitHub classic and OAuth tokens only).
    pub scopes_header: Option<String>,
}

/// Which review requests the GitHub search finds (`review_requests` of a forge host).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewRequests {
    /// Requests to me and to my teams (`review-requested:@me`).
    #[default]
    Teams,
    /// Requests to me only (`user-review-requested:@me`).
    Direct,
}

/// The calls of every forge. Blocking.
pub trait Forge {
    /// `GET /user`: the user of the token.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn user(&self) -> Result<TokenUser, ForgeError>;
}

/// The GitHub REST reads. Blocking. Every list follows its pages.
pub trait GithubForge: Forge {
    /// The open pull requests where I am a requested reviewer (search).
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn review_requests(&self, requests: ReviewRequests) -> Result<SearchPage, ForgeError>;

    /// The pull request of `key`; with `etag`, [`Conditional::NotModified`] when it did not
    /// change.
    ///
    /// # Errors
    ///
    /// [`ForgeError`]; [`ForgeError::Unexpected`] for a key that is not of GitHub.
    fn pull(&self, key: &MrKey, etag: Option<&str>) -> Result<Conditional<Mr>, ForgeError>;

    /// The changed files of the pull request, with their patches.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn files(&self, key: &MrKey) -> Result<Vec<FileDiff>, ForgeError>;

    /// The reviews of the pull request.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn reviews(&self, key: &MrKey) -> Result<Vec<Review>, ForgeError>;

    /// The general comments of the pull request, one thread each.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn issue_comments(&self, key: &MrKey) -> Result<Vec<Thread>, ForgeError>;

    /// The review threads of the pull request (GraphQL), every page of threads and of
    /// comments read.
    ///
    /// # Errors
    ///
    /// [`ForgeError`]; [`ForgeError::Graphql`] with [`GraphqlStep::ReviewThreads`] or
    /// [`GraphqlStep::ThreadComments`].
    fn review_threads(&self, key: &MrKey) -> Result<ReviewThreads, ForgeError>;

    /// The pending reviews of the viewer on the pull request (GraphQL), with their commit and
    /// comments. At most one: GitHub allows one pending review per user and pull request.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn pending_reviews(&self, key: &MrKey) -> Result<Vec<PendingReview>, ForgeError>;

    /// Publish step 1: a pending review on `commit` with the new `threads`, every one anchored
    /// on `commit`. Returns the ID of the review. Nothing is visible to others yet.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Unexpected`], before any request, for a thread anchored on another
    /// commit; [`ForgeError::Graphql`] with [`GraphqlStep::AddReview`] when the state
    /// of the pull request changed since the checks (another pending review, a line outside
    /// the diff).
    fn add_review(
        &self,
        pull_request_id: &str,
        commit: &Sha,
        threads: &[NewThread<'_>],
    ) -> Result<String, ForgeError>;

    /// Publish step 2: a reply to the thread `thread_id`, in the pending review `review_id`.
    /// Returns the ID of the comment.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn add_reply(&self, review_id: &str, thread_id: &str, body: &str)
    -> Result<String, ForgeError>;

    /// Publish steps 3 and 4: the submission of the pending review with the verdict and the
    /// summary. The review becomes visible.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn submit_review(
        &self,
        review_id: &str,
        verdict: VerdictKind,
        body: Option<&str>,
    ) -> Result<SubmittedReview, ForgeError>;

    /// Publish step 5: the resolution of the thread `thread_id`. Returns its ID.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn resolve_thread(&self, thread_id: &str) -> Result<String, ForgeError>;
}

/// A GitLab merge request in the REST paths: `/projects/<project_id>/merge_requests/<iid>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitlabMr {
    /// The ID of the target project, such as `77`.
    pub project_id: u64,
    /// The number of the merge request in its project, such as `88` for `!88`.
    pub iid: u64,
}

/// The GitLab REST calls of the review. Blocking. Every list follows its pages.
pub trait GitlabForge: Forge {
    /// `GET /personal_access_tokens/self`: the scopes of the token. It fails for a token that
    /// is not a personal access token, such as the OAuth token of `glab`.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn token_scopes(&self) -> Result<TokenInfo, ForgeError>;

    /// The open merge requests where I am a reviewer (`scope=reviews_for_me`), changed after
    /// `updated_after` when it is given.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn review_requests(
        &self,
        updated_after: Option<Timestamp>,
    ) -> Result<Vec<MrSummary>, ForgeError>;

    /// The merge request; with `etag`, [`Conditional::NotModified`] when it did not change.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn merge_request(
        &self,
        mr: GitlabMr,
        etag: Option<&str>,
    ) -> Result<Conditional<Mr>, ForgeError>;

    /// The versions of the merge request, newest first.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn versions(&self, mr: GitlabMr) -> Result<Vec<MrVersion>, ForgeError>;

    /// The diffs of the files of the current version.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn diffs(&self, mr: GitlabMr) -> Result<Vec<FileDiff>, ForgeError>;

    /// The discussions, without system notes. `head` is the current head SHA: a discussion on
    /// another head is outdated.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn discussions(&self, mr: GitlabMr, head: &Sha) -> Result<Vec<Thread>, ForgeError>;

    /// My draft notes on the merge request, those made in the web UI included.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn draft_notes(&self, mr: GitlabMr) -> Result<Vec<DraftNote>, ForgeError>;

    /// Creates a draft note, visible only to me until [`Self::bulk_publish`].
    ///
    /// # Errors
    ///
    /// [`ForgeError`]; [`ForgeError::BadRequest`] for a line outside the diff,
    /// [`ForgeError::InsufficientScope`] for a token without `api`.
    fn create_draft_note(
        &self,
        mr: GitlabMr,
        body: &DraftNoteBody,
    ) -> Result<DraftNote, ForgeError>;

    /// Publishes all my draft notes, with the summary and the reviewer state of `body`.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn bulk_publish(&self, mr: GitlabMr, body: &BulkPublishBody) -> Result<(), ForgeError>;

    /// Approves the merge request at the head `sha`.
    ///
    /// # Errors
    ///
    /// [`ForgeError`]; [`ForgeError::Conflict`] when `sha` is not the head any more.
    fn approve(&self, mr: GitlabMr, sha: &Sha) -> Result<Approvals, ForgeError>;

    /// The approvals of the merge request.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn approvals(&self, mr: GitlabMr) -> Result<Approvals, ForgeError>;

    /// Resolves the discussion `discussion_id`.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn resolve_discussion(&self, mr: GitlabMr, discussion_id: &str) -> Result<(), ForgeError>;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use loom_api::{ForgeKind, Timestamp};

    use super::{REDACTED, RateLimitWait, Token, api_base, sso_url};

    #[test]
    fn api_bases_of_public_and_self_hosted_forges() {
        let github = api_base(ForgeKind::Github, "github.com");
        assert_eq!(github.rest, "https://api.github.com");
        assert_eq!(
            github.graphql.as_deref(),
            Some("https://api.github.com/graphql")
        );
        let enterprise = api_base(ForgeKind::Github, "github.company-b.example");
        assert_eq!(enterprise.rest, "https://github.company-b.example/api/v3");
        assert_eq!(
            enterprise.graphql.as_deref(),
            Some("https://github.company-b.example/api/graphql")
        );
        let gitlab = api_base(ForgeKind::Gitlab, "gitlab.company-a.example");
        assert_eq!(gitlab.rest, "https://gitlab.company-a.example/api/v4");
        assert_eq!(gitlab.graphql, None);
        assert_eq!(
            api_base(ForgeKind::Gitlab, "gitlab.com").rest,
            "https://gitlab.com/api/v4"
        );
    }

    #[test]
    fn a_token_never_prints_its_value() {
        let token = Token::new("ghp_secret".to_owned());
        assert_eq!(format!("{token:?}"), format!("Token({REDACTED})"));
        assert_eq!(token.to_string(), REDACTED);
        assert_eq!(
            token.redact("bad ghp_secret, ghp_secret"),
            format!("bad {REDACTED}, {REDACTED}")
        );
        assert_eq!(Token::new(String::new()).redact("text"), "text");
    }

    #[test]
    fn sso_urls() {
        assert_eq!(
            sso_url("required; url=https://github.com/orgs/acme/sso?authorization_request=x"),
            Some("https://github.com/orgs/acme/sso?authorization_request=x".to_owned())
        );
        assert_eq!(sso_url("partial-results; organizations=21955855"), None);
        assert_eq!(sso_url("required; url="), None);
    }

    #[test]
    fn rate_limit_waits_from_now() {
        let now = Timestamp::from_unix_secs(1_000).unwrap();
        let wait = |wait: RateLimitWait| wait.duration(now);
        assert_eq!(
            wait(RateLimitWait::After(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
        let reset = Timestamp::from_unix_secs(1_090).unwrap();
        assert_eq!(wait(RateLimitWait::Until(reset)), Duration::from_secs(90));
        let past = Timestamp::from_unix_secs(900).unwrap();
        assert_eq!(wait(RateLimitWait::Until(past)), Duration::ZERO);
        assert_eq!(wait(RateLimitWait::Unknown), Duration::from_secs(60));
    }
}
