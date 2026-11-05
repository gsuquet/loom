# Code review

> Merge requests to review (v0.4): discovery, the MR mirror, fetched refs and diffs, drafts, publish through the forge APIs, credentials. Decisions in [ADR-0016](../adr/0016-mr-review-mirror.md). The page is in [User workflow](user-workflow.md#review).

> The data, git and forge sides below stand, except that a Review task now starts when I start on the MR, not at discovery: see [Waiting on me](#waiting-on-me), which replaces that part of ADR-0016 decision 1. The flow and keys of the page are in [User workflow](user-workflow.md#review), which overrides any key or gesture named here. Multi-repo MRs show as a group, and a task can hold one worktree per repo.

"MR" means a GitLab merge request or a GitHub pull request. The forge facts below come from the v0.4 spike (2026-10-27); the requests and responses are recorded in [`fixtures/forge/`](../../fixtures/forge/README.md).

## Discovery

- **Who asks**: loomd, per context, per forge host that the context has a credential for (see [Credentials](#credentials)). One token can see several contexts' projects; an MR is kept only by the context its project matches.
- **Which MRs**: open MRs where I am a requested reviewer. The other reasons an MR waits on me are in [Waiting on me](#waiting-on-me).
  - GitHub: `GET /search/issues?q=is:pr is:open archived:false review-requested:@me`. `review-requested` includes requests to my teams; `user-review-requested` is direct requests only. Default: `review-requested`, configurable per forge host (`review_requests = "direct" | "teams"`).
  - GitLab: `GET /merge_requests?scope=reviews_for_me&state=opened`, with `updated_after` set to the last successful poll minus a margin.
- **Context of an MR**, first match wins: the context of an indexed clone of the target project (so `match.paths` and repo overrides count); the `match.hosts` rules of [Repositories](repos.md#contexts) on `<host>/<project path>`; otherwise the context whose token found it, when each other context with a credential for the host (held here or not) is polled on this machine with a token of another account (the logins differ). One token, or two tokens of one account, find the same MRs, so then the finder tells nothing: the MR goes to no store and each poll reports it as failed, asking for a `match.hosts` rule. Never `default_context`: a client MR found by a client token must not land in the personal store. A context never stores an MR that belongs to another context, even when its token sees it.
- **Moving a Review task** to another context (ADR-0015) is refused: its context comes from the MR.
- **Poll**: every `[forges."<host>"] poll_interval` (default 5 min, jittered ±10 %), and on `loom review sync`. GitHub search is limited to 30 requests per minute; one search per host and context per poll stays far below.
- **Open Review tasks are polled too**, one by one with their ETag, not only the search results: GitHub removes me from `requested_reviewers` once I submit a review, and GitLab `updated_after` does not show a removed request. Without this, a new head after my verdict would never reopen the task.

## Waiting on me

An MR that waits on me is a queue entry, not a task. It shows on Reviews and in the "Waiting on you" section of Today, and creates no task and no mirror until I start on it. This keeps requested reviews out of the Inbox and spares a task for an MR I only open at the forge.

- **Reasons**: an entry holds one or more reasons, and leaves the queue when none is left, or when the MR is merged or closed.

| Reason | GitLab | Gone when |
| --- | --- | --- |
| `review_requested` | `GET /merge_requests?scope=reviews_for_me&state=opened` | Request removed, or my verdict is on the current head and no request is newer than it (see [Existing tasks](#waiting-on-me)) |
| `assigned` | `scope=assigned_to_me`, when I am not the author | Unassigned |
| `mentioned` | `GET /todos?type=MergeRequest&state=pending&action=mentioned`, and `directly_addressed` | The to-do is done at the forge |
| `own_new_comments` | `scope=created_by_me`: a note from someone else, newer than my latest note and than `seen_at`. Discussions are read only when the MR changed | I open the entry, or I reply |
| `own_pipeline_failed` | `head_pipeline.status = failed`, on the single MR request (the list does not return it) | A new pipeline on the head is not failed |
| `own_ready_to_merge` | `detailed_merge_status = mergeable` and at least one approval | Not mergeable any more |

- **Assigned**: my own MRs are often assigned to me, so that case counts only through the `own_` reasons.
- **Ready to merge**: a project with no approvals on an MR never shows this reason, otherwise each green MR would ask for me.
- **Polling**: the three lists and the to-dos on each poll, four requests. Each entry is also read one by one with its ETag, since a list does not show an MR whose request was removed, or that is merged or closed; only its verdict is read when it changed, not its threads or versions. Open MRs of mine are also read one by one on each poll with their ETag: it is not known that a pipeline result changes `updated_at`, so `updated_after` does not cover them.
- **GitHub**: `review_requested` only until the GitHub connector is delivered (step 5), through the existing search. Its other reasons are designed then.
- **Files**: `contexts/<ctx>/queue/mrs/<provider>-<key>.json`, daemon-owned, deleted when the entry leaves. It holds the key, forge IDs (a GitLab entry that the list does not show is read by its project ID), title, author, web URL, head SHA, forge `updated_at`, reasons and `seen_at`. It is one file per MR and is written only when its bytes change, like the mirror, so a poll with no change makes no commit, and a store sync and a context purge carry it.
- **Context**: the rules of [Discovery](#discovery). An entry for an MR of no store is reported as failed, as a task was. When a rule change or a new clone moves an MR to another context, the task thread writes its entry into that store and removes it here, because the poll of that context will not list an unchanged MR. It does so only when this machine polls that context and host with the same account, or the new poll would remove the entry at once. Otherwise the entry stays here and the poll reports it as failed.
- **Failures**: a failed list or read changes no entry, with one refinement: a `404` on an MR that has only an entry, from a host that answers, means the MR is gone, and the entry goes with no event, as for a closed MR. A `403` stays a failure. Connector status tells that the queue may be stale.
- **Start**: opening the review, sending a reviewer or `loom review start <key>` creates the Review task in `active` with its mirror, and removes the entry of a `review_requested`, `assigned` or `mentioned` MR. From then on the task follows the rules below. It runs on the poller thread with a full read of the MR: it cannot race a poll, and a second start returns the same task, in whatever context the task is. A forge that fails refuses the start and changes nothing. A merged or closed MR is refused and loses its entry, and so does an MR that the forge answers `404` for. A machine that does not poll the context refuses the start: its task would stay unknown to the poll until a store sync.
- **My own MRs**: no Review task, ever. The entry stays while a reason holds, and shows under the task that has the MR as a `delivery` link, read when the page is built. `Enter` opens that task, or the forge when there is none.
- **Existing tasks**: an MR with an open Review task gets no `review_requested` entry. A closed Review task keeps its reopening rules. A Review task in any context counts: the MR stays in the context of its task. An MR whose Review task is archived can return as an entry. My verdict on the current head means no entry, unless a request is known to be newer than it. GitHub removes me from `requested_reviewers` when I submit, so being listed directly is newer. GitLab: my reviewer state `unreviewed` (`GET .../reviewers`, read only for an approved MR without a task) is taken as newer; the docs do not say what a new request does to that state, and the real GitLab check of the next issue verifies it. A request through a team on GitHub does not show this: it gives no entry, a known limit.

## Review task

- One Review task per MR, created when I [start on it](#waiting-on-me), with an `origin` link: provider, key, URL. Keys: `acme/shop#412` (GitHub), `company-a/checkout!88` (GitLab). The key is an alias of the task (storage IDs): `/v1/tasks/{id}` accepts an exact MR key (percent-encoded by the CLI) and resolves it to the Review task whose `origin` link names it; two tasks with that origin (two contexts) is an `invalid_request` that asks for the ID.
- **Fields the sync owns**: title, kind and origin link at creation only; afterwards the status, only on the transitions below, each preceded by a daemon note giving the reason. Importance, due, pinned, notes and later title edits are mine and never touched. Archived Review tasks are not changed.
- **Transitions are edges between two polls**, not levels: "request removed" is requested at the last poll and not now, "requested again" the reverse (kept in `sync.json`). Closing a task by hand while still requested therefore sticks until a new head or a new request.
- Done (domain model): a verdict posted for the current head SHA. A new head after my verdict reopens the task (`ready`), with an event.
- MR merged or closed: the task is `done` or `cancelled` with an event (a terminal remote state closes the local task). My review request removed: `cancelled`, unless I already posted a verdict (then `done`).
- An MR I am asked to review again (GitHub re-request, GitLab request added again) reopens the task. A GitLab reviewer state reset without a new request is not seen in v0.4 (the reviewer states are not read).
- Verdict: GitHub, my latest submitted review that approves, requests changes or comments, on a commit (a new review ID is a new verdict); GitLab, my approval (`approvals.user_has_approved`), dated to the head of the poll that first shows it: an approval still shown after a push is not a new verdict (projects that do not reset approvals), one shown again after a reset is. An MR with a task keeps that head in `sync.json`. An MR without a task keeps it in the machine-local `index.db`, per MR, written by the task thread, with no store commit; such an MR is polled one by one until it leaves, and its row goes with it. A rebuilt `index.db` dates the approval to the head of the next poll. A comment without approval is not a verdict. The last verdict is kept in `sync.json`, so an approval reset by a push is not forgotten.
- A request removed after a verdict on an older head (I reviewed, the author pushed before the poll) leaves the task open: the new head is still to review. No entry is created for an MR whose current head already has my verdict, unless a request is newer (see [Existing tasks](#waiting-on-me)) (GitLab lists approved MRs): nothing to review. A done task reopens on a new head only when my verdict is not on it, so a head that a failed mirror write missed does not reopen it. A cancelled task reopens when the MR is open again after a close and I am requested. An archived Review task is never changed: its mirror is the previous state, and only a new head or a new request brings the MR back as an entry. Merged and closed are edges too: a task I reopen by hand on a closed MR stays open.
- An incomplete GitHub search does not count as a removed request: an MR missing from it keeps the request state of the last poll. A `done` or `cancelled` task of a merged or closed MR is not polled any more.

## MR mirror

Daemon-owned, under the task, like `remote/` (one owner per file), except the drafts, which are mine and written through loomd.

```
tasks/<id>/mrs/<provider>-<key>/       # key with every byte but [A-Za-z0-9_-] as %XX, e.g. github-acme%2Fshop%23412
  mr.json            # snapshot: title, author, state, draft, labels, reviewers, base and head refs, head SHA, web URL, forge IDs
  description.md     # MR description, read-only
  versions.json      # every head SHA seen: SHA, base SHA, merge base, first seen, forge version ID (GitLab), kind of its patch
  threads.json       # snapshot of every thread and general comment, normalized, rewritten on each pull
  sync.json          # sync state: requested at the last poll, last verdict seen (SHA, review ID), verdict shown at the last read
  diffs/<sha>.patch  # diff of a version (merge base..head); a version gets it when the sync sees its head, not when I open it
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

- **Where**: the indexed clone of the MR's target project: the repo whose remote URL names the same host and path. The remote used is that remote, not always the primary (a fork as `origin`, the project as `upstream`). Several clones or worktrees of the project: the main worktree of the most recently used clone (search ranking), then the shortest path. The choice is a path of this machine: it is kept in `index.db` (`mr_clones`), not in `mr.json` or anywhere in the store, and changes only when that clone is gone (or no longer has a remote of the project). A clone replaced that still exists loses its refs, and its row stays until they are deleted. `loom reindex` keeps the rows; a new `index.db` chooses again by the same rule.
- **Fetch** (git CLI, ADR-0008, same non-interactive environment as automatic updates): `git fetch --no-tags --no-write-fetch-head --no-prune --no-prune-tags --no-recurse-submodules --refmap= -- <remote> +refs/pull/<n>/head:refs/loom/mr/<remote>/<n>/head +refs/heads/<base>:refs/loom/mr/<remote>/<n>/base` (GitLab: `refs/merge-requests/<iid>/head`). The forced update is safe: `refs/loom/` is loom's namespace. Branches, the working tree and the index are never touched, so a dirty tree does not block it. `--refmap=` is required: without it, git also moves `refs/remotes/<remote>/<base>` (opportunistic update), a ref outside `refs/loom/`. The other flags hold against `fetch.prune`, `fetch.pruneTags`, `tagOpt` and `fetch.recurseSubmodules`, as for automatic updates.
- **When**: after each poll of a context and host, and after each [start](#waiting-on-me), its own thread (`mr_worker`) goes through the Review tasks of that context and host whose MR is open. An MR with only a queue entry has no task, so it gets no fetch until I start on it. A head without a recorded patch is fetched and diffed; the record of the patch ends the work, so a failure or a busy lock is retried. A poll with nothing new runs no fetch and no diff: one `git rev-parse` per open MR checks that the head ref is still there, and a clone that changed or lost its refs is fetched again. A git failure is logged: it never fails, delays or rolls back the mirror sync. Only tasks that exist are fetched. Merged and closed MRs get no new diff.
- **`update`**: the policy governs automatic updates only. `update = "off"` stops them, not MR fetches: these move no branch and run only for tasks that exist.
- **Fetch failures**: an SSH authentication or network failure (`classify_remote_failure`) stops the other MRs of that endpoint for this poll; an HTTP 401 or 403, which can be of one project, or a remote that is gone stops those of that clone. In all cases, and for any other failure such as a timeout, the MR falls back to the forge's patches and backs off in polls, doubling up to 8 times the interval, so it cannot hold back the MRs after it at each poll. A base branch the remote no longer has does not fail: the head is fetched alone, and the diff uses the base SHA of the version.
- **Limits of a patch**: 5 MiB per patch and 50 MiB per MR, since the store and its backup keep each patch in their history. git is stopped as soon as its output reaches the limit. Over them, the current head falls back to the forge's patches (or to the list of its files when those are over too), an older version keeps a note alone, and `versions.json` says `diff: reduced_too_large`. The patch starts with a `# loom:` line that says why.
- **Keep reviewed versions**: a force push removes the old head from the forge ref. `refs/loom/mr/<remote>/<n>/reviewed` points at the last reviewed SHA, so git keeps its objects for "since last review". Until `review.json` exists, the last reviewed SHA is my last verdict (`sync.json`); a commit the clone does not have is skipped until a fetch brings it. The patch in `diffs/` keeps the diff even when the clone is gone.
- **Diff of a version**: `git diff -M <merge base> <head>`, with options that hold against the user's diff configuration (prefixes, context, algorithm, external driver, color, submodules, `core.attributesFile`), so the bytes and the hashes are the same for every user. Attributes are read from the head commit (`--attr-source`, git 2.41); an older git falls back to those of the working tree, and `.git/info/attributes` still applies on every version. `git diff` runs with the settings of a remote command, as a partial clone fetches blobs through it; the check for a commit never fetches (`GIT_NO_LAZY_FETCH`, git 2.45; ignored by older versions). The patch is kept as bytes: a file need not be UTF-8. The diff of a head changes only on a retarget of an open MR: a new target branch (`base_ref`), or a new GitLab version of the same head with another base. `base.sha` alone says nothing: GitHub moves it with the tip of the target branch, with no push and no retarget, so the base recorded for a head stays. The merge base of the head can still change when the target branch absorbs commits of the head: until the next push, the local diff can show extra hunks against a three-dot diff of the current tip, and a draft on such a hunk gets flagged by publish check 2. GitLab gives the merge base (`diff_refs.base_sha`). GitHub gives the base tip the forge used (`base.sha`): loom computes `git merge-base <base.sha> <head>`, not with the fetched base ref, which may have moved since and would shift the hunks from the forge's. The fetched base ref only brings the objects.
- **Since last review**: the files whose diff changed between the last reviewed version and the new one. Per file, loom compares a hash of the normalized patch: no `index` line, no `@@` line numbers or section text, only the `+`, `-` and context lines, and the `\ No newline at end of file` marker, without which a change of the final newline from one side to the other would not count. A file without hunks is compared by what its header says: mode, rename source, and for a binary file the `index` line, which alone tells two versions apart. A file dropped from the diff counts as changed. A file the forge gives no patch for has no hash and always counts as changed. The mode of a new or deleted file with hunks does not count, since the forges do not give it. A plain `git diff <old head> <new head>` is wrong after a rebase: it shows every change that came from the base branch (fixture: `README.md`). The view shows the current diff of those files and marks the hunks that are new.
- **Viewed marks**: per file, the normalized patch hash above. A new version keeps the mark when the hash is the same and drops it otherwise, so viewed marks and "since last review" always agree (a rebase that only moves lines keeps both).
- **No clone**: a started MR still syncs (task, threads, description). The view uses the forge's per-file patches (GitHub `pulls/<n>/files`, GitLab `merge_requests/<iid>/diffs`): no context expansion, no worktree, and large files may be missing (GitHub cuts the patch of a big file, GitLab marks it `too_large`). The view says so and shows the remote URL to clone. loomd writes these patches into `diffs/<sha>.patch` and marks the version `diff: reduced` in `versions.json`; a file without a patch has a `# ` line that says so; a rename the forge did not cut needs none. A clone that comes later replaces the reduced patch of the current head.
- **Cleanup**: after the task is archived, the next poll of its context and host deletes `refs/loom/mr/<remote>/<n>/*` (`git update-ref -d`), from the clones in `index.db` and each indexed clone of the project, only the exact `head`, `base` and `reviewed` refs, unless that remote now names another project. A clone has one set of refs per MR: they stay while an active Review task of any context has the MR. A lock or a failure is retried by the next poll, and each archived task is looked at once more after a restart.
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

The repo card gets, from the forge, for repos of a host with a credential: project ID, default branch on the forge, visibility, archived, web URL, open MRs count. Refreshed on the schedule of the MR poll (`poll_interval`, its own thread until the MR poll exists), and at once for a repository without metadata; no backoff: a token refused twice or a forge not reachable stops the reads of the credential until its next poll; shown by `loom repo show`, never used for updates (the git state stays the source for branches).

## `loom pr`

`loom pr [<task>]` opens a draft MR for the current branch, linked to the task (default: the task of the current branch): `git push -u -- <remote> refs/heads/<b>:refs/heads/<b>` (git CLI, user's credentials), then the forge API (`POST /repos/<o>/<r>/pulls` with `draft: true`, GitLab `POST /projects/<id>/merge_requests` with `Draft:` title). Title and body from the task (title, kind template, links). The MR becomes a `mr` link of the task (role `delivery`) and a `mr` event. Refuses on the default branch and on a branch with no commit ahead of the base. When an MR already exists for the branch, it links it instead.

- **Who does what**: `loom` reads the branch and checks it; a `dry_run` request to `POST /v1/tasks/{id}/mr` has loomd check that the task can change (not archived, not read-only) and that the context has a credential for the host, sending nothing and not running `token_cmd` (it could prompt); then `loom` pushes (the user's terminal and git credentials), and the same request without `dry_run` asks loomd, which holds the forge credential of the context of the task, to search the open MRs of the branch and open the draft. A refused push sends nothing to the forge.
- **Remote and base**: the upstream remote of the branch, otherwise the primary remote; the project is the host and path of its URL. The base is `--base`, otherwise the default branch: `refs/remotes/<remote>/HEAD`, otherwise the default branch of the repo index; unknown is a refusal, also with `--base` (the check "not the default branch" needs it). A branch whose upstream is `.` (a local branch), a URL, or a branch with another name (`branch.<b>.merge`) is refused, since the push goes to `<b>` of a named remote. Commits ahead are counted against `<remote>/<base>`, otherwise the local `<base>`. The head is a branch of the same project: an MR from a fork is out of v0.4, and an open MR of a fork with the same branch name is not linked.
- **Description**: the body template of the kind and the `origin` and `related` links. Never the notes: `task.md` is private (storage).
- **Ledger**: an `edited` event adds the `delivery` link, then an `mr` event (`opened` or `linked`): a version without the `mr` type still replays the link. Linking an MR the task has already records nothing.
- `-n` prints the push and the two forge requests, and sends nothing.

## Out of scope for v0.4

Agent findings inline (v0.7), comments outside hunks, file-level comments, image diffs, editing or deleting published comments, multi-machine (one poller per context, later), a loom-owned clone when no local clone exists.
