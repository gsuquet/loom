# Forge fixtures

Composed requests and responses of the GitHub and GitLab APIs that the v0.4 review work reads and sends, for tests that run against a local fake server. No test talks to a real forge.

Design: [Code review](../../docs/design/reviews.md), [ADR-0016](../../docs/adr/0016-mr-review-mirror.md).

## Sources

The JSON files are composed, not captured from a real account, so they hold no real names, tokens or code. Their shapes come from, checked on 2026-10-27:

- GitHub REST: the [OpenAPI description](https://github.com/github/rest-api-description) (`api.github.com.json`, version 1.1.4) and live responses of `GET /repos/{owner}/{repo}/pulls/{n}` and `.../files` on this repository.
- GitHub GraphQL: the public schema ([octokit/graphql-schema](https://github.com/octokit/graphql-schema)). No GraphQL call could be made from the environment that wrote them, so the GraphQL error messages are taken from the GitHub docs and forum reports, not from a response.
- GitLab REST: the API docs in `gitlab-org/gitlab` (`doc/api/merge_requests.md`, `discussions.md`, `draft_notes.md`, `merge_request_approvals.md`) and a live, unauthenticated `GET /projects/:id/merge_requests/:iid` on gitlab.com (`discussions` and `versions` need a token there).

Each file keeps the fields loom reads and some it does not: a parser ignores unknown fields. Error message texts are examples. Code matches on the HTTP status and, for GraphQL, the error `type` and the step that failed, never on the message. Both GraphQL errors here have the type `UNPROCESSABLE`: loom tells them apart by its pre-checks (a pending review is looked for before `addPullRequestReview`, anchors are checked against the forge's patch), so at that step either error means "state changed since the checks".

`github/error-422-own-pull.json` is the REST shape, for the REST fallback of ADR-0016. The GraphQL publish refuses an own pull request before any call (pre-check 4).

## Scenario

One merge request, the same on both forges, with a force push between two versions. `scenario.sh <dir>` builds its git side: a bare repository with `main`, `auth-refactor`, `refs/pull/412/head` and `refs/merge-requests/88/head`. Commits have fixed authors and dates, so the SHAs below are stable and match the JSON.

| Name | SHA | What |
| --- | --- | --- |
| M1 | `506ce3f338acb7125973db71a5c43fd062c4f046` | `main`, base of version 1 |
| V1 | `7ebf283bfee25ab48f95c0c55181a4ca66b4c990` | head of version 1 (kept as `refs/loom-fixture/v1`; no forge ref points at it after the force push) |
| M2 | `ee047f5164ae63069c7237b6f24c66d34f9eff26` | `main` after a README change, base of version 2 |
| V2 | `84280c47b6aa128c645b1fbea54aded8b32e8463` | head of version 2: version 1 rebased on M2, plus a leeway in `src/auth/jwt.rs` |

- Changed files: `Cargo.lock` (generated), `src/api/login.rs` (same blob `7891dec` in both versions), `src/auth/jwt.rs` (changed between versions).
- Since the last review of V1, only `src/auth/jwt.rs` changed. `README.md` changed between V1 and V2 too, but it is not in the diff of either version: a plain `git diff V1 V2` shows it, which is why "since last review" compares the per-file diffs.
- Users: `alice` (author), `bob` (another reviewer), `carol` (the loom user).
- Threads: bob's question on `src/auth/jwt.rs` line 10 of V1, with alice's reply, unresolved and outdated in V2; bob's nitpick on `src/api/login.rs` line 6, resolved and still current; one general comment; on GitLab, one system note.
- Drafts that carol publishes: a suggestion on `src/auth/jwt.rs` line 14 of V2, a comment on lines 9 to 11, a reply to bob's question that resolves it, then the verdict.

## GitHub (`github/`)

Host `github.com`, API `https://api.github.com`, repository `acme/shop`, pull request #412 (node `PR_kwDOAAATic4AA9-c`).

| Request | Status | Response headers that matter | File |
| --- | --- | --- | --- |
| `GET /user` | 200 | `x-oauth-scopes` (classic tokens only), `x-ratelimit-*` | `user.json` |
| `GET /search/issues?q=is:pr+is:open+archived:false+review-requested:@me&per_page=100` | 200 | `link` (`rel="next"`) | `search-review-requested.json`, `.empty.json` |
| `GET /repos/acme/shop/pulls/412` | 200, 304 with `If-None-Match` | `etag` | `pull-412.v1.json`, `pull-412.v2.json` |
| `GET /repos/acme/shop/pulls/412/files?per_page=100` | 200 | `link` | `pull-412-files.v2.json` |
| `GET /repos/acme/shop/pulls/412/reviews?per_page=100` | 200 | `link` | `pull-412-reviews.json` |
| `GET /repos/acme/shop/issues/412/comments?per_page=100` | 200 | `link` | `issue-412-comments.json` |
| `POST /graphql`, `reviewThreads` of the pull request | 200 | | `graphql-review-threads.json` |
| `POST /graphql`, the next comments of one thread (`node(id:)`, for a thread with more than one page) | 200 | | composed in `crates/loom-forge/tests/github_graphql.rs` |
| `POST /graphql`, pending reviews of the viewer | 200 | | `graphql-pending-reviews.json`, `.empty.json` |
| `POST /graphql`, `addPullRequestReview` (no `event`: pending) | 200 | | `graphql-add-review.request.json`, `.response.json` |
| `POST /graphql`, `addPullRequestReviewThreadReply` | 200 | | `graphql-add-reply.request.json`, `.response.json` |
| `POST /graphql`, `submitPullRequestReview` | 200 | | `graphql-submit-review.request.json`, `.response.json` |
| `POST /graphql`, `resolveReviewThread` | 200 | | `graphql-resolve-thread.request.json`, `.response.json` |
| `POST /graphql`, a second pending review | 200 with `errors` | | `graphql-error-one-pending-review.json` |
| `POST /graphql`, a thread on a line outside the diff | 200 with `errors` | | `graphql-error-line-not-in-diff.json` |
| any, bad token | 401 | | `error-401.json` |
| any, rate limit | 403 or 429 | `x-ratelimit-remaining: 0`, `x-ratelimit-reset`, `retry-after` | `error-403-rate-limit.json` |
| `POST /repos/acme/shop/pulls/412/reviews`, approve own pull request | 422 | | `error-422-own-pull.json` |

## GitLab (`gitlab/`)

Host `gitlab.example.com`, API `https://gitlab.example.com/api/v4`, project `company-a/checkout` (id 77, path encoded `company-a%2Fcheckout`), merge request !88.

| Request | Status | Response headers that matter | File |
| --- | --- | --- | --- |
| `GET /user` | 200 | | `user.json` |
| `GET /personal_access_tokens/self` | 200 | | `personal-access-token-self.json` (`api`), `.read-api.json` (`read_api`) |
| `GET /merge_requests?scope=reviews_for_me&state=opened&updated_after=…&per_page=100` (no `updated_after` on the first poll) | 200 | `x-next-page`, `link` | `merge-requests-reviews-for-me.json`, `.empty.json` |
| `GET /projects/77/merge_requests/88` | 200 | `etag` | `merge-request-88.v1.json`, `merge-request-88.v2.json` |
| `GET /projects/77/merge_requests/88/versions?per_page=100` | 200 | | `merge-request-88-versions.json` (newest first) |
| `GET /projects/77/merge_requests/88/diffs?per_page=100` | 200 | `x-next-page` | `merge-request-88-diffs.json` |
| `GET /projects/77/merge_requests/88/discussions?per_page=2` | 200 | `x-next-page: 2`, `x-total-pages: 2` | `merge-request-88-discussions.page-1.json` |
| `GET /projects/77/merge_requests/88/discussions?per_page=2&page=2` | 200 | `x-next-page:` (empty) | `merge-request-88-discussions.page-2.json` |
| `GET /projects/77/merge_requests/88/draft_notes?per_page=100` | 200 | | `draft-notes.empty.json`, `draft-notes.foreign.json` (a draft started in the web UI) |
| `POST /projects/77/merge_requests/88/draft_notes`, added line | 201 | | `draft-note-create.new-line.request.json`, `.response.json` |
| same, line range (`line_range` with line codes) | 201 | | `draft-note-create.range.request.json` |
| same, unchanged line (both `old_line` and `new_line`) | 201 | | `draft-note-create.context-line.request.json` |
| same, reply that resolves a discussion | 201 | | `draft-note-create.reply.request.json` |
| same, a line outside the diff | 400 | | `error-400-line-not-in-diff.json` |
| `POST /projects/77/merge_requests/88/draft_notes/bulk_publish` | 204 | | `draft-notes-bulk-publish.request.json` |
| `POST /projects/77/merge_requests/88/approve` | 201 | | `approve.request.json`, `approve.response.json` |
| same, after a new push | 409 | | `error-409-sha-mismatch.json` |
| `GET /projects/77/merge_requests/88/approvals` | 200 | | `approve.response.json` (same shape) |
| `PUT /projects/77/merge_requests/88/discussions/6a9c1750b37d513a43987b574953fceb50b03ce7?resolved=true` | 200 | | (the discussion; not read) |
| any, bad token | 401 | | `error-401.json` |
| a write with a `read_api` token | 403 | | `error-403-insufficient-scope.json` |

Lists are read with `per_page=100`; the discussions use `per_page=2` here to show two pages. The first page has no `page` parameter, the next ones add `page=<x-next-page>`.

A line code is `<sha1 of the path>_<old line>_<new line>`; for `src/auth/jwt.rs` the hash is `d6731b5918e52205f2871b9c60cea4aed6807584`.

The GitLab API has no `outdated` flag on a discussion. When a new version keeps the lines of a discussion in the diff, GitLab moves its `position` to that version (bob's nitpick: version 2). An outdated discussion keeps the `position` of the version it was written on (bob's question: version 1).
