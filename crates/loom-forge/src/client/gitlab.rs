//! The GitLab REST client.

use loom_api::{ForgeKind, ForgeProject, Timestamp};
use serde::Serialize;

use super::http::{Answer, Body, Headers, Http, MAX_PAGES, next_link};
use super::{
    ApiBase, Conditional, Forge, ForgeError, GitlabForge, GitlabMr, OpenForge, Token, TokenUser,
};
use crate::error::ParseError;
use crate::gitlab::request::{ApproveBody, BulkPublishBody, DraftNoteBody};
use crate::gitlab::{
    Approvals, DraftNote, TokenInfo, parse_approvals, parse_branch_merge_requests, parse_diffs,
    parse_discussions, parse_draft_note, parse_draft_notes, parse_merge_request,
    parse_merge_request_summary, parse_merge_requests, parse_project, parse_token, parse_user,
    parse_versions,
};
use crate::model::{FileDiff, Mr, MrSummary, MrVersion, Sha, Thread};
use crate::open::{NewMr, create_request, find_request};

/// The largest page that GitLab gives.
const MAX_PER_PAGE: u8 = 100;

/// A GitLab client: one token on one host.
#[derive(Debug)]
pub struct GitlabClient {
    http: Http,
    per_page: u8,
}

impl GitlabClient {
    /// A client of the API at `base` (`https://<host>/api/v4`) with `token`, sent to that base
    /// only. Lists are read 100 items per page.
    #[must_use]
    pub fn new(base: &ApiBase, token: Token, user_agent: &str) -> Self {
        Self {
            http: Http::new(ForgeKind::Gitlab, &base.rest, None, token, user_agent),
            per_page: MAX_PER_PAGE,
        }
    }

    /// The same client with `per_page` items per page of a list, from 1 to 100 (a value out
    /// of that range is moved into it). For tests of the pages.
    #[must_use]
    pub fn with_per_page(mut self, per_page: u8) -> Self {
        self.per_page = per_page.clamp(1, MAX_PER_PAGE);
        self
    }

    /// The answer when its status is a success, otherwise its error.
    fn success(&self, answer: Answer) -> Result<Answer, ForgeError> {
        if (200..300).contains(&answer.status) {
            Ok(answer)
        } else {
            Err(self.http.error(&answer))
        }
    }

    /// `GET path` with a success status.
    fn get(&self, path_and_query: &str) -> Result<Answer, ForgeError> {
        let answer = self
            .http
            .send("GET", &self.http.url(path_and_query), &[], None)?;
        self.success(answer)
    }

    /// `method path` with `body` as JSON, with a success status.
    fn send_json(
        &self,
        method: &str,
        path_and_query: &str,
        body: &impl Serialize,
    ) -> Result<Answer, ForgeError> {
        let bytes = serde_json::to_vec(body)
            .map_err(|error| ForgeError::Unexpected(format!("request body: {error}")))?;
        let answer = self.http.send(
            method,
            &self.http.url(path_and_query),
            &[],
            Some(Body {
                content_type: "application/json",
                bytes: &bytes,
            }),
        )?;
        self.success(answer)
    }

    /// `parse` of a success body, its error redacted.
    fn parse<T>(
        &self,
        body: &[u8],
        parse: impl FnOnce(&[u8]) -> Result<T, ParseError>,
    ) -> Result<T, ForgeError> {
        parse(body).map_err(|error| self.http.parse_error(&error))
    }

