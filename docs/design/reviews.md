# Code review

> Merge requests to review (v0.4): discovery, the MR mirror, fetched refs and diffs, drafts, publish through the forge APIs, credentials. Decisions in [ADR-0016](../adr/0016-mr-review-mirror.md). The TUI view is in [User workflow](user-workflow.md#code-review).

"MR" means a GitLab merge request or a GitHub pull request. The forge facts below come from the v0.4 spike (2026-10-27); the requests and responses are recorded in [`fixtures/forge/`](../../fixtures/forge/README.md).

## Discovery

- **Who asks**: loomd, per context, per forge host that the context has a credential for (see [Credentials](#credentials)). One token can see several contexts' projects; an MR is kept only by the context its project matches.
- **Which MRs**: open MRs where I am a requested reviewer.
  - GitHub: `GET /search/issues?q=is:pr is:open archived:false review-requested:@me`. `review-requested` includes requests to my teams; `user-review-requested` is direct requests only. Default: `review-requested`, configurable per forge host (`review_requests = "direct" | "teams"`).
  - GitLab: `GET /merge_requests?scope=reviews_for_me&state=opened`, with `updated_after` set to the last successful poll minus a margin.
- **Context of an MR**: the `match.hosts` rules of [Repositories](repos.md#contexts), applied to `<host>/<project path>` of the MR's target project. An MR that matches no context goes to `default_context` when set, otherwise it is ignored and listed by `loom repo doctor`. A context never stores an MR that matches another context, even when its token sees it (client data stays in the client's store).
- **Poll**: every `[forges."<host>"] poll_interval` (default 5 min, jittered ±10 %), and on `loom review sync`. GitHub search is limited to 30 requests per minute; one search per host and context per poll stays far below.

## Review task

- One Review task per MR, created in `inbox` with an `origin` link: provider, key, URL. Keys: `acme/shop#412` (GitHub), `company-a/checkout!88` (GitLab). The key is an alias of the task (storage IDs).
- Done (domain model): a verdict posted for the current head SHA. A new head after my verdict reopens the task (`ready`), with an event.
- MR merged or closed: the task is `done` or `cancelled` with an event (a terminal remote state closes the local task). My review request removed: `cancelled`, unless I already posted a verdict (then `done`).
- An MR I am asked to review again (GitHub re-request, GitLab reviewer state reset) reopens the task.

## MR mirror

Daemon-owned, under the task, like `remote/` (one owner per file), except the drafts, which are mine and written through loomd.

```
tasks/<id>/mrs/<provider>-<key>/       # key with / and # or ! encoded, e.g. github-acme%2Fshop%23412
  mr.json            # snapshot: title, author, state, draft, labels, reviewers, base and head refs, head SHA, web URL, forge IDs
  description.md     # MR description, read-only
  versions.json      # every head SHA seen: SHA, base SHA, merge base, first seen, forge version ID (GitLab)
  threads.json       # snapshot of every thread and general comment, normalized, rewritten on each pull
  diffs/<sha>.patch  # diff of a version I opened or reviewed (merge base..head)
  review.json        # mine: draft comments, replies, resolves, verdict, viewed marks, last reviewed SHA, publish progress
```

- `threads.json` is a snapshot, not a log: a `jsonl` file under `merge=union` (multi-machine storage) would duplicate every rewritten line. The thread history is the forge's; what I did is in `events.jsonl` (`review` events).
- A thread: forge ID, file-level or line anchor (path, side, start and end line, SHA it was written on), outdated, resolved, comments (author, body, time, forge IDs). General comments are threads without anchor. GitLab system notes are dropped.
- The store is backed up (storage): patches and threads hold client code and are pushed to the context's backup remote. That remote must be allowed by the client, as for every file of the store.

## Git: refs and diffs

- **Where**: the indexed clone of the MR's target project: the repo whose remote URL names the same host and path. The remote used is that remote, not always the primary (a fork as `origin`, the project as `upstream`).
- **Fetch** (git CLI, ADR-0008, same non-interactive environment as automatic updates): `git fetch --no-tags --no-write-fetch-head <remote> +refs/pull/<n>/head:refs/loom/mr/<remote>/<n>/head +refs/heads/<base>:refs/loom/mr/<remote>/<n>/base` (GitLab: `refs/merge-requests/<iid>/head`). The forced update is safe: `refs/loom/` is loom's namespace. Branches, the working tree and the index are never touched, so a dirty tree does not block it.
- **Keep reviewed versions**: a force push removes the old head from the forge ref. `refs/loom/mr/<remote>/<n>/reviewed` points at the last reviewed SHA, so git keeps its objects for "since last review". The patch in `diffs/` keeps the diff even when the clone is gone.
- **Diff of a version**: `git diff -M <merge base> <head>`, merge base of the base ref and the head. GitLab gives it (`diff_refs.base_sha`); GitHub gives the base tip (`base.sha`), so loom computes it.
- **Since last review**: the files whose diff changed between the last reviewed version and the new one (per file, compare the two patches with their line numbers removed). A plain `git diff <old head> <new head>` is wrong after a rebase: it shows every change that came from the base branch (fixture: `README.md`). The view shows the current diff of those files and marks the hunks that are new.
- **Viewed marks**: per file, the blob ID of the file at the head I viewed. A new version keeps the mark when the blob is the same and drops it otherwise.
- **No clone**: the MR still syncs (task, threads, description). The view uses the forge's per-file patches (GitHub `pulls/<n>/files`, GitLab `merge_requests/<iid>/diffs`): no context expansion, no worktree, and large files may be missing (GitHub cuts the patch of a big file, GitLab marks it `too_large`). The view says so and shows the remote URL to clone.
- **Cleanup**: when the task is archived, loomd deletes `refs/loom/mr/<remote>/<n>/*` (`git update-ref -d`).
- **Worktree** (`w`): `git worktree add --detach <path> refs/loom/mr/<remote>/<n>/head` under `~/.loom/worktrees/<task-id>`, removed when the task closes if it is clean; a dirty worktree is kept and reported.

## Drafts

- A draft comment: anchor (path, side, start and end line, head SHA it was written on), body with an optional Conventional Comments label and decorations, optional suggestion. A reply: thread forge ID and body. A resolve: thread forge ID. One verdict: `approve`, `request_changes` or `comment`, with an optional body.
- Lines are file line numbers (GitHub `line`/`side`, GitLab `old_line`/`new_line`), never diff positions (GitHub `position` is deprecated).
- A comment can be on an added, removed or unchanged line inside a hunk, or on a range of lines in one hunk and one side. Lines outside the hunks (expanded context) are not commentable in v0.4: both forges refuse them (fixtures: `graphql-error-line-not-in-diff.json`, `error-400-line-not-in-diff.json`).
- Suggestions: GitHub ` ```suggestion `, GitLab ` ```suggestion:-0+0 ` (lines above and below the anchor). Only on the RIGHT side (new lines).
- **Re-anchor after a new version** (pure): a draft on a file whose blob did not change keeps its lines. Otherwise, the anchored lines are looked up in the new diff of that file (same text, nearest line); found: moved; not found or no longer in a hunk: flagged, kept, not publishable until moved or deleted.

## Publish

`P` publishes all drafts of an MR as one review, then the verdict. Publishing is team-visible: the autonomy policy applies to agents; for me it is a confirmation modal with the summary.

Before any write, loomd checks, then refuses with the reason:

1. **Fresh head**: it pulls the MR. If the head moved since the drafts were anchored, it re-anchors and asks again. An `approve` is only for the current head.
2. **Anchors**: every anchor is inside a hunk of the forge's own patch of that file (the local diff and the forge's diff can differ: rename detection, diff algorithm). A mismatch flags the draft.
3. **No foreign pending review**: GitHub allows one pending review per user and MR; GitLab `bulk_publish` publishes every draft note of the user, those made in the web UI included. If one exists that loom did not create, loom stops and names it (fixtures: `graphql-pending-reviews.json`, `draft-notes.foreign.json`).
4. **Not my own MR** for `approve` and `request_changes` (GitHub refuses with 422, GitLab may allow self-approval by project setting).

Then, per forge:

| Step | GitHub (GraphQL) | GitLab (REST) |
| --- | --- | --- |
| 1. Pending review with new comments | `addPullRequestReview(pullRequestId, commitOID, threads)`, no `event` | one `POST draft_notes` per comment, `position` from `diff_refs` |
| 2. Replies | `addPullRequestReviewThreadReply(pullRequestReviewId, threadId, body)` | `POST draft_notes` with `in_reply_to_discussion_id` |
| 3. Publish | `submitPullRequestReview(event, body)` | `POST draft_notes/bulk_publish` with `note` and `reviewer_state` (`requested_changes`, `reviewed`) |
| 4. Approve | in step 3 (`event: APPROVE`) | `POST approve` with `sha` (409 if the head moved) |
| 5. Resolve | `resolveReviewThread` per thread | in step 2 (`resolve_discussion`), or `PUT discussions/<id>?resolved=true` |

- Nothing is visible to others before step 3 on either forge: pending reviews and draft notes are visible only to their author.
- **Not atomic, but resumable**: `review.json` records each step and each forge ID as it is created. A failure stops the publish; the next `P` resumes from the first step not done, and finds the pending review or draft notes it created by their IDs. Steps 4 and 5 after a published step 3 are retried alone.
- GitLab "request changes" is a reviewer state, not an approval rule: `reviewer_state = requested_changes`. GitLab has no single call for comments and verdict, GitHub has (REST `POST pulls/<n>/reviews`), but the REST call cannot carry replies to existing threads, hence GraphQL on GitHub.
- The publish is one `review` event: MR, head SHA, verdict, counts, forge review URL.

## Polling and limits

- GitHub REST: 5,000 requests per hour per token. `GET pulls/<n>` with `If-None-Match`: a 304 does not count. Threads (GraphQL, point-based limit) are pulled only when `updated_at` of the MR changed.
- GitLab: `updated_after` on the list, then MR, versions and discussions of changed MRs only. gitlab.com allows 2,000 requests per minute per user; self-hosted limits vary.
- Pagination: GitHub `link` header, GitLab `x-next-page` (or `link`), `per_page=100`.
- 429, or 403 with `x-ratelimit-remaining: 0`: wait until `retry-after` or `x-ratelimit-reset`; other hosts continue. Other failures back off per host as automatic updates do.
- Network failures never touch the mirror: the last snapshot stays, with the time of the last successful pull.

## Credentials

- Resolved by loomd at runtime, never stored by loom, never logged, never passed to git or to agents (ADR-0004). git uses the user's own credentials for fetch (ADR-0008).

```toml
[forges."github.com"]
kind = "github"
token_cmd = "gh auth token --hostname github.com"

[forges."gitlab.company-a.example"]
kind = "gitlab"
token_env = "GITLAB_TOKEN"                     # or token_cmd

[contexts.company-a.forges."github.com"]
token_cmd = "op read op://company-a/github/token"   # another account on the same host
```

- Lookup for a context and a host: `[contexts.<ctx>.forges."<host>"]`, then `[forges."<host>"]`. No credential: the context does not poll that host.
- `token_cmd` runs through `sh -c` with a 10 s timeout; stdout, trimmed, is the token. `token_env` reads loomd's environment (a launchd agent does not see shell variables: `token_cmd` is the usual choice). The token is kept in memory and resolved again after a 401.
- Scopes: GitHub fine-grained token with Pull requests read and write and Contents read, or a classic token / `gh` token with `repo`. A fine-grained token has one resource owner: a context that reviews in two organizations needs a classic token or two contexts. GitLab: `read_api` to read, `api` to publish. `GET /personal_access_tokens/self` gives the scopes; GitHub classic tokens return `x-oauth-scopes`.
- `loom repo doctor` gets a `forge <host>` check per context and host: token resolved, `GET /user` succeeds, scopes enough to publish (warn when read-only).

## Repo forge metadata

The repo card gets, from the forge, for repos of a host with a credential: project ID, default branch on the forge, visibility, archived, web URL, open MRs count. Refreshed with the MR poll, shown by `loom repo show`, never used for updates (the git state stays the source for branches).

## `loom pr`

`loom pr [<task>]` opens a draft MR for the current branch, linked to the task (default: the focus of the current context): `git push -u <remote> <branch>` (git CLI, user's credentials), then the forge API (`POST /repos/<o>/<r>/pulls` with `draft: true`, GitLab `POST /projects/<id>/merge_requests` with `Draft:` title). Title and body from the task (title, kind template, links). The MR becomes a `mr` link of the task (role `delivery`) and a `mr` event. Refuses on the default branch and on a branch with no commit ahead of the base. When an MR already exists for the branch, it links it instead.

## Out of scope for v0.4

Agent findings inline (v0.7), comments outside hunks, file-level comments, image diffs, editing or deleting published comments, multi-machine (one poller per context, later), a loom-owned clone when no local clone exists.
