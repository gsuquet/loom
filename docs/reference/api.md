# API reference

> Transport and endpoints of the loomd API, and the types of the `loom-api` crate, shared by the loomd API and the CLI `--json` output, as shipped.

## Transport

| Property | Value |
| --- | --- |
| Protocol | HTTP/1.1, JSON bodies. |
| Socket | Unix socket `<loom home>/run/loomd.sock`, mode `0600` (see [configuration](configuration.md)). |
| Authentication | None: access is limited by the socket and `run/` modes to the user running `loomd`. |
| Response `content-type` | `application/json`, for successes and errors. |

`HEAD` is accepted wherever `GET` is.

## Endpoints

| Method | Path | Success | Body |
| --- | --- | --- | --- |
| `GET` | `/v1/status` | `200` | [`StatusResponse`](#statusresponse) |
| `GET` | `/v1/repos` | `200` | [`RepoList`](#repolist) |
| `GET` | `/v1/repos/{context}/{name}` | `200` | [`RepoSummary`](#reposummary); with `?refresh=true`, after reading its state |
| `DELETE` | `/v1/repos/{context}/{name}` | `200` | [`RepoSummary`](#reposummary) of the removed repository |
| `POST` | `/v1/repos/scan` | `200` | [`ScanResult`](#scanresult); with `?rebuild=true`, of a rebuild; with `?dry_run=true`, nothing applied |
| `POST` | `/v1/repos/register` | `200` | [`RepoSummary`](#reposummary) |
| `POST` | `/v1/repos/sync` | `200` | [`SyncResult`](#syncresult) |
| `GET` | `/v1/search` | `200` | [`SearchResults`](#searchresults) |
| `GET` | `/v1/diagnostics` | `200` | [`DaemonDiagnostics`](#daemondiagnostics) |
| `POST` | `/v1/diagnostics/remote-check` | `200` | [`RemoteCheckResult`](#remotecheckresult) |
| `POST` | `/v1/tasks/reindex` | `200` | [`ReindexResult`](#reindexresult) |
| `GET` | `/v1/tasks` | `200` | [`TaskList`](#tasklist) |
| `POST` | `/v1/tasks` | `200` | [`TaskDetail`](#taskdetail) of the new task; takes a [`NewTask`](#newtask) |
| `GET` | `/v1/tasks/{id}` | `200` | [`TaskDetail`](#taskdetail) |
| `PATCH` | `/v1/tasks/{id}` | `200` | [`TaskDetail`](#taskdetail) after the edit; takes a [`TaskEdit`](#taskedit) |
| `POST` | `/v1/tasks/{id}/actions/{action}` | `200` | [`TaskDetail`](#taskdetail) after the action; takes a [`TaskActionRequest`](#taskactionrequest) or no body |
| `GET` | `/v1/tasks/{id}/events` | `200` | [`TaskEvents`](#taskevents) |
| `POST` | `/v1/tasks/{id}/undo` | `200` | [`TaskDetail`](#taskdetail) after the undo; no body |
| `GET` | `/v1/tasks/backup` | `200` | [`BackupReport`](#backupreport) |
| `POST` | `/v1/tasks/sync` | `200` | [`BackupReport`](#backupreport) |

`uptime_secs` is computed, and `<loom home>/state/update.json` read for `update`, when the request is handled. An unreadable state file is logged and reported as an empty state. `disk` is the last measure of the loom home while it is fresh, otherwise a new one ([cache](#diskusage)).

### Repositories

The repository index is `<loom home>/index.db` (see [configuration](configuration.md#indexdb)). `loomd` scans when it starts and on `POST /v1/repos/scan`; reads never wait for a scan and return the index before or after it, never a part of it.

`GET /v1/repos` query parameters, both optional; an empty value is ignored, any other parameter is an `invalid_request`:

| Parameter | Content |
| --- | --- |
| `context` | Only the repositories of this context. A context name (the rule of `-c`), or `unassigned`. |
| `query` | Only the repositories whose name, slug, path, remote URLs or context contain this text, ASCII case ignored. Plain substring: `%` and `_` match themselves. |

Repositories are sorted by slug.

`/v1/repos/{context}/{name}` addresses a repository by its slug `<context>/<name>`, the two parts as two path segments (`/v1/repos/personal/loom-2` for the slug `personal/loom-2`). Characters outside the URL path syntax are percent-encoded.

`GET /v1/repos/{context}/{name}` query parameter, optional; any other parameter is an `invalid_request`:

| Parameter | Content |
| --- | --- |
| `refresh` | `true`: read the repository's [state](#repostate) now (within its time budget, on a thread of its own), store it, and return the repository with it. `false` (default): return the stored state. |

`POST /v1/repos/scan` walks the roots (`[repos] roots`, then every `match.paths`, `~` expanded with `HOME`) and reads every registered repository in place, then applies the changes to the index in one transaction:

- a repository found is added, or updated when its identity changed (remotes, context, forge or slug);
- a repository no longer found under a scanned root is removed, unless an error hides it (an unreadable directory above it, an unreadable `.git` or config); the repositories under other roots are kept;
- a registered repository is kept while its `.git` exists, even deeper than `max_depth`, excluded, or outside the roots, and removed once its `.git` is gone;
- contexts are recomputed for every repository, so slugs follow a rule change.

`POST /v1/repos/scan` query parameters, both optional, `true` or `false` (default); any other parameter is an `invalid_request`:

| Parameter | Content |
| --- | --- |
| `rebuild` | `true`: the index is replaced, in one transaction, by the repositories the roots hold. Registered repositories are not read; every repository, its state, `registered` flag and `last_used` is dropped. The result counts every repository of the index as removed and every repository found as added (`updated` is `0`). |
| `dry_run` | `true`: the changes are computed and returned, nothing is applied, no state is read. |

Scans run one at a time. A plain scan request (neither parameter `true`) made while a plain scan runs waits for that scan and receives its result; a rebuild or a dry run runs after it, on its own. Registration and removal requests wait for a running scan.

`POST /v1/repos/register` takes a [`RegisterRequest`](#registerrequest). The repository is the path itself or its nearest ancestor with a `.git` entry (directory, file or symbolic link); symbolic links in the path are kept. It is identified like a scanned repository, added or updated, and marked `registered`. A repository already indexed keeps its slug while its context and name do not change; otherwise it gets `<context>/<name>`, or the lowest free `<context>/<name>-N` (N ≥ 2).

With `on_use: true` (registration on use, sent by `loom`), a repository already in the index is left as it is: not read again, not marked `registered`; only its `last_used` is set to now. It is found by its path, otherwise among the repositories with the same directory name by canonical path (the same directory through other symbolic links). A repository not in the index is registered as above, then gets its `last_used`.

`DELETE /v1/repos/{context}/{name}` removes the repository from the index, registered or not. A scan adds a repository found under a root again.

#### State refresh

`loomd` reads the [state](#repostate) of a repository with `gix`, never starting git, and stores it in the index. It reads it:

- after each scan, for the repositories added or updated by the scan;
- after the scan on start, then every `[repos] fetch_interval` (default 1 hour), for every repository;
- after a registration, for the registered repository;
- after each [update](#updates) that ran git, for that repository, stored with its result;
- on `GET /v1/repos/{context}/{name}?refresh=true`, for that repository, before answering.

Except on `?refresh=true`, states are read in the background by at most 4 threads (fewer on a machine with fewer cores), one repository each, and each repository is queued at most once at a time. Reads of the index never wait for them. A repository without a state yet has `state: null`.

#### Updates

`loomd` updates every repository of the index on a schedule: fetch of the primary remote, then fast-forwards under the conditions of its update policy ([how updates work](../explanation/repo-updates.md)). Each update stores its result as the repository's [`last_update`](#lastupdate), and the state read after it.

| Property | Value |
| --- | --- |
| First update | At once for a repository never updated (within a second of the scan that indexed it); otherwise one interval after its `last_update`. |
| Interval | `[repos] fetch_interval` (default 1 hour) after the end of the previous update, ±10 % at random. The index is read again every 10 seconds for new repositories. |
| Concurrency | At most `[repos] fetch_concurrency` updates at once (default 4, at most 64). |
| Backoff | After a failed fetch, the repository's next update waits 2, 4, then 8 times the interval (never more), counting consecutive failures. Every repository of the same host (`host[:port]` of the primary remote, or its URL for a local path) waits as long, unless a fetch of that host that started earlier succeeded in the meantime. A successful fetch clears the host's backoff. Other hosts are not affected. A fetch that met a lock held by another git process (`busy`) neither backs off nor counts as a failure: the next update is one interval away. |
| Policy | `[repos."<path>"] update`, otherwise the context's `update`, otherwise `ff`; `fetch` for an `unassigned` repository without `[repos."<path>"] update`. |
| git commands | `git fetch --no-prune --no-prune-tags --no-tags <primary>` (timeout 120 s; no tag is fetched, whatever `fetch.pruneTags`, `remote.<name>.pruneTags` or `remote.<name>.tagOpt` say); `git merge --ff-only --no-autostash <commit>` (10 min); `git fetch --no-prune --no-prune-tags --no-tags <primary> <merge ref>:refs/heads/<default branch>` (120 s). On timeout, the command's process group gets `SIGTERM`, then `SIGKILL` 5 s later. A command that fails only because another git process holds a lock of the repository (`Unable to create '….lock': File exists`, `cannot lock ref '…': is at … but expected …`) gives the skip reason `busy`; the lock is never removed. |
| git environment | `loomd`'s environment and the user's git configuration and hooks, plus `GIT_TERMINAL_PROMPT=0`, `GIT_CEILING_DIRECTORIES=<parent of the working tree>`, `GIT_SSH_COMMAND="ssh -o BatchMode=yes"` unless `core.sshCommand` (read with `git config --get`), `GIT_SSH_COMMAND` or `GIT_SSH` is set; `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_COMMON_DIR`, `GIT_NAMESPACE` and `GIT_PREFIX` removed. |

`POST /v1/repos/sync` takes a [`SyncRequest`](#syncrequest) and updates the repositories it names (every repository of the index when it names none) now, regardless of their schedule and of the backoff, then answers with a [`SyncResult`](#syncresult), sorted by slug, after every result is stored. A repository already being updated is updated again once that update ends. The answer may take as long as the updates (each command has its timeout). With `dry_run: true`, nothing runs: each state is read now and the result says what an update would do.

#### Remote check

`POST /v1/diagnostics/remote-check` (no body) checks that each remote endpoint of the index answers `loomd`'s git, and answers with a [`RemoteCheckResult`](#remotecheckresult) (the checks of `loom repo doctor`).

| Property | Value |
| --- | --- |
| Endpoints | One per distinct endpoint and scheme of the primary remotes of the index (see [`RemoteCheck`](#remotecheckresult)). |
| Repository | One repository of the endpoint: one whose last update did not fail to fetch when there is one, then the lowest slug. |
| git command | `git -C <repository> ls-remote --exit-code <primary remote> HEAD`, with the [git environment](#updates) of the updates: the user's git configuration (`core.sshCommand`, `includeIf`, credential helpers), `GIT_TERMINAL_PROMPT=0`, SSH `BatchMode` unless an SSH command is configured. Nothing is written to the repository. |
| Timing | Every endpoint at once, each stopped after 20 s (process group `SIGTERM`, then `SIGKILL` 5 s later). |
| Result | `reachable` when git exits with `0`, or with `2` (no `HEAD` on the remote, such as an empty repository); otherwise `failed` with the kind of failure read from git's error output. |

### Search

`GET /v1/search` searches the repositories of the index. Query parameters; an empty `context` is ignored, any other parameter is an `invalid_request`:

| Parameter | Content |
| --- | --- |
| `q` | Required. The query: words separated by whitespace. |
| `context` | Only the repositories of this context. A context name (the rule of `-c`), or `unassigned`. |
| `limit` | Most results returned, at least `1`. Default `20`. |

Matching:

- The searched fields are the repository's name, slug, path, remote URLs, context name and branch (the branch `HEAD` is on in its stored [state](#repostate), unborn included; empty when detached or not read yet).
- Each field is split into words on every character that is not a letter or a digit (`loom-api` is `loom` and `api`). Case and diacritics are ignored: `cafe` finds `Café`, `viet` finds `Việt`.
- A repository matches when every word of the query starts a word of one of its fields, not necessarily the same field. A query word made of punctuation only is ignored; a query without words returns no result.
- Query words are text only: `"`, `*`, `:`, `(`, `)`, `^`, `-`, `+`, `{`, `}` act as word separators, and `AND`, `OR`, `NOT`, `NEAR` are words to find.

Results are ranked by these keys, in order:

1. How the repository matches: its name is the query (the same words, in order, such as `loom-api` for `loom api`), then every query word starts a word of its name, then every query word starts a word of its slug, then the other cases.
2. `last_used`, most recent first; never used last.
3. Full-text relevance (SQLite FTS5 `bm25`), most relevant first.
4. Slug.

The ranking is computed on every match, then cut to `limit`.

### Tasks

The tasks are files in the context stores (see [configuration](configuration.md#context-stores)), indexed in `<loom home>/index.db`. `loomd` reconciles them with the index when it starts and when their files change, recording hand edits (see [context stores](configuration.md#context-stores)).

`POST /v1/tasks/reindex` (no body) rebuilds the tasks index from the files of every context store, in one transaction, and answers with a [`ReindexResult`](#reindexresult) once it is written. Rows of tasks whose directory is gone are dropped; an invalid `task.md` is indexed read-only and listed, never rewritten. Before the rebuild, every task is reconciled: a hand edit not recorded yet gets its `edited` event first, so a reindex may append events, and the rebuild never hides an edit. The repositories of the index are left as they are. A task whose files cannot be read (permissions, I/O), or a store whose `tasks/` cannot be listed, by the reconciliation or by the rebuild, is listed in `failed` and keeps the row it had; the other tasks are indexed. Only a failure to write the index answers `500 internal`, the index then unchanged.

#### Task endpoints

`{id}` is a task ID or a prefix of one, read as Crockford base32: case ignored, `i` and `l` read as `1`, `o` as `0`, 1 to 8 characters. It is resolved against every indexed task, of every context. A prefix several tasks start with is an `ambiguous_prefix` error listing them; `/v1/tasks/reindex` is the reindex endpoint, never a task.

Reads (`GET /v1/tasks`, `GET /v1/tasks/{id}`) use a read connection to the index and never wait for a write. Creations, edits, actions, undos and event reads run on the task thread, one at a time, after the hand edits and reindexes queued before them: two requests on one task never interleave and none is lost. Their events have the actor `user` and the `id` of [`machine.toml`](configuration.md#machinetoml). A request that changes nothing (pin a pinned task, edit a field to its value) writes nothing and answers the task as it is. A write is committed to the git repository of its context store before the answer, one commit per request ([store repository](configuration.md#store-repository)); a commit that fails does not fail the request.

`GET /v1/tasks` query parameters, all optional; an empty `context` or `status` is ignored, any other parameter is an `invalid_request`:

| Parameter | Content |
| --- | --- |
| `context` | Only the tasks of this context. A context without a store gives an empty list. |
| `status` | Only the tasks with this status: `inbox`, `ready`, `active`, `waiting`, `done`, `cancelled`. Tasks with an invalid `task.md` or an unknown status are left out. |
| `ranked` | `true`: the priority list, in its order, each task with its `rank`. `false` (default): every task, sorted by ID. |

The priority list holds the `ready` and `active` tasks, and the `waiting` tasks whose `until` is today or past; `inbox`, `done`, `cancelled`, unknown statuses and invalid files are never in it. Order, each rule breaking the ties of the previous one: pinned first; overdue or due today first, earliest due date first; importance, `p0` first; due date, earliest first, none last; oldest `created` first; ID. "Today" is the local date of `loomd` (`TZ`, otherwise `/etc/localtime`), read at each request. With `context` or `status`, the list is the priority list of the tasks they keep.

`{action}` of `POST /v1/tasks/{id}/actions/{action}`:

| Action | Body | Change |
| --- | --- | --- |
| `inbox`, `ready`, `active`, `done`, `cancelled` | none | Moves the task to this status. |
| `waiting` | `on`, `until`, both optional | Moves the task to `waiting` (or changes what it waits on and until when). |
| `pin`, `unpin` | none | Pins or unpins the task. |
| `log_note` | `text`, required | Logs a note as a `note_logged` event. |

An open status (`inbox`, `ready`, `active`, `waiting`) can move to any other one, `waiting` to `waiting` too; `done` and `cancelled` only to `ready`. Any other action name is a `not_found`. An empty body (or only whitespace) is no body; a field the action does not take is an `invalid_request`.

`POST /v1/tasks/{id}/undo` reverts the last change of the task (an edit, a status change, a pin, a hand edit or an undo; notes and the creation are not changes) by appending an `undone` event with the inverse changes. Undoing an undo redoes the change. `loomd` computes the inverse from the task's log; the request has no body.

`GET /v1/tasks/{id}/events` answers the event log of the task as `events.jsonl` holds it, with the lines this version cannot replay listed apart.

`GET /v1/tasks/backup` answers with the backup state of every context store held on this machine ([`BackupReport`](#backupreport)), without running git: the last commit failure and the last sync of each store since loomd started.

`POST /v1/tasks/sync` with a [`BackupRequest`](#backuprequest) body syncs the stores ([store backup](configuration.md#store-backup)) and answers once every sync ended, with the stores synced. `400 invalid_request` when `context` names a context without `store.backup.remote`, one not held on this machine, or an unknown one, or when the body is invalid. A sync that fails is in the answer, not an error status.

### Errors

Every error response has an [`ApiError`](#apierror) body.

| Case | HTTP status | `code` |
| --- | --- | --- |
| No endpoint at the path | `404` | `not_found` |
| Endpoint exists, method not accepted | `405` (with an `allow` header) | `invalid_request` |
| No repository with the slug of `/v1/repos/{context}/{name}`, or with a slug of a `SyncRequest` | `404` | `not_found` |
| Sync: body not a valid `SyncRequest` | `400` | `invalid_request` |
| Invalid query parameter, unknown parameter, invalid `context`, or `refresh`, `rebuild` or `dry_run` neither `true` nor `false` | `400` | `invalid_request` |
| Register: body not a valid `RegisterRequest`, path not absolute or with a `..` component, no repository at or above the path, or a repository whose `.git` or config cannot be read | `400` | `invalid_request` |
| Task: `{id}` not 1 to 8 Crockford base32 characters | `400` | `invalid_request` |
| Task: no task ID starts with `{id}` | `404` | `not_found` |
| Task: several task IDs start with `{id}` | `400` | `ambiguous_prefix`, `details`: [`AmbiguousPrefix`](#problem-details) |
| Task: body not a valid `NewTask`, `TaskEdit` or `TaskActionRequest` (unknown field, missing `text`, field the action does not take), empty title or note, unknown kind, format or status, no store for the `context` of a new task, invalid `status` or `ranked` | `400` | `invalid_request` |
| Task: unknown `{action}` | `404` | `not_found` |
| Task: change of a task whose kind, format or status is unknown, or whose `task.md` is invalid | `409` | `read_only`, `details`: [`TaskReadOnly`](#problem-details) |
| Task: status change the transitions forbid | `409` | `illegal_transition`, `details`: [`IllegalTransition`](#problem-details) |
| Task: undo of a change whose field changed since (a hand edit not recorded yet) | `409` | `conflict`, `details`: [`UndoConflict`](#problem-details) |
| Task: undo of a task without a change | `409` | `nothing_to_undo` |
| Task: the task files cannot be read or written | `500` | `internal` |
| The index cannot be read or written | `500` | `internal` |
| The daemon cannot read its clock as a timestamp | `500` | `internal` |
| The daemon stops before answering a scan, register, remove, refresh, sync, reindex or task write request | `503` | `unavailable` |

```bash
curl --unix-socket ~/.loom/run/loomd.sock http://loomd/v1/status
curl --unix-socket ~/.loom/run/loomd.sock 'http://loomd/v1/repos?context=personal&query=loom'
curl --unix-socket ~/.loom/run/loomd.sock 'http://loomd/v1/repos/personal/loom?refresh=true'
curl --unix-socket ~/.loom/run/loomd.sock -X POST http://loomd/v1/repos/scan
curl --unix-socket ~/.loom/run/loomd.sock -X POST -H 'content-type: application/json' \
  -d '{"path": "/home/me/src/tool"}' http://loomd/v1/repos/register
curl --unix-socket ~/.loom/run/loomd.sock -X DELETE http://loomd/v1/repos/unassigned/tool
curl --unix-socket ~/.loom/run/loomd.sock -X POST -H 'content-type: application/json' \
  -d '{"repos": ["personal/loom"], "dry_run": true}' http://loomd/v1/repos/sync
curl --unix-socket ~/.loom/run/loomd.sock -X POST 'http://loomd/v1/repos/scan?dry_run=true'
curl --unix-socket ~/.loom/run/loomd.sock http://loomd/v1/diagnostics
curl --unix-socket ~/.loom/run/loomd.sock -X POST http://loomd/v1/diagnostics/remote-check
curl --unix-socket ~/.loom/run/loomd.sock 'http://loomd/v1/tasks?ranked=true&context=work'
curl --unix-socket ~/.loom/run/loomd.sock -X POST -H 'content-type: application/json' \
  -d '{"title": "Review the API", "kind": "review", "context": "work", "status": "inbox"}' \
  http://loomd/v1/tasks
curl --unix-socket ~/.loom/run/loomd.sock -X PATCH -H 'content-type: application/json' \
  -d '{"due": "2026-10-20", "importance": "p0"}' http://loomd/v1/tasks/k3q
curl --unix-socket ~/.loom/run/loomd.sock -X POST -H 'content-type: application/json' \
  -d '{"on": "Alice"}' http://loomd/v1/tasks/k3q/actions/waiting
curl --unix-socket ~/.loom/run/loomd.sock -X POST http://loomd/v1/tasks/k3q/undo
```

## Types

The JSON Schema (draft 2020-12) of each type is generated from the Rust types and committed under [`schemas/`](schemas/). A test fails when a committed schema differs from the types; `mise run docs` regenerates them.

| Type | Schema |
| --- | --- |
| `AmbiguousPrefix` | [`schemas/ambiguous_prefix.json`](schemas/ambiguous_prefix.json) (`details` of `ambiguous_prefix`) |
| `ApiError` | [`schemas/api_error.json`](schemas/api_error.json) |
| `ApiVersion` | [`schemas/api_version.json`](schemas/api_version.json) |
| `ContextAssignment` | [`schemas/context_assignment.json`](schemas/context_assignment.json) (in `RepoSummary`) |
| `DaemonDiagnostics` | [`schemas/daemon_diagnostics.json`](schemas/daemon_diagnostics.json) |
| `DoctorReport` | [`schemas/doctor_report.json`](schemas/doctor_report.json) (`loom repo doctor --json`, no endpoint) |
| `ErrorCode` | [`schemas/error_code.json`](schemas/error_code.json) |
| `IllegalTransition` | [`schemas/illegal_transition.json`](schemas/illegal_transition.json) (`details` of `illegal_transition`) |
| `LastUpdate` | [`schemas/last_update.json`](schemas/last_update.json) (in `RepoSummary` and `SyncResult`) |
| `NewTask` | [`schemas/new_task.json`](schemas/new_task.json) |
| `RegisterRequest` | [`schemas/register_request.json`](schemas/register_request.json) |
| `ReindexResult` | [`schemas/reindex_result.json`](schemas/reindex_result.json) |
| `BackupReport` | [`schemas/backup_report.json`](schemas/backup_report.json) |
| `BackupRequest` | [`schemas/backup_request.json`](schemas/backup_request.json) |
| `RemoteCheckResult` | [`schemas/remote_check_result.json`](schemas/remote_check_result.json) |
| `RepoIdentity` | [`schemas/repo_identity.json`](schemas/repo_identity.json) (inlined in `RepoSummary`) |
| `RepoList` | [`schemas/repo_list.json`](schemas/repo_list.json) |
| `RepoState` | [`schemas/repo_state.json`](schemas/repo_state.json) (in `RepoSummary`) |
| `RepoSummary` | [`schemas/repo_summary.json`](schemas/repo_summary.json) |
| `ScanResult` | [`schemas/scan_result.json`](schemas/scan_result.json) |
| `SearchResults` | [`schemas/search_results.json`](schemas/search_results.json) (`SearchResult` inside) |
| `StatusResponse` | [`schemas/status_response.json`](schemas/status_response.json) |
| `SyncRequest` | [`schemas/sync_request.json`](schemas/sync_request.json) |
| `SyncResult` | [`schemas/sync_result.json`](schemas/sync_result.json) |
| `TaskActionName` | [`schemas/task_action_name.json`](schemas/task_action_name.json) (`{action}` of the action endpoint) |
| `TaskActionRequest` | [`schemas/task_action_request.json`](schemas/task_action_request.json) |
| `TaskDetail` | [`schemas/task_detail.json`](schemas/task_detail.json) (`Task`, `Action` inside) |
| `TaskEdit` | [`schemas/task_edit.json`](schemas/task_edit.json) |
| `TaskEvents` | [`schemas/task_events.json`](schemas/task_events.json) (`EventRecord` inside) |
| `TaskList` | [`schemas/task_list.json`](schemas/task_list.json) (`TaskSummary` inside) |
| `TaskReadOnly` | [`schemas/task_read_only.json`](schemas/task_read_only.json) (`details` of `read_only`, `read_only` of a task) |
| `UndoConflict` | [`schemas/undo_conflict.json`](schemas/undo_conflict.json) (`details` of `conflict`) |
| `UpdateState` | [`schemas/update_state.json`](schemas/update_state.json) (file `state/update.json`, see [configuration](configuration.md#state)) |
| `VersionInfo` | [`schemas/version_info.json`](schemas/version_info.json) |

### API versioning

`ApiVersion` is a closed enum serialized as a string.

| Value | Meaning |
| --- | --- |
| `v1` | First version of the API. Current version. |

A breaking change to any API type adds a new value. Compatible changes (new optional fields, new endpoints) keep the current value.

### Unknown fields

| Message | Unknown fields |
| --- | --- |
| Request | Rejected (`#[serde(deny_unknown_fields)]`, or key by key for `NewTask` and `TaskEdit`): `RegisterRequest`, `NewTask`, `TaskEdit`, `TaskActionRequest`, and the query parameters of `GET /v1/repos`, `GET /v1/repos/{context}/{name}`, `POST /v1/repos/scan` and `GET /v1/tasks`. |
| Response | Ignored, so that an older client can read a newer daemon. |

Unknown enum values (`api_version`, `code`, `channel`, `channel_source`, `profile`, `policy`, `kind`, `operation`, `state`, `status`) are rejected, except the `kind`, `format` and `status` of a task, which a newer version may write: the task is then read-only.

### Scalar encodings

| Kind | Encoding | Example |
| --- | --- | --- |
| Instant (`Timestamp`) | RFC 3339 string in UTC, whole seconds: `YYYY-MM-DDTHH:MM:SSZ`, years 1970 to 9999. Any other form is rejected: fractional seconds, offsets other than `Z`, lowercase `t` or `z`, leap seconds (`:60`). | `"2026-10-12T08:00:00Z"` |
| Duration | Unsigned integer of whole seconds; the field name ends in `_secs`. | `90` |
| Path | String. Paths that are not valid UTF-8 cannot be serialized. | `"/run/user/1000/loom/loomd.sock"` |

`VersionInfo.commit_date` predates `Timestamp`: it is the git committer date with its original offset (`2026-10-12T10:00:00+02:00`) or `unknown`.

### `StatusResponse`

State of a running loomd.

| Field | Type | Content |
| --- | --- | --- |
| `api_version` | `ApiVersion` | API version the daemon serves. |
| `daemon` | `VersionInfo` | Build information of the daemon binary (same fields as `loom version --json`, see [`loom version` output](cli/conventions.md#loom-version-output)). |
| `pid` | integer (u32) | Process ID of the daemon. |
| `started_at` | `Timestamp` | Instant the daemon started. |
| `uptime_secs` | integer (u64) | Whole seconds elapsed since `started_at`. |
| `loom_home` | path | Absolute path of the loom home directory the daemon uses. |
| `socket_path` | path | Absolute path of the unix socket the daemon listens on. |
| `config_path` | path | Absolute path of the configuration file the daemon loaded. |
| `update` | [`UpdateStatus`](#updatestatus) or `null` | Auto-upgrade policy and state. Always sent by this version; optional when reading, so that a client reads a daemon older than this field. |
| `disk` | [`DiskUsage`](#diskusage) or `null` | Disk usage of the loom home. Sent by this version (`null` only when the measure failed unexpectedly); optional when reading, so that a client reads a daemon older than this field. |

Every field is required, except `update` and `disk`.

```json
{
  "api_version": "v1",
  "daemon": {
    "version": "0.1.0-dev.42",
    "commit": "0123456789abcdef0123456789abcdef01234567",
    "commit_date": "2026-10-12T10:00:00+02:00",
    "target": "aarch64-apple-darwin",
    "channel": "dev",
    "profile": "release"
  },
  "pid": 4242,
  "started_at": "2026-10-12T08:00:00Z",
  "uptime_secs": 90,
  "loom_home": "/home/me/.local/share/loom",
  "socket_path": "/run/user/1000/loom/loomd.sock",
  "config_path": "/home/me/.config/loom/config.toml",
  "update": {
    "policy": "auto",
    "channel": "dev",
    "channel_source": "chosen",
    "check_interval_secs": 21600,
    "last_check": "2026-10-12T08:05:00Z",
    "available": null,
    "pending": null,
    "failed_versions": [],
    "last_result": { "kind": "up_to_date", "at": "2026-10-12T08:05:00Z" }
  },
  "disk": {
    "computed_at": "2026-10-12T08:01:30Z",
    "total_bytes": 35012608,
    "versions_bytes": 30027776,
    "index_bytes": 4206592,
    "index_wal_bytes": 4120576,
    "logs_bytes": 24576,
    "state_bytes": 8192,
    "other_bytes": 745472,
    "contexts": [],
    "kept_versions": ["0.1.0-dev.40", "0.1.0-dev.41"],
    "available_bytes": 120394752000,
    "warnings": []
  }
}
```

### `DiskUsage`

Disk usage of the loom home, measured by `loomd`.

| Field | Type | Content |
| --- | --- | --- |
| `computed_at` | `Timestamp` | Instant of the measure. |
| `total_bytes` | integer (u64) | The whole loom home: the sum of every part below. |
| `versions_bytes` | integer (u64) | `versions/`: previous versions kept for `loom rollback`. |
| `index_bytes` | integer (u64) | `index.db`, `index.db-wal` and `index.db-shm`. |
| `index_wal_bytes` | integer (u64) | `index.db-wal` alone, included in `index_bytes`. |
| `logs_bytes` | integer (u64) | `log/`. |
| `state_bytes` | integer (u64) | `state/`. |
| `other_bytes` | integer (u64) | Everything else: the home directory itself, `config.toml`, `run/`, `staging/`, `contexts/` itself and the files directly in it, any other entry. |
| `contexts` | list of objects | One per directory under `contexts/`, sorted by `name`: `name` (directory name, lossy UTF-8) and `bytes`. Empty when there is none. |
| `kept_versions` | list of strings | Names of the directories in `versions/`, sorted. |
| `available_bytes` | integer (u64) \| `null` | Bytes an unprivileged process can still write on the volume of the loom home (`statvfs`: `f_bavail` × `f_frsize`); `null` when the query failed. |
| `warnings` | list of strings | What could not be measured, in English (`cannot read <path>: <error>`). |

| Property | Value |
| --- | --- |
| Sizes | Allocated bytes: `st_blocks` × 512 of each entry, not file lengths. A directory counts its own blocks too. |
| Symbolic links | Counted as links (their own blocks), never followed. The loom home itself may be a symbolic link: it is followed once. |
| Hard links | A file with several links counts once. |
| Bounds | Only entries under the loom home are read. Directories mounted under it are walked like any other. |
| Errors | Never an error response. A directory or entry that cannot be read counts as 0 and adds a warning, also logged at `warn` level. A missing `versions/`, `log/`, `state/` or `contexts/` counts as 0 without a warning. An entry deleted during the walk is skipped. |
| Cache | Measured on a blocking thread at the first `GET /v1/status`, then reused for 5 minutes. Measured again earlier when the modification time of `versions/` changed (a version saved, restored or pruned by an upgrade or a rollback). A new `loomd` (after an upgrade, a rollback or a restart) starts without a measure. |

### `UpdateStatus`

| Field | Type | Content |
| --- | --- | --- |
| `policy` | `auto` \| `notify` \| `off` | Effective `[update] policy` (configured, or the default of the build channel). |
| `channel` | `stable` \| `dev` \| `null` | Effective channel: `[update] channel`, otherwise the channel chosen with `loom upgrade --channel`, otherwise the build channel ([channel resolution](configuration.md#channel-resolution)); `null` for a local build without a configured or chosen channel: nothing is checked. |
| `channel_source` | `config` \| `chosen` \| `build` \| `null` | Where `channel` comes from; `null` when `channel` is. Always sent by this version; optional when reading, so that a client reads a daemon older than this field. |
| `check_interval_secs` | integer (u64) | Effective interval between checks, before jitter. |
| `last_check` | `Timestamp` \| `null` | `last_check` of the state. |
| `available` | string \| `null` | `available` of the state, `null` when it is not newer than the daemon's version. |
| `pending` | object \| `null` | `pending` of the state: `from`, `to`, `started_at`. |
| `failed_versions` | list of strings | `failed_versions` of the state. |
| `last_result` | object \| `null` | `last_result` of the state, tagged by `kind` ([kinds](configuration.md#state)). |

### `RepoSummary`

A repository of the index: the fields of [`RepoIdentity`](schemas/repo_identity.json) (`path`, `name`, `slug`, `context`, `remotes`, `primary_remote`, `forge`), inlined, plus:

| Field | Type | Content |
| --- | --- | --- |
| `registered` | boolean | Added by `POST /v1/repos/register`: scans keep it while its `.git` exists. |
| `last_used` | `Timestamp` or absent | Last time the repository was used: a registration on use (`on_use`), sent by `loom` run inside it and by `loom repo show`. Kept by scans, dropped by a rebuild. |
| `state` | [`RepoState`](#repostate) \| `null` | State of the repository, `null` until it is read for the first time (see [state refresh](#state-refresh)). |
| `last_update` | [`LastUpdate`](#lastupdate) \| `null` | Result of the last [update](#updates), `null` until the first one. Kept across state refreshes and scans. |

```json
{
  "path": "/home/me/perso/loom",
  "name": "loom",
  "slug": "personal/loom",
  "context": { "rule": "path", "context": "personal", "prefix": "~/perso" },
  "remotes": [
    {
      "name": "origin",
      "url": "git@github.com:gsuquet/loom.git",
      "location": { "host": "github.com", "path": "gsuquet/loom" }
    }
  ],
  "primary_remote": "origin",
  "forge": "github",
  "registered": false,
  "state": null,
  "last_update": null
}
```

### `RepoState`

The state of a repository (its card), read with `gix` without starting git, and without writing to the repository.

Every field but `refreshed_at` and `error` is either `{"known": <value>}` or the string `"unknown"`. A field is unknown when its time budget ran out or it could not be read; `error` then says why. A known `null` (`{"known": null}`) is a value: no operation, no upstream, and so on.

The status walk (`dirty`), the untracked count and the ahead/behind counts share a time budget of 2 seconds per repository, counted from the start of the read; what is not computed in time is unknown, the other fields are still read. The git configuration of the user (`HOME`, `XDG_CONFIG_HOME`, `GIT_CONFIG_*`) and of the system applies (`core.excludesFile`, `status.renames`, `core.abbrev`…); the configuration of the git installation, which only git itself can locate, does not.

| Field | Known value | Content |
| --- | --- | --- |
| `refreshed_at` | `Timestamp` | When the state was read. |
| `head` | object | What `HEAD` points to, tagged by `kind`: `{"kind": "branch", "name": "main"}`, `{"kind": "unborn", "name": "main"}` (a branch without commits) or `{"kind": "detached", "commit": "abc1234"}` (commit id abbreviated per `core.abbrev`, at least 7 digits). A rebase in progress detaches `HEAD`. |
| `dirty` | integer (u64) | Tracked paths with a change between `HEAD`, the index and the working tree: modified, added, deleted, renamed (one path), type-changed or conflicted, staged or not, each path once. A submodule follows its `ignore` configuration. Untracked files are not counted. Filter drivers (`filter.<driver>.clean` or `.process`, such as git-lfs) are never run; built-in conversions (end of line, `ident`, `working-tree-encoding`) apply. When a file changed on disk has a `filter` attribute naming a configured driver, its content cannot be compared: `dirty` is unknown and `error` names the driver and the file. |
| `untracked` | object | `count`: untracked files, each counted (an untracked directory counts its files, an untracked nested repository counts as one), `.gitignore`, `.git/info/exclude` and `core.excludesFile` applied, empty directories ignored; at most 1,000. `capped`: `true` when the count stopped at 1,000. |
| `operation` | string \| `null` | Operation in progress, from the markers of the git directory of the working tree: `rebase` (either backend), `merge`, `cherry-pick`, `revert`, `bisect`, `am`. When several markers exist, the first in this order wins: `am`, `rebase`, `cherry-pick`, `merge`, `bisect`, `revert`. |
| `upstream` | object \| `null` | Upstream of the `HEAD` branch: `name` (short ref name, such as `origin/main`), `ahead` and `behind` (commits of `HEAD` not in the upstream and the reverse, each a known integer or `"unknown"`). `null` when `HEAD` is detached, the branch has no upstream configured, or the upstream ref does not exist (never fetched, or pruned). |
| `default_branch` | string \| `null` | The branch `refs/remotes/<primary remote>/HEAD` points to; otherwise `main`, then `master`, when the branch exists locally or on the primary remote; otherwise `null`. |
| `last_commit` | `Timestamp` \| `null` | Committer date of `HEAD`'s commit; `null` on an unborn branch. |
| `last_fetch` | `Timestamp` \| `null` | Modification time of `FETCH_HEAD`, the latest of the working tree's git directory and of the main one (a linked worktree has its own); `null` when neither exists. |
| `error` | string \| `null` | Why fields are unknown, one reason per field, joined with `; `, such as `dirty: time budget of 2s exceeded`. A repository that cannot be opened has every field unknown but `last_fetch`, and `cannot open the repository: …`. |

```json
{
  "refreshed_at": "2026-10-13T08:00:00Z",
  "head": { "known": { "kind": "branch", "name": "main" } },
  "dirty": { "known": 2 },
  "untracked": { "known": { "count": 1, "capped": false } },
  "operation": { "known": null },
  "upstream": {
    "known": { "name": "origin/main", "ahead": { "known": 1 }, "behind": { "known": 0 } }
  },
  "default_branch": { "known": "main" },
  "last_commit": { "known": "2026-10-12T17:42:10Z" },
  "last_fetch": { "known": null },
  "error": null
}
```

### `LastUpdate`

The result of the last update of a repository, scheduled or requested.

| Field | Type | Content |
| --- | --- | --- |
| `time` | `Timestamp` | When the update ended. |
| `policy` | string | Update policy applied: `ff`, `fetch` or `off`. |
| `outcomes` | list of objects | What happened, in order, never empty: the fetch or why there was none, the branch `HEAD` is on, then the default branch when it was moved or could not be. Each is tagged by `kind`. |

| `kind` | Other fields | Meaning |
| --- | --- | --- |
| `fetched` | | The primary remote was fetched; the branch `HEAD` is on had nothing to bring in, or the policy is `fetch`. |
| `fast_forwarded` | `branch`, `from`, `to` (full commit ids) | The branch `HEAD` is on was fast-forwarded. |
| `default_branch_moved` | `branch`, `from`, `to` | The local default branch, checked out in no working tree, was fast-forwarded. |
| `skipped` | `reason` | A step was not done (below). |
| `failed` | `step` (`fetch`, `fast_forward`, `move_default_branch`), `error` (exit status, `timed out after <n>s`, or `cannot run git: …`), `stderr_tail` (the last 2,000 bytes at most of git's standard error, from a line start) | A git command failed. A failed fetch counts for the [backoff](#updates). |

`reason` is tagged by `code`:

| `code` | Other fields | Meaning |
| --- | --- | --- |
| `policy_off` | | Policy `off`: no git command. |
| `no_remote` | | No remote to fetch: no git command. |
| `unreadable` | `error` | The repository cannot be opened: no git command. |
| `unknown` | `field`: `head`, `dirty`, `operation`, `upstream`, `ahead`, `behind`, `commits`, `default_branch` | A field the decision needs could not be read (time budget, filter driver, read error). |
| `operation` | `operation` | An operation is in progress (`rebase`, `merge`, `cherry-pick`, `revert`, `bisect`, `am`). |
| `dirty` | `paths` | Tracked paths are modified, staged or conflicted. |
| `detached` | | `HEAD` is not on a branch. |
| `unborn` | `branch` | The branch has no commit. |
| `no_upstream` | `branch` | The branch has no upstream, or its ref does not exist. |
| `diverged` | `branch`, `ahead`, `behind` | The branch and its upstream both have commits the other lacks. |
| `merge_refused` | `stderr_tail` | `git merge --ff-only` refused (an untracked file in the way, a change made since the decision). Not retried before the next update. |
| `default_branch_diverged` | `branch`, `ahead`, `behind` | The local default branch and its upstream both have commits the other lacks. |
| `busy` | `step` (`fetch`, `fast_forward`, `move_default_branch`), `stderr_tail` | Another git process (an editor's automatic fetch, a command of the user) held a lock of the repository during this step: `index.lock` or another `*.lock` file existed, or a ref moved under the fetch. Retried at the next interval, without [backoff](#updates); the lock is never removed. Any other failure, or a lock along with another error, stays `failed`. |

```json
{
  "time": "2026-10-13T08:00:00Z",
  "policy": "ff",
  "outcomes": [
    {
      "kind": "fast_forwarded",
      "branch": "main",
      "from": "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d",
      "to": "5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f80"
    }
  ]
}
```

### `SyncRequest`

| Field | Type | Content |
| --- | --- | --- |
| `repos` | list of strings, optional | Slugs of the repositories to update; empty or absent: every repository of the index. |
| `dry_run` | boolean, optional | `true`: run nothing, say what an update would do. Default `false`. |

Unknown fields are rejected.

### `SyncResult`

| Field | Type | Content |
| --- | --- | --- |
| `repos` | list of objects | One per repository, sorted by slug: `slug`, and `result`, tagged by `kind`. |

| `kind` | Other fields | Content |
| --- | --- | --- |
| `updated` | `update`: [`LastUpdate`](#lastupdate) | The update ran; also stored as the repository's `last_update`. |
| `planned` | `policy`, `actions` | Dry run. `actions`, in order, each tagged by `kind`: `skip` (with a `reason`, as in `LastUpdate`), `fetch`, `decide_after_fetch` (whether a branch moves depends on what the fetch brings). With policy `ff`, `decide_after_fetch` is replaced by a `skip` when the working tree already rules out a fast-forward of the branch `HEAD` is on (operation, tracked change, detached or unborn `HEAD`, or one of these unknown). The default branch is never predicted. |

### `RepoList`

| Field | Type | Content |
| --- | --- | --- |
| `repos` | list of [`RepoSummary`](#reposummary) | The repositories, sorted by slug. |

### `SearchResults`

| Field | Type | Content |
| --- | --- | --- |
| `results` | list of `SearchResult` | The results, best first (see [search](#search)), at most `limit`. |

`SearchResult`:

| Field | Type | Content |
| --- | --- | --- |
| `kind` | string | What matched: `repo`. |
| `slug` | string | Slug of the repository. |
| `name` | string | Directory name of the repository. |
| `context` | string | Context name, `unassigned` for a repository without context. |
| `path` | string | Absolute path of the repository. |
| `score` | number | Full-text relevance: SQLite FTS5 `bm25` with its sign flipped, higher is more relevant. One of the ranking keys only: the order of `results` is the ranking. |
| `matched_fields` | list of strings | Fields holding at least one query word: `name`, `slug`, `path`, `remotes`, `context`, `branch`, in this order. |

### `BackupReport`

| Field | Type | Meaning |
| --- | --- | --- |
| `machine` | string | This machine: the `id` of [`machine.toml`](configuration.md#machinetoml). |
| `contexts` | array | One object per store held here, sorted by context; after a sync, the stores synced only. |
| `contexts[].context` | string | The context. |
| `contexts[].path` | string | Root of its store. |
| `contexts[].remote` | string or `null` | `store.backup.remote`, the password of a `scheme://user:password@host` URL replaced with `***`; `null` without one. |
| `contexts[].uncommitted` | string or `null` | Why the last commit of the store failed; `null` when every change is committed. |
| `contexts[].last_sync` | object or `null` | The last sync since loomd started: `time` (when it ended) and `outcome`; `null` before the first one or without a remote. |
| `not_held` | array of strings | Contexts whose `machines` does not name this machine. |

`outcome` is tagged by `kind`:

| `kind` | Fields | Meaning |
| --- | --- | --- |
| `up_to_date` | | The store and its remote hold the same commits. |
| `pushed` | `commits` | Local commits pushed. |
| `pulled` | `commits` | The store fast-forwarded to the remote. |
| `diverged` | `local`, `remote` | Both sides have commits the other lacks: nothing was merged, pushed or pulled. |
| `push_rejected` | `stderr_tail` | The remote refused the push (`[rejected]` or `[remote rejected]`): the commits stay local. |
| `failed` | `step`, `error`, `stderr_tail` | A step failed or timed out. `step`: `commit`, `remote`, `fetch`, `compare`, `push`, `fast_forward`. |

### `BackupRequest`

| Field | Type | Meaning |
| --- | --- | --- |
| `context` | string, optional | The context to sync; absent: every store held here with a `store.backup.remote`. Unknown fields are rejected. |

### `ReindexResult`

Answer of [`POST /v1/tasks/reindex`](#tasks), and output of `loom reindex --json`.

| Field | Type | Content |
| --- | --- | --- |
| `tasks` | integer (u64) | Tasks indexed, invalid ones included. |
| `invalid` | list of `InvalidTaskFile` | Tasks whose `task.md` is invalid, sorted by context and ID. |
| `failed` | list of `FailedTaskFile` | Task directories, or `tasks/` directories of stores, that could not be read, sorted by path. Their rows are kept as they were. Empty when absent. |

`InvalidTaskFile`:

| Field | Type | Content |
| --- | --- | --- |
| `context` | string | Context of the store holding the task. |
| `id` | string | Task ID, the name of its directory. |
| `error` | string | Why the file cannot be read, starting with `line <n>: ` when the line is known: not UTF-8, no `+++` frontmatter, invalid TOML, a missing or invalid key, an `id` that is not the directory name, a `context` that is not the store's. |

`FailedTaskFile`:

| Field | Type | Content |
| --- | --- | --- |
| `path` | path | The task directory, or the `tasks/` directory of a store that could not be listed. |
| `error` | string | What failed, with the file when it is an I/O error. |

### `TaskList`

Answer of `GET /v1/tasks`.

| Field | Type | Content |
| --- | --- | --- |
| `tasks` | list of `TaskSummary` | The tasks the query keeps: sorted by ID, or the priority list with `ranked=true` (see [task endpoints](#task-endpoints)). |

`TaskSummary`:

| Field | Type | Content |
| --- | --- | --- |
| `id` | string | Task ID: 8 lowercase Crockford base32 characters. |
| `prefix` | string | Shortest prefix of `id` no other task ID starts with, across every context (not only the listed tasks). |
| `context` | string | Context of the store holding the task. |
| `task` | `Task` \| `null` | The task, as `task.md` holds it, `updated` being the time of its last event; `null` when `task.md` is invalid. Fields: `id`, `title`, `kind` and `format`, `context`, `status`, `waiting_on` and `waiting_until`, `importance`, `due`, `pinned`, `links`, `relations`, `created`, `updated` (see the [task file](task-file.md)). |
| `read_only` | `TaskReadOnly`, optional | Why the task cannot be changed; absent when it can. |
| `rank` | object, optional | With `ranked=true` only: `position` (from 1) and `reason`, the rule that placed the task after the previous one: `first`, `pinned`, `urgent`, `importance`, `due_date`, `age`, `id`. |

`TaskReadOnly`, tagged by `reason`, with the name or the error in `value`: `unknown_kind`, `unknown_content_format`, `unknown_status` (written by a newer version of loom), `invalid_file` (why `task.md` cannot be read).

### `TaskDetail`

Answer of `GET /v1/tasks/{id}` and of every task write: the fields of `TaskSummary` (without `rank`), and:

| Field | Type | Content |
| --- | --- | --- |
| `body` | string | The notes after the frontmatter; the whole file when `task.md` is invalid. |
| `actions` | list of `Action` | What the task accepts, empty when it is read-only: `{"action": "set_status", "status": …}` for each status it can move to, then `{"action": "edit"}` (`PATCH`), `{"action": "pin"}` or `{"action": "unpin"}`, `{"action": "log_note"}`. |

### `TaskEvents`

Answer of `GET /v1/tasks/{id}/events`.

| Field | Type | Content |
| --- | --- | --- |
| `id` | string | Task ID. |
| `events` | list of `EventRecord` | The events, in log order: `v`, `id`, `at`, `actor` (tagged by `type`: `user`, `file`, `daemon`, `agent` with `name`), `machine_id`, and the event tagged by `type`: `created` (`task`), `edited` (`changes`), `status_changed` (`from`, `to`), `note_logged` (`text`), `undone` (`of`, `changes`). Each change has `field`, `from` and `to`. |
| `skipped` | list of objects | Lines not replayed (newer schema version, unknown type, invalid): `line`, `raw`, `reason`. |

### `NewTask`

Body of `POST /v1/tasks`.

| Field | Type | Required | Content |
| --- | --- | --- | --- |
| `title` | string | yes | Not blank. |
| `kind` | string | yes | `story`, `bug`, `chore`, `research`, `investigation`, `review`, `support`, `content`, `admin`, `interview`, `learning`. |
| `format` | string | no | `content` only: `talk`, `article`, `training`, `workshop`. |
| `context` | string | yes | A context with a task store. |
| `status` | string | yes | Initial status, usually `inbox`. |
| `waiting_on`, `waiting_until` | string, date | no | `waiting` only. |
| `importance` | string | no | `p0` to `p3`; default by kind: `p1` for bug, investigation, support, review; `p3` for chore, research, learning; `p2` otherwise. |
| `due` | date | no | Due date. |
| `pinned` | boolean | no | Default `false`. |
| `links`, `relations` | list, object | no | As in the [task file](task-file.md). |

The ID is drawn by `loomd`, free across every context.

### `TaskEdit`

Body of `PATCH /v1/tasks/{id}`: the fields to change, each optional; an absent field is left as it is.

| Field | Type | Content |
| --- | --- | --- |
| `title` | string | Not blank. |
| `kind` | object | `{"kind": …, "format": …}`, as in `NewTask`; the importance is kept. |
| `importance` | string | `p0` to `p3`. |
| `due` | date \| `null` | `null` removes it. |
| `links` | list | Replaces every link. |
| `relations` | object | Replaces every relation. |

The status and the pin are changed by [actions](#task-endpoints); `status` or `pinned` in the body is an `invalid_request`.

### `TaskActionRequest`

Body of `POST /v1/tasks/{id}/actions/{action}`, optional.

| Field | Type | Content |
| --- | --- | --- |
| `on` | string, optional | `waiting` only: what or whom the task waits on. |
| `until` | date, optional | `waiting` only: hidden from the priority list until this date. |
| `text` | string | `log_note` only, required: the note, not blank. |

### Problem details

`details` of the task errors:

| Code | Type | Content |
| --- | --- | --- |
| `ambiguous_prefix` | `AmbiguousPrefix` | `prefix` (normalized), `candidates`: `id`, `context` and `title` (empty for an invalid file) of every task whose ID starts with it, sorted by ID. |
| `read_only` | `TaskReadOnly` | See [`TaskList`](#tasklist). |
| `illegal_transition` | `IllegalTransition` | `from` and `to`, status names. |
| `conflict` | `UndoConflict` | `field`: the field that no longer has the value the undo expects. |

```json
{
  "code": "ambiguous_prefix",
  "message": "`k3` matches 2 tasks",
  "details": {
    "prefix": "k3",
    "candidates": [
      { "id": "k3q7zm2a", "context": "work", "title": "Review the API" },
      { "id": "k3r0a1b2", "context": "home", "title": "Book the train" }
    ]
  }
}
```

### `RegisterRequest`

| Field | Type | Content |
| --- | --- | --- |
| `path` | path | Absolute path of the repository or of a directory inside it. `.` components and a trailing `/` are ignored; `..` is rejected. |
| `on_use` | boolean, optional | `true`: registration on use (see [repositories](#repositories)). Default `false`, and omitted when `false`, so that a daemon older than this field still reads the request. |

### `ScanResult`

| Field | Type | Content |
| --- | --- | --- |
| `added` | integer (u64) | Repositories added to the index. |
| `updated` | integer (u64) | Repositories whose identity changed (remotes, context, forge or slug). |
| `removed` | integer (u64) | Repositories removed from the index. |
| `errors` | list of objects | Paths that could not be read: `path`, `message`. The repositories they hide are kept unchanged. A repository whose path is not valid UTF-8 is reported here and not indexed. |
| `duration_ms` | integer (u64) | Duration of the scan, in whole milliseconds. |

### `DaemonDiagnostics`

The environment of `loomd` its git operations depend on, read at each request. No secret value is returned.

| Field | Type | Content |
| --- | --- | --- |
| `ssh_auth_sock` | object | `SSH_AUTH_SOCK` of `loomd`, tagged by `state`: `{"state": "unset"}` (unset or empty), `{"state": "missing", "path": …}` (nothing can be examined at the path), `{"state": "not_a_socket", "path": …}`, `{"state": "socket", "path": …}`. |
| `home` | path \| `null` | `HOME` of `loomd`, which expands `~` in the configuration; `null` when unset or empty. |

```json
{
  "ssh_auth_sock": { "state": "socket", "path": "/run/user/1000/ssh-agent.socket" },
  "home": "/home/me"
}
```

### `RemoteCheckResult`

Answer of [`POST /v1/diagnostics/remote-check`](#remote-check).

| Field | Type | Content |
| --- | --- | --- |
| `remotes` | list of `RemoteCheck` | One per distinct endpoint and scheme of the primary remotes of the index, sorted by scheme (in the order below), then endpoint. Empty when no repository has a remote. |

`RemoteCheck`:

| Field | Type | Content |
| --- | --- | --- |
| `endpoint` | string | `<user>@<host>[:<port>]` for SSH (user `git` when the URL has none; the host lowercase, an SSH alias as written), `<scheme>://<host>[:<port>]` for `https`, `http` and `git` (no user, no password), the URL otherwise. |
| `scheme` | string | `ssh` (`ssh://`, `git+ssh://`, `ssh+git://`, scp-like `[user@]host:path`), `https`, `http`, `git`, `local` (a path or `file://`), `other`. |
| `via` | string | Slug of the repository the check ran in. |
| `path` | path | Its path. |
| `remote` | string | Name of its primary remote. |
| `result` | object | Tagged by `kind`: `reachable`, or `failed` with `problem`, `error` (exit status, `no answer within 20s`, `cannot run git: …`, `repository unreadable: …`) and `stderr_tail` (the last 2,000 bytes at most of git's standard error). |

`problem`, read from git's error output, the first that matches:

| `problem` | When git's error output holds (ASCII case ignored) |
| --- | --- |
| `ssh_auth` | `Permission denied (publickey`, `(password` or `(keyboard-interactive`, `Host key verification failed`, `Too many authentication failures`, `no such identity` |
| `https_auth` | `could not read Username`, `could not read Password`, `terminal prompts disabled`, `Authentication failed for`, `HTTP Basic: Access denied`, `Invalid username or password`, `The requested URL returned error: 401` or `403` |
| `not_found` | `Repository not found`, `could not be found or you don't have permission`, `does not appear to be a git repository`, `The requested URL returned error: 404`, `' not found` |
| `network` | `Could not resolve host`, `Could not resolve hostname`, `Name or service not known`, `Connection timed out`, `Operation timed out`, `Connection refused`, `Network is unreachable`, `No route to host`, `Failed to connect to`, `Connection reset`, `Connection closed by`; also a check stopped after 20 s |
| `other` | Anything else, or git could not run |

```json
{
  "remotes": [
    {
      "endpoint": "git@github.com",
      "scheme": "ssh",
      "via": "personal/loom",
      "path": "/home/me/perso/loom",
      "remote": "origin",
      "result": { "kind": "reachable" }
    },
    {
      "endpoint": "https://gitlab.example",
      "scheme": "https",
      "via": "work/api",
      "path": "/home/me/work/api",
      "remote": "origin",
      "result": {
        "kind": "failed",
        "problem": "https_auth",
        "error": "exit status: 128",
        "stderr_tail": "fatal: could not read Username for 'https://gitlab.example': terminal prompts disabled\n"
      }
    }
  ]
}
```

### `DoctorReport`

Output of `loom repo doctor --json`; checks and their order: [CLI conventions](cli/conventions.md#loom-repo-doctor).

| Field | Type | Content |
| --- | --- | --- |
| `checks` | list of objects | `check` (string: `config`, `roots`, `contexts`, `loomd`, `index`, `unassigned`, `ssh-agent`, `home`, `remote`, `remote <endpoint> (<scheme>, via <slug>)`, `updates`), `status` (`ok`, `warn`, `fail`), `detail` (string), `fix` (string, absent when there is nothing to fix; may hold several lines). |

### `ApiError`

Error returned instead of a response body.

| Field | Type | Required | Content |
| --- | --- | --- | --- |
| `code` | `ErrorCode` | yes | Machine-readable category. Clients branch on it, never on `message`. |
| `message` | string | yes | Human-readable explanation, in English. |
| `details` | any JSON value | no | Structured context; its shape depends on `code` and the endpoint. Omitted when empty. |

```json
{
  "code": "invalid_request",
  "message": "invalid priority",
  "details": { "field": "priority", "expected": "integer" }
}
```

### Error codes

`ErrorCode` is a closed enum serialized in `snake_case`. A new code is an API change.

| Code | Meaning | Retry |
| --- | --- | --- |
| `invalid_request` | The request is malformed: unparsable body, unknown or missing field, invalid value. | No: the same request fails the same way. |
| `not_found` | The addressed resource or endpoint does not exist. | No. |
| `version_mismatch` | The client asked for an API version the daemon does not serve. | No: upgrade the client or the daemon. |
| `unavailable` | The daemon cannot serve the request right now: starting, shutting down, or a dependency is down. | Yes, later. |
| `internal` | The daemon failed while handling a valid request: a bug or an unexpected I/O failure. | Not expected to help. |
| `ambiguous_prefix` | A task ID prefix matches several tasks; `details` lists them. | With a longer prefix. |
| `read_only` | The task cannot be changed: unknown kind, format or status, or an invalid `task.md`. | No: fix the file, or upgrade loom. |
| `illegal_transition` | The task cannot move from its status to the requested one. | No. |
| `conflict` | An undo found a field changed since the change it reverts. | After the hand edit is recorded, the undo reverts it instead. |
| `nothing_to_undo` | The task has no change to undo. | No. |