    /// Every page of a JSON list from `path_and_query`, each parsed by `parse`. The next page
    /// is `x-next-page` (empty on the last page), otherwise the `rel="next"` URL of the `link`
    /// header, on this host only.
    fn list<T>(
        &self,
        path_and_query: &str,
        mut parse: impl FnMut(&[u8]) -> Result<Vec<T>, ParseError>,
    ) -> Result<Vec<T>, ForgeError> {
        let mut all = Vec::new();
        let mut url = self.http.url(path_and_query);
        for _ in 0..MAX_PAGES {
            let answer = self.success(self.http.send("GET", &url, &[], None)?)?;
            all.extend(self.parse(&answer.body, &mut parse)?);
            let next = next_page(&answer.headers, &url).map_err(|value| {
                ForgeError::Unexpected(format!(
                    "x-next-page is not a page number: `{}`",
                    self.http.redact(&value)
                ))
            })?;
            let Some(next) = next else {
                return Ok(all);
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

    /// `/projects/<id>/merge_requests/<iid>` and `rest`, with `?per_page=` when `paged`.
    fn mr_path(&self, mr: GitlabMr, rest: &str, paged: bool) -> String {
        let path = format!(
            "/projects/{}/merge_requests/{}{rest}",
            mr.project_id, mr.iid
        );
        if paged {
            format!("{path}?per_page={}", self.per_page)
        } else {
            path
        }
    }
}

/// The next page of a GitLab list read from `url`: `x-next-page` first (empty: the last page),
/// then the `rel="next"` URL of `link`. `Err` with the value of an `x-next-page` that is not
/// a page number. Pure.
fn next_page(headers: &Headers, url: &str) -> Result<Option<String>, String> {
    if let Some(next) = headers.get("x-next-page") {
        let next = next.trim();
        if next.is_empty() {
            return Ok(None);
        }
        return match next.parse::<u32>() {
            Ok(page) if page > 0 => Ok(Some(with_page(url, page))),
            Ok(_) | Err(_) => Err(next.to_owned()),
        };
    }
    Ok(headers.get("link").and_then(next_link))
}

/// `url` with its `page` parameter set to `page`, added when it has none. Pure.
fn with_page(url: &str, page: u32) -> String {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let mut params: Vec<String> = query
        .split('&')
        .filter(|param| !param.is_empty() && !param.starts_with("page="))
        .map(str::to_owned)
        .collect();
    params.push(format!("page={page}"));
    format!("{path}?{}", params.join("&"))
}

/// The query of the merge requests to review. Pure.
fn review_requests_query(updated_after: Option<Timestamp>, per_page: u8) -> String {
    let after = updated_after.map_or_else(String::new, |time| format!("&updated_after={time}"));
    format!("/merge_requests?scope=reviews_for_me&state=opened{after}&per_page={per_page}")
}

/// `text` as one segment of a URL path: every byte but letters, digits, `-`, `_` and `~`
/// percent-encoded (`.` too, so that `..` is not a dot segment), so a discussion ID cannot
/// change the path. Pure.
fn path_segment(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

impl Forge for GitlabClient {
    fn user(&self) -> Result<TokenUser, ForgeError> {
        let answer = self.get("/user")?;
        Ok(TokenUser {
            user: self.parse(&answer.body, parse_user)?,
            scopes_header: None,
        })
    }
}

impl OpenForge for GitlabClient {
    fn open_mrs_of_branch(&self, project: &str, head: &str) -> Result<Vec<MrSummary>, ForgeError> {
        let request = find_request(ForgeKind::Gitlab, project, head);
        self.list(&request.path, parse_branch_merge_requests)
    }

    fn create_draft_mr(&self, mr: &NewMr) -> Result<MrSummary, ForgeError> {
        let request = create_request(ForgeKind::Gitlab, mr);
        let answer = self.send_json(request.method, &request.path, &request.body)?;
        self.parse(&answer.body, parse_merge_request_summary)
    }
}

/// The count of a GitLab list from its `x-total` header; `None` when absent (GitLab leaves it
/// out above 10,000 items) or not a number. Pure.
fn total(headers: &Headers) -> Option<u64> {
    headers.get("x-total")?.trim().parse().ok()
}

impl GitlabForge for GitlabClient {
    fn project(&self, path: &str) -> Result<ForgeProject, ForgeError> {
        let answer = self.get(&format!("/projects/{}", path_segment(path)))?;
        let mut project = self.parse(&answer.body, parse_project)?;
        // Merge requests disabled, or open to members only: the project is still known.
        project.open_mrs = match self.get(&format!(
            "/projects/{}/merge_requests?state=opened&per_page=1",
            project.id
        )) {
            Ok(open) => total(&open.headers),
            Err(ForgeError::Forbidden { .. } | ForgeError::NotFound { .. }) => None,
            Err(error) => return Err(error),
        };
        Ok(project)
    }

    fn token_scopes(&self) -> Result<TokenInfo, ForgeError> {
        let answer = self.get("/personal_access_tokens/self")?;
        self.parse(&answer.body, parse_token)
    }

    fn review_requests(
        &self,
        updated_after: Option<Timestamp>,
    ) -> Result<Vec<MrSummary>, ForgeError> {
        let all = self.list(
            &review_requests_query(updated_after, self.per_page),
            parse_merge_requests,
        )?;
        // Results can move between pages while they are read: each MR once.
        let mut unique: Vec<MrSummary> = Vec::with_capacity(all.len());
        for mr in all {
            if !unique.iter().any(|seen| seen.key == mr.key) {
                unique.push(mr);
            }
        }
        Ok(unique)
    }

    fn merge_request(
        &self,
        mr: GitlabMr,
        etag: Option<&str>,
    ) -> Result<Conditional<Mr>, ForgeError> {
        let url = self.http.url(&self.mr_path(mr, "", false));
        let headers: Vec<(&str, &str)> = etag
            .map(|etag| ("If-None-Match", etag))
            .into_iter()
            .collect();
        let answer = self.http.send("GET", &url, &headers, None)?;
        if answer.status == 304 && etag.is_some() {
            return Ok(Conditional::NotModified);
        }
        let answer = self.success(answer)?;
        Ok(Conditional::Modified {
            value: self.parse(&answer.body, parse_merge_request)?,
            etag: answer.headers.get("etag").map(str::to_owned),
        })
    }

    fn versions(&self, mr: GitlabMr) -> Result<Vec<MrVersion>, ForgeError> {
        self.list(&self.mr_path(mr, "/versions", true), parse_versions)
    }

    fn diffs(&self, mr: GitlabMr) -> Result<Vec<FileDiff>, ForgeError> {
        self.list(&self.mr_path(mr, "/diffs", true), parse_diffs)
    }

    fn discussions(&self, mr: GitlabMr, head: &Sha) -> Result<Vec<Thread>, ForgeError> {
        self.list(&self.mr_path(mr, "/discussions", true), |body| {
            parse_discussions(body, head)
        })
    }

    fn draft_notes(&self, mr: GitlabMr) -> Result<Vec<DraftNote>, ForgeError> {
        self.list(&self.mr_path(mr, "/draft_notes", true), parse_draft_notes)
    }

    fn create_draft_note(
        &self,
        mr: GitlabMr,
        body: &DraftNoteBody,
    ) -> Result<DraftNote, ForgeError> {
        let answer = self.send_json("POST", &self.mr_path(mr, "/draft_notes", false), body)?;
        self.parse(&answer.body, parse_draft_note)
    }

    fn bulk_publish(&self, mr: GitlabMr, body: &BulkPublishBody) -> Result<(), ForgeError> {
        self.send_json(
            "POST",
            &self.mr_path(mr, "/draft_notes/bulk_publish", false),
            body,
        )?;
        Ok(())
    }

    fn approve(&self, mr: GitlabMr, sha: &Sha) -> Result<Approvals, ForgeError> {
        let answer = self.send_json(
            "POST",
            &self.mr_path(mr, "/approve", false),
            &ApproveBody::new(sha.clone()),
        );
        match answer {
            Ok(answer) => self.parse(&answer.body, parse_approvals),
            // GitLab answers 401 when it refuses the approval itself: already approved, an
            // author without self-approval, an approval password. The approvals tell these
            // apart from a token that is not valid, which fails them with 401 too.
            Err(ForgeError::Unauthorized { message }) => {
                let approvals = self.approvals(mr)?;
                if approvals.user_has_approved {
                    Ok(approvals)
                } else {
                    Err(ForgeError::ApprovalRefused { message })
                }
            }
            Err(error) => Err(error),
        }
    }

    fn approvals(&self, mr: GitlabMr) -> Result<Approvals, ForgeError> {
        let answer = self.get(&self.mr_path(mr, "/approvals", false))?;
        self.parse(&answer.body, parse_approvals)
    }

    fn resolve_discussion(&self, mr: GitlabMr, discussion_id: &str) -> Result<(), ForgeError> {
        let path = self.mr_path(
            mr,
            &format!("/discussions/{}?resolved=true", path_segment(discussion_id)),
            false,
        );
        let answer = self.http.send("PUT", &self.http.url(&path), &[], None)?;
        self.success(answer)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use loom_api::Timestamp;

    use super::{Headers, next_page, path_segment, review_requests_query, total, with_page};

    #[test]
    fn open_merge_requests_from_x_total() {
        assert_eq!(total(&Headers::from([("x-total", "4")])), Some(4));
        assert_eq!(total(&Headers::from([("x-total", " 0 ")])), Some(0));
        // Left out above 10,000 items.
        assert_eq!(total(&Headers::from([("x-total-pages", "1")])), None);
        assert_eq!(total(&Headers::from([("x-total", "many")])), None);
    }

    const URL: &str = "https://h/api/v4/projects/77/merge_requests/88/discussions?per_page=2";

    #[test]
    fn x_next_page_wins_and_an_empty_one_stops() {
        let next = Headers::from([
            ("x-next-page", "2"),
            ("link", r#"<https://h/api/v4/other?page=9>; rel="next""#),
        ]);
        assert_eq!(next_page(&next, URL), Ok(Some(format!("{URL}&page=2"))));
        let last = Headers::from([
            ("x-next-page", ""),
            ("link", r#"<https://h/api/v4/other?page=9>; rel="next""#),
        ]);
        assert_eq!(next_page(&last, URL), Ok(None));
        assert_eq!(
            next_page(&Headers::from([("x-next-page", " ")]), URL),
            Ok(None)
        );
        for bad in ["two", "0", "-1"] {
            assert_eq!(
                next_page(&Headers::from([("x-next-page", bad)]), URL),
                Err(bad.to_owned())
            );
        }
    }

    #[test]
    fn link_is_the_fallback() {
        let link = Headers::from([("link", r#"<https://h/api/v4/x?page=3>; rel="next""#)]);
        assert_eq!(
            next_page(&link, URL),
            Ok(Some("https://h/api/v4/x?page=3".to_owned()))
        );
        assert_eq!(next_page(&Headers::default(), URL), Ok(None));
    }

    #[test]
    fn pages_replace_the_page_parameter() {
        assert_eq!(with_page("https://h/x", 2), "https://h/x?page=2");
        assert_eq!(
            with_page("https://h/x?per_page=2&page=2", 3),
            "https://h/x?per_page=2&page=3"
        );
        assert_eq!(
            with_page("https://h/x?page=1&per_page=2", 2),
            "https://h/x?per_page=2&page=2"
        );
        // `per_page` is not `page`.
        assert_eq!(
            with_page("https://h/x?per_page=20", 2),
            "https://h/x?per_page=20&page=2"
        );
    }

    #[test]
    fn review_requests_queries() {
        assert_eq!(
            review_requests_query(None, 100),
            "/merge_requests?scope=reviews_for_me&state=opened&per_page=100"
        );
        let after = Timestamp::from_unix_secs(1_793_088_000).unwrap();
        assert_eq!(
            review_requests_query(Some(after), 100),
            "/merge_requests?scope=reviews_for_me&state=opened&updated_after=2026-10-27T08:00:00Z&per_page=100"
        );
    }

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(
            path_segment("6a9c1750b37d513a43987b574953fceb50b03ce7"),
            "6a9c1750b37d513a43987b574953fceb50b03ce7"
        );
        assert_eq!(path_segment("../x?y=1"), "%2E%2E%2Fx%3Fy%3D1");
        assert_eq!(path_segment("é"), "%C3%A9");
    }
}
