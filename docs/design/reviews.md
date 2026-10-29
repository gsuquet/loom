# Code review

> Merge requests to review (v0.4): discovery, the MR mirror, fetched refs and diffs, drafts, publish through the forge APIs, credentials. Decisions in [ADR-0016](../adr/0016-mr-review-mirror.md). The TUI view is in [User workflow](user-workflow.md#code-review).

"MR" means a GitLab merge request or a GitHub pull request. The forge facts below come from the v0.4 spike (2026-10-27); the requests and responses are recorded in [`fixtures/forge/`](../../fixtures/forge/README.md).

## Discovery

- **Who asks**: loomd, per context, per forge host that the context has a credential for (see [Credentials](#credentials)). One token can see several contexts' projects; an MR is kept only by the context its project matches.
- **Which MRs**: open MRs where I am a requested reviewer.
  - GitHub: `GET /search/issues?q=is:pr is:open archived:false review-requested:@me`. `review-requested` includes requests to my teams; `user-review-requested` is direct requests only. Default: `review-requested`, configurable per forge host (`review_requests = "direct" | "teams"`).
  - GitLab: `GET /merge_requests?scope=reviews_for_me&state=opened`, with `updated_after` set to the last successful poll minus a margin.
- **Context of an MR**, first match wins: the context of an indexed clone of the target project (so `match.paths` and repo overrides count); the `match.hosts` rules of [Repositories](repos.md#contexts) on `<host>/<project path>`; otherwise the context whose credential found it. Never `default_context`: a client MR found by a client token must not land in the personal store. A context never stores an MR that belongs to another context, even when its token sees it.
- **Moving a Review task** to another context (ADR-0015) is refused: its context comes from the MR, and the next poll would put it back.
- **Poll**: every `[forges."<host>"] poll_interval` (default 5 min, jittered ±10 %), and on `loom review sync`. GitHub search is limited to 30 requests per minute; one search per host and context per poll stays far below.
- **Open Review tasks are polled too**, one by one with their ETag, not only the search results: GitHub removes me from `requested_reviewers` once I submit a review, and GitLab `updated_after` does not show a removed request. Without this, a new head after my verdict would never reopen the task.

## Review task

- One Review task per MR, created in `inbox` with an `origin` link: provider, key, URL. Keys: `acme/shop#412` (GitHub), `company-a/checkout!88` (GitLab). The key is an alias of the task (storage IDs): `/v1/tasks/{id}` accepts an exact MR key (percent-encoded by the CLI) and resolves it to the Review task whose `origin` link names it; two tasks with that origin (two contexts) is an `invalid_request` that asks for the ID.
- **Fields the sync owns**: title, kind and origin link at creation only; afterwards the status, only on the transitions below, each preceded by a daemon note giving the reason. Importance, due, pinned, notes and later title edits are mine and never touched. Archived Review tasks are not changed.
- **Transitions are edges between two polls**, not levels: "request removed" is requested at the last poll and not now, "requested again" the reverse (kept in `sync.json`). Closing a task by hand while still requested therefore sticks until a new head or a new request.
- Done (domain model): a verdict posted for the current head SHA. A new head after my verdict reopens the task (`ready`), with an event.
- MR merged or closed: the task is `done` or `cancelled` with an event (a terminal remote state closes the local task). My review request removed: `cancelled`, unless I already posted a verdict (then `done`).
- An MR I am asked to review again (GitHub re-request, GitLab request added again) reopens the task. A GitLab reviewer state reset without a new request is not seen in v0.4 (the reviewer states are not read).
- Verdict: GitHub, my latest submitted review that approves, requests changes or comments, on a commit (a new review ID is a new verdict); GitLab, my approval (`approvals.user_has_approved`), dated to the head of the poll that first shows it: an approval still shown after a push is not a new verdict (projects that do not reset approvals), one shown again after a reset is. A comment without approval is not a verdict. The last verdict is kept in `sync.json`, so an approval reset by a push is not forgotten.
- A request removed after a verdict on an older head (I reviewed, the author pushed before the poll) leaves the task open: the new head is still to review. A task created for an MR whose current head already has my verdict (GitLab lists approved MRs) is closed at once.
- An incomplete GitHub search does not count as a removed request: an MR missing from it keeps the request state of the last poll. A `done` or `cancelled` task of a merged or closed MR is not polled any more.

## MR mirror

Daemon-owned, under the task, like `remote/` (one owner per file), except the drafts, which are mine and written through loomd.

```
tasks/<id>/mrs/<provider>-<key>/       # key with every byte but [A-Za-z0-9_-] as %XX, e.g. github-acme%2Fshop%23412
  mr.json            # snapshot: title, author, state, draft, labels, reviewers, base and head refs, head SHA, web URL, forge IDs
  description.md     # MR description, read-only
  versions.json      # every head SHA seen: SHA, base SHA, merge base, first seen, forge version ID (GitLab)
  threads.json       # snapshot of every thread and general comment, normalized, rewritten on each pull
  sync.json          # sync state: requested at the last poll, last verdict seen (SHA, review ID), verdict shown at the last read
  diffs/<sha>.patch  # diff of a version I opened or reviewed (merge base..head)
  review.json        # mine: draft comments, replies, resolves, verdict, viewed marks, last reviewed SHA, publish progress
```

- Writes: a file is written (atomic rename) only when its bytes change, and a task whose status or mirror changed gets one store commit per poll, with the message of its events, else `mr(<id>): <key> mirror updated`. A poll that sees no change writes and commits nothing. ETags and the last threads pull time live in memory: after a restart, each MR is read in full once, which rewrites nothing if it did not change.
- `threads.json` is a snapshot, not a log: a `jsonl` file under `merge=union` (multi-machine storage) would duplicate every rewritten line. The thread history is the forge's; what I did is in `events.jsonl` (`review` events).
- A thread: forge ID, file-level or line anchor (path, side, start and end line, head SHA), outdated, resolved, comments (author, body, time, forge IDs). General comments are threads without anchor. GitLab system notes are dropped.
  - The anchor is the thread's position in the newest version it applies to: the current version when the thread is not outdated (GitHub `line`, GitLab moves `position` to the new version when it can), otherwise the version it was written on (GitHub `originalLine` and `originalCommit`, GitLab `position`). Both forges then give the same anchor for the same thread.
  - The path is the path in the head on both sides. A range that starts on the other side, or after its end, is kept on its last line, where both forges show the thread.
  - GitLab has no `outdated` flag: a discussion is outdated when its `position.head_sha` is not the current head. Until GitLab has moved the positions after a push (a background job), every line discussion reads as outdated; a discussion on one commit (commit view) always does. GitHub `isOutdated` (the hunk changed) and GitLab tracing (the line changed) can disagree on edge cases.
  - MR keys ignore the ASCII case of the project, as both forges do; the mirror directory name is lowercase and at most 200 bytes.
- The store is backed up (storage): patches and threads hold client code and are pushed to the context's backup remote. That remote must be allowed by the client, as for every file of the store.

## Git: refs and diffs

- **Where**: the indexed clone of the MR's target project: the repo whose remote URL names the same host and path. The remote used is that remote, not always the primary (a fork as `origin`, the project as `upstream`). Several clones or worktrees of the project: the main worktree of the most recently used clone (search ranking), then the shortest path. The choice is kept in `mr.json` and changes only when that clone is gone.
- **Fetch** (git CLI, ADR-0008, same non-interactive environment as automatic updates): `git fetch --no-tags --no-write-fetch-head <remote> +refs/pull/<n>/head:refs/loom/mr/<remote>/<n>/head +refs/heads/<base>:refs/loom/mr/<remote>/<n>/base` (GitLab: `refs/merge-requests/<iid>/head`). The forced update is safe: `refs/loom/` is loom's namespace. Branches, the working tree and the index are never touched, so a dirty tree does not block it.
- **Keep reviewed versions**: a force push removes the old head from the forge ref. `refs/loom/mr/<remote>/<n>/reviewed` points at the last reviewed SHA, so git keeps its objects for "since last review". The patch in `diffs/` keeps the diff even when the clone is gone.
- **Diff of a version**: `git diff -M <merge base> <head>`. GitLab gives the merge base (`diff_refs.base_sha`). GitHub gives the base tip the forge used (`base.sha`): loom computes `git merge-base <base.sha> <head>`, not with the fetched base ref, which may have moved since and would shift the hunks from the forge's. The fetched base ref only brings the objects.
- **Since last review**: the files whose diff changed between the last reviewed version and the new one. Per file, loom compares a hash of the normalized patch: no `index` line, no `@@` line numbers or section text, only the `+`, `-` and context lines. A plain `git diff <old head> <new head>` is wrong after a rebase: it shows every change that came from the base branch (fixture: `README.md`). The view shows the current diff of those files and marks the hunks that are new.
- **Viewed marks**: per file, the normalized patch hash above. A new version keeps the mark when the hash is the same and drops it otherwise, so viewed marks and "since last review" always agree (a rebase that only moves lines keeps both).
- **No clone**: the MR still syncs (task, threads, description). The view uses the forge's per-file patches (GitHub `pulls/<n>/files`, GitLab `merge_requests/<iid>/diffs`): no context expansion, no worktree, and large files may be missing (GitHub cuts the patch of a big file, GitLab marks it `too_large`). The view says so and shows the remote URL to clone.
- **Cleanup**: when the task is archived, loomd deletes `refs/loom/mr/<remote>/<n>/*` (`git update-ref -d`).
- **Worktree** (`w`): `git worktree add --detach <path> refs/loom/mr/<remote>/<n>/head` under `~/.loom/worktrees/<task-id>`, removed when the task closes if it is clean; a dirty worktree is kept and reported.

## Drafts

- A draft comment: anchor (path, side, start and end line, head SHA it was written on), body with an optional Conventional Comments label and decorations, optional suggestion. A reply: thread forge ID and body. A resolve: thread forge ID. One verdict: `approve`, `request_changes` or `comment`, with an optional body.
- Lines are file line numbers (GitHub `line`/`side`, GitLab `old_line`/`new_line`), never diff positions (GitHub `position` is deprecated).
- A comment can be on an added, removed or unchanged line inside a hunk, or on a range of lines in one hunk and one side. Lines outside the hunks (expanded context) are not commentable in v0.4: both forges refuse them (fixtures: `graphql-error-line-not-in-diff.json`, `error-400-line-not-in-diff.json`).
- Suggestions: GitHub ` ```suggestion `, GitLab ` ```suggestion:-0+0 ` (lines above and below the anchor). Only on lines of the new file (added or unchanged lines, GitHub side `RIGHT`), never on removed lines.
- **Re-anchor after a new version** (pure): a draft on a file whose new blob did not change keeps its lines. Otherwise, the anchored lines are looked up in the new diff of that file (same text, nearest line); found: moved; not found or no longer in a hunk: flagged, kept, not publishable until moved or deleted.

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
| 4. Approve | in step 3 (`event: APPROVE`) | `POST approve` with `sha` (409 if the head moved); step 3 sends no `reviewer_state` then. A project that asks for `approval_password` is a reported failure: loom does not handle passwords |
| 5. Resolve | `resolveReviewThread(threadId)` per thread (replies use `pullRequestReviewThreadId`) | in step 2 (`resolve_discussion`), or `PUT discussions/<id>?resolved=true` |

- Nothing is visible to others before step 3 on either forge: pending reviews and draft notes are visible only to their author.
- **Not atomic, but resumable through an intent**: before each forge call, `review.json` records the intent (step, start time, hash of each body and anchor); after it, the forge IDs. loomd can stop between a forge write and the record of its ID, so a resume never trusts the record alone. It reads the forge first, then reconciles:
  - GitHub: my pending review created after the intent time with the same thread bodies is adopted; a submitted review on the head with the intent's body means step 3 is done.
  - GitLab: my draft notes are matched to drafts by body and anchor hash and adopted, so a retried step 1 or 2 creates no duplicate; a published note with the summary body after the intent time means step 3 is done (the summary is not posted twice); `GET approvals` (`user_has_approved`) before step 4.
  - Pre-check 3 (foreign pending review) runs after this reconciliation, on what is left: only drafts that match no intent are foreign.
  - Steps 4 and 5 after a done step 3 are retried alone. A step whose outcome cannot be read back is reported, never sent twice.
- GitLab "request changes" is a reviewer state, not an approval rule: `reviewer_state = requested_changes`. GitLab has no single call for comments and verdict, GitHub has (REST `POST pulls/<n>/reviews`), but the REST call cannot carry replies to existing threads, hence GraphQL on GitHub.
- The publish is one `review` event: MR, head SHA, verdict, counts, forge review URL.

## Polling and limits

- GitHub REST: 5,000 requests per hour per token. `GET pulls/<n>` with `If-None-Match`: a 304 does not count. Threads (GraphQL, point-based limit) are pulled when `updated_at` of the MR changed, and in full every hour (it is not known that every thread resolution changes `updated_at`; the first sync task checks it).
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
- loomd as a service has a minimal `PATH`: `gh` or `op` from Homebrew are not found by name. The doctor reports "command not found" with the absolute path to use. A `token_cmd` that waits for an interactive unlock (1Password) times out; the doctor says so.
- The token is sent only to the API host of its table: redirects to another host are not followed with it.
- API base: `https://api.github.com` (GraphQL `https://api.github.com/graphql`); GitHub Enterprise Server `https://<host>/api/v3` and `https://<host>/api/graphql`; GitLab `https://<host>/api/v4`.
- GitHub SAML SSO: a 403 with `x-github-sso` is reported with the authorization URL it gives, not as a bad token.
- Scopes: GitHub fine-grained token with Pull requests read and write and Contents read, or a classic token / `gh` token with `repo`. A fine-grained token has one resource owner: a context that reviews in two organizations needs a classic token or two contexts. GitLab: `read_api` to read, `api` to publish. `GET /personal_access_tokens/self` gives the scopes of a personal access token; it fails for the OAuth token of `glab`, and then the scopes are reported unknown (warn). GitHub classic tokens return `x-oauth-scopes`; fine-grained ones do not, and are reported unknown as well.
- `loom repo doctor` gets a `forge <host>` check per context and host: token resolved, `GET /user` succeeds, scopes enough to publish (warn when read-only).

## Repo forge metadata

The repo card gets, from the forge, for repos of a host with a credential: project ID, default branch on the forge, visibility, archived, web URL, open MRs count. Refreshed with the MR poll, shown by `loom repo show`, never used for updates (the git state stays the source for branches).

## `loom pr`

`loom pr [<task>]` opens a draft MR for the current branch, linked to the task (default: the focus of the current context): `git push -u <remote> <branch>` (git CLI, user's credentials), then the forge API (`POST /repos/<o>/<r>/pulls` with `draft: true`, GitLab `POST /projects/<id>/merge_requests` with `Draft:` title). Title and body from the task (title, kind template, links). The MR becomes a `mr` link of the task (role `delivery`) and a `mr` event. Refuses on the default branch and on a branch with no commit ahead of the base. When an MR already exists for the branch, it links it instead.

## Out of scope for v0.4

Agent findings inline (v0.7), comments outside hunks, file-level comments, image diffs, editing or deleting published comments, multi-machine (one poller per context, later), a loom-owned clone when no local clone exists.
