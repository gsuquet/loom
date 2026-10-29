//! The GitHub client: REST here, GraphQL in `graphql`.

use loom_api::ForgeKind;

use super::http::{Answer, Http, MAX_PAGES, next_link};
use super::{
    ApiBase, Conditional, Forge, ForgeError, GithubForge, NewThread, ReviewRequests, ReviewThreads,
    Token, TokenUser,
};
use crate::error::ParseError;
use crate::github::{
    PendingReview, Review, SearchPage, SubmittedReview, parse_files, parse_issue_comments,
    parse_pull, parse_reviews, parse_search, parse_user,
};
use crate::model::{FileDiff, Mr, MrKey, Sha, Thread, VerdictKind};

/// A GitHub client: one token on one host.
#[derive(Debug)]
pub struct GithubClient {
    pub(super) http: Http,
    pub(super) graphql: Option<String>,
}

impl GithubClient {
    /// A client of the API at `base` with `token`. The token is sent to the REST base (and the
    /// GraphQL endpoint) only.
    #[must_use]
    pub fn new(base: &ApiBase, token: Token, user_agent: &str) -> Self {
        Self {
            http: Http::new(
                ForgeKind::Github,
                &base.rest,
                base.graphql.as_deref(),
                token,
                user_agent,
            ),
            graphql: base.graphql.clone(),
        }
    }

    /// `GET path` with a success status.
    fn get(&self, path_and_query: &str) -> Result<Answer, ForgeError> {
        let answer = self
            .http
            .send("GET", &self.http.url(path_and_query), &[], None)?;
        if (200..300).contains(&answer.status) {
            Ok(answer)
        } else {
            Err(self.http.error(&answer))
        }
    }

    /// Every page of a list from `path_and_query`, each parsed by `parse` and joined by
    /// `join`. The next page is the `rel="next"` URL of the `link` header, on this host only.
    fn pages<T>(
        &self,
        path_and_query: &str,
        mut parse: impl FnMut(&[u8]) -> Result<T, ParseError>,
        mut join: impl FnMut(T),
    ) -> Result<(), ForgeError> {
        let mut url = self.http.url(path_and_query);
        for _ in 0..MAX_PAGES {
            let answer = self.http.send("GET", &url, &[], None)?;
            if !(200..300).contains(&answer.status) {
                return Err(self.http.error(&answer));
            }
            join(parse(&answer.body).map_err(|error| self.http.parse_error(&error))?);
            let Some(next) = answer.headers.get("link").and_then(next_link) else {
                return Ok(());
            };
            if !self.http.allowed(&next) {
                return Err(ForgeError::Unexpected(format!(
                    "the next page is not on the API host: {}",
                    self.http.redact(&next)
                )));
            }
            url = next;
        }
        Err(ForgeError::Unexpected(format!(
            "more than {MAX_PAGES} pages from {path_and_query}"
        )))
    }

    /// Every page of a JSON list.
    fn list<T>(
        &self,
        path_and_query: &str,
        parse: impl FnMut(&[u8]) -> Result<Vec<T>, ParseError>,
    ) -> Result<Vec<T>, ForgeError> {
        let mut all = Vec::new();
        self.pages(path_and_query, parse, |page| all.extend(page))?;
        Ok(all)
    }
}

/// `/repos/<owner>/<repo>` of a GitHub key.
fn repo_path(key: &MrKey) -> Result<String, ForgeError> {
    match key.forge() {
        ForgeKind::Github => Ok(format!("/repos/{}", key.project())),
        ForgeKind::Gitlab => Err(ForgeError::Unexpected(format!(
            "{key} is not a GitHub pull request"
        ))),
    }
}

/// The search query of the pull requests to review. Pure.
fn search_query(requests: ReviewRequests) -> String {
    let qualifier = match requests {
        ReviewRequests::Teams => "review-requested",
        ReviewRequests::Direct => "user-review-requested",
    };
    format!("/search/issues?q=is:pr+is:open+archived:false+{qualifier}:@me&per_page=100")
}

impl Forge for GithubClient {
    fn user(&self) -> Result<TokenUser, ForgeError> {
        let answer = self.get("/user")?;
        Ok(TokenUser {
            user: parse_user(&answer.body).map_err(|error| self.http.parse_error(&error))?,
            scopes_header: answer.headers.get("x-oauth-scopes").map(str::to_owned),
        })
    }
}

impl GithubForge for GithubClient {
    fn repository(&self, project: &str) -> Result<loom_api::ForgeProject, ForgeError> {
        self.graphql_repository(project)
    }

    fn review_requests(&self, requests: ReviewRequests) -> Result<SearchPage, ForgeError> {
        let mut all = SearchPage {
            total_count: 0,
            incomplete_results: false,
            items: Vec::new(),
        };
        self.pages(&search_query(requests), parse_search, |page| {
            all.total_count = page.total_count;
            all.incomplete_results |= page.incomplete_results;
            // Results can move between pages while they are read: each MR once.
            for item in page.items {
                if !all.items.iter().any(|seen| seen.key == item.key) {
                    all.items.push(item);
                }
            }
        })?;
        Ok(all)
    }

    fn pull(&self, key: &MrKey, etag: Option<&str>) -> Result<Conditional<Mr>, ForgeError> {
        let url = self
            .http
            .url(&format!("{}/pulls/{}", repo_path(key)?, key.number()));
        let headers: Vec<(&str, &str)> = etag
            .map(|etag| ("If-None-Match", etag))
            .into_iter()
            .collect();
        let answer = self.http.send("GET", &url, &headers, None)?;
        if answer.status == 304 && etag.is_some() {
            return Ok(Conditional::NotModified);
        }
        if !(200..300).contains(&answer.status) {
            return Err(self.http.error(&answer));
        }
        Ok(Conditional::Modified {
            value: parse_pull(&answer.body).map_err(|error| self.http.parse_error(&error))?,
            etag: answer.headers.get("etag").map(str::to_owned),
        })
    }

    fn files(&self, key: &MrKey) -> Result<Vec<FileDiff>, ForgeError> {
        self.list(
            &format!(
                "{}/pulls/{}/files?per_page=100",
                repo_path(key)?,
                key.number()
            ),
            parse_files,
        )
    }

    fn reviews(&self, key: &MrKey) -> Result<Vec<Review>, ForgeError> {
        self.list(
            &format!(
                "{}/pulls/{}/reviews?per_page=100",
                repo_path(key)?,
                key.number()
            ),
            parse_reviews,
        )
    }

    fn issue_comments(&self, key: &MrKey) -> Result<Vec<Thread>, ForgeError> {
        self.list(
            &format!(
                "{}/issues/{}/comments?per_page=100",
                repo_path(key)?,
                key.number()
            ),
            parse_issue_comments,
        )
    }

    fn review_threads(&self, key: &MrKey) -> Result<ReviewThreads, ForgeError> {
        self.graphql_review_threads(key)
    }

    fn pending_reviews(&self, key: &MrKey) -> Result<Vec<PendingReview>, ForgeError> {
        self.graphql_pending_reviews(key)
    }

    fn add_review(
        &self,
        pull_request_id: &str,
        commit: &Sha,
        threads: &[NewThread<'_>],
    ) -> Result<String, ForgeError> {
        self.graphql_add_review(pull_request_id, commit, threads)
    }

    fn add_reply(
        &self,
        review_id: &str,
        thread_id: &str,
        body: &str,
    ) -> Result<String, ForgeError> {
        self.graphql_add_reply(review_id, thread_id, body)
    }

    fn submit_review(
        &self,
        review_id: &str,
        verdict: VerdictKind,
        body: Option<&str>,
    ) -> Result<SubmittedReview, ForgeError> {
        self.graphql_submit_review(review_id, verdict, body)
    }

    fn resolve_thread(&self, thread_id: &str) -> Result<String, ForgeError> {
        self.graphql_resolve_thread(thread_id)
    }
}

#[cfg(test)]
mod tests {
    use super::search_query;
    use crate::client::ReviewRequests;

    #[test]
    fn search_queries() {
        assert_eq!(
            search_query(ReviewRequests::Teams),
            "/search/issues?q=is:pr+is:open+archived:false+review-requested:@me&per_page=100"
        );
        assert_eq!(
            search_query(ReviewRequests::Direct),
            "/search/issues?q=is:pr+is:open+archived:false+user-review-requested:@me&per_page=100"
        );
    }
}
