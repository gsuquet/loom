# API reference

> This page gives the transport and the endpoints of the loomd API, as shipped. It also gives the types of the `loom-api` crate, which the loomd API and the CLI `--json` output share.

## Transport

| Property | Value |
| --- | --- |
| Protocol | HTTP/1.1, JSON bodies. |
| Socket | Unix socket `<loom home>/run/loomd.sock`, mode `0600` (see [configuration](configuration.md)). |
| Authentication | None: the modes of the socket and of `run/` limit access to the user that runs `loomd`. |
| Response `content-type` | `application/json`, for successes and errors. |

The API accepts `HEAD` on each path where it accepts `GET`.

## Endpoints

| Method | Path | Success | Body |
| --- | --- | --- | --- |
| `GET` | `/v1/status` | `200` | [`StatusResponse`](#statusresponse) |
| `GET` | `/v1/repos` | `200` | [`RepoList`](#repolist) |
| `GET` | `/v1/repos/{context}/{name}` | `200` | [`RepoSummary`](#reposummary); with `?refresh=true`, after a new read of its state |
| `DELETE` | `/v1/repos/{context}/{name}` | `200` | [`RepoSummary`](#reposummary) of the removed repository |
| `POST` | `/v1/repos/scan` | `200` | [`ScanResult`](#scanresult); with `?rebuild=true`, of a rebuild; with `?dry_run=true`, nothing applied |
| `POST` | `/v1/repos/register` | `200` | [`RepoSummary`](#reposummary) |
| `POST` | `/v1/repos/sync` | `200` | [`SyncResult`](#syncresult) |
| `GET` | `/v1/search` | `200` | [`SearchResults`](#searchresults) |
| `GET` | `/v1/diagnostics` | `200` | [`DaemonDiagnostics`](#daemondiagnostics) |
| `POST` | `/v1/diagnostics/remote-check` | `200` | [`RemoteCheckResult`](#remotecheckresult) |
| `POST` | `/v1/diagnostics/forge-check` | `200` | [`ForgeCheckResult`](#forgecheckresult) |
| `POST` | `/v1/tasks/reindex` | `200` | [`ReindexResult`](#reindexresult) |
| `GET` | `/v1/tasks` | `200` | [`TaskList`](#tasklist) |
| `POST` | `/v1/tasks` | `200` | [`TaskDetail`](#taskdetail) of the new task; takes a [`NewTask`](#newtask) |
| `GET` | `/v1/tasks/{id}` | `200` | [`TaskDetail`](#taskdetail) |
| `PATCH` | `/v1/tasks/{id}` | `200` | [`TaskDetail`](#taskdetail) after the edit; takes a [`TaskEdit`](#taskedit) |
| `POST` | `/v1/tasks/{id}/actions/{action}` | `200` | [`TaskDetail`](#taskdetail) after the action; takes a [`TaskActionRequest`](#taskactionrequest) or no body |
| `GET` | `/v1/tasks/{id}/events` | `200` | [`TaskEvents`](#taskevents) |
| `POST` | `/v1/tasks/{id}/undo` | `200` | [`TaskDetail`](#taskdetail) after the undo; no body |
| `POST` | `/v1/tasks/{id}/move` | `200` | [`TaskDetail`](#taskdetail) after the move; takes a [`TaskMoveRequest`](#taskmoverequest) |
| `POST` | `/v1/tasks/{id}/mr` | `200` | [`MrOpenResult`](#mropenresult); takes a [`MrOpenRequest`](#mropenrequest) |
| `GET` | `/v1/tasks/backup` | `200` | [`BackupReport`](#backupreport) |
| `POST` | `/v1/tasks/sync` | `200` | [`BackupReport`](#backupreport) |
| `GET` | `/v1/tasks/health` | `200` | [`TaskStoreHealth`](#taskstorehealth) |
| `GET` | `/v1/events` | `200` | `text/event-stream` of [`ChangeEvent`](#changeevent) objects ([change events](#change-events)) |

`loomd` computes `uptime_secs` when it handles the request. At the same time, it reads `<loom home>/state/update.json` for `update`. If `loomd` cannot read the state file, it logs the error and reports an empty state. `disk` is the last measure of the loom home while that measure is fresh, otherwise a new measure ([cache](#diskusage)).

### Repositories

The repository index is `<loom home>/index.db` (see [configuration](configuration.md#indexdb)). `loomd` scans when it starts and on `POST /v1/repos/scan`. Reads never wait for a scan. A read returns the index as it is before or after a scan, never a partly changed index.

`GET /v1/repos` takes these query parameters, both optional. `loomd` ignores an empty value. Any other parameter is an `invalid_request`:

| Parameter | Content |
| --- | --- |
| `context` | Only the repositories of this context. A context name (the rule of `-c`), or `unassigned`. |
| `query` | Only the repositories whose name, slug, path, remote URLs or context contain this text, ASCII case ignored. Plain substring: `%` and `_` match themselves. |

`loomd` sorts the repositories by slug.

`/v1/repos/{context}/{name}` addresses a repository by its slug `<context>/<name>`. The two parts of the slug are two path segments (`/v1/repos/personal/loom-2` for the slug `personal/loom-2`). A client must percent-encode the characters outside the URL path syntax.

`GET /v1/repos/{context}/{name}` takes this query parameter, which is optional. Any other parameter is an `invalid_request`:

| Parameter | Content |
| --- | --- |
| `refresh` | `true`: `loomd` reads the repository's [state](#repostate) now (within its time budget, on a thread of its own), stores it, and returns the repository with it. `false` (default): `loomd` returns the stored state. |

`POST /v1/repos/scan` walks the roots (`[repos] roots`, then every `match.paths`, `~` expanded with `HOME`). It also reads every registered repository in place. Then it applies the changes to the index in one transaction:

- The scan adds a repository that it finds, or updates it when its identity changed (remotes, context, forge or slug).
- The scan removes a repository that it no longer finds under a scanned root. If an error hides the repository, the scan keeps it. Such an error is an unreadable directory above the repository, or an unreadable `.git` or config. The scan keeps a repository that is not under a scanned root.
- The scan keeps a registered repository while its `.git` exists, also when it is deeper than `max_depth`, excluded, or outside the roots. When its `.git` is gone, the scan removes it.
- The scan computes the contexts of every repository again, so the slugs follow a rule change.

`POST /v1/repos/scan` takes these query parameters, both optional. Each one is `true` or `false` (default). Any other parameter is an `invalid_request`:

| Parameter | Content |
| --- | --- |
| `rebuild` | `true`: the scan replaces the index, in one transaction, with the repositories that the roots hold. The scan does not read the registered repositories. It drops every repository, with its state, its `registered` flag and its `last_used`. The result counts every repository of the index as removed, and every repository found as added (`updated` is `0`). |
| `dry_run` | `true`: the scan computes and returns the changes. It applies nothing and reads no state. |

Scans run one at a time. A plain scan request has neither parameter `true`. If a plain scan request arrives while a plain scan runs, the request waits for that scan and receives its result. A rebuild or a dry run runs after that scan, on its own. Registration and removal requests wait for the scan that runs.

`POST /v1/repos/register` takes a [`RegisterRequest`](#registerrequest). The repository is the path itself, or its nearest ancestor with a `.git` entry (directory, file or symbolic link). `loomd` keeps the symbolic links in the path. It identifies the repository like a scanned repository, adds or updates it, and marks it `registered`. A repository already indexed keeps its slug while its context and name do not change. Otherwise, it gets `<context>/<name>`, or the lowest free `<context>/<name>-N` (N ≥ 2).

`on_use: true` is a registration on use, which `loom` sends. With `on_use: true`, if the repository is already in the index, `loomd` only sets its `last_used` to now. It does not read the repository again and does not mark it `registered`. `loomd` looks for the repository by its path. If no repository of the index has this path, `loomd` looks for one with the same directory name and the same canonical path (the same directory through other symbolic links). A repository not in the index is registered as above, then gets its `last_used`.

`DELETE /v1/repos/{context}/{name}` removes the repository from the index, registered or not. A scan adds a repository found under a root again.

#### State refresh

`loomd` reads the [state](#repostate) of a repository with `gix`, and never starts git. It stores the state in the index. It reads the state at these times:

- After each scan, for the repositories that the scan added or updated.
- After the scan on start, then every `[repos] fetch_interval` (default 1 hour), for every repository.
- After a registration, for the registered repository.
- After each [update](#updates) that ran git, for that repository. `loomd` stores the state with the result of the update.
- On `GET /v1/repos/{context}/{name}?refresh=true`, for that repository, before the answer.

Except on `?refresh=true`, `loomd` reads the states in the background. It uses at most 4 threads (fewer on a machine with fewer cores), and each thread reads one repository at a time. Each repository is in the queue at most once at a time. Reads of the index never wait for these threads. A repository without a state yet has `state: null`.

#### Updates

`loomd` updates every repository of the index on a schedule. An update fetches the primary remote, then fast-forwards under the conditions of the update policy of the repository ([how updates work](../explanation/repo-updates.md)). Each update stores its result as the repository's [`last_update`](#lastupdate), and the state that `loomd` reads after it.

| Property | Value |
| --- | --- |
| First update | At once for a repository never updated (within a second of the scan that indexed it); otherwise one interval after its `last_update`. |
| Interval | `[repos] fetch_interval` (default 1 hour) after the end of the previous update, ±10 % at random. `loomd` reads the index again every 10 seconds to find new repositories. |
| Concurrency | At most `[repos] fetch_concurrency` updates at once (default 4, at most 64). |
| Backoff | After a failed fetch, the next update of the repository waits 2, 4, then 8 times the interval (never more). The factor doubles with each consecutive failure. The host of a repository is the `host[:port]` of its primary remote, or its URL for a local path. Every repository of the same host waits as long, unless a fetch of that host that started earlier succeeded in the meantime. A successful fetch clears the host's backoff. Other hosts are not affected. A fetch that found a lock of another git process (`busy`) does not cause a backoff and does not count as a failure. The next update is then one interval later. |
| Policy | `[repos."<path>"] update`, otherwise the context's `update`, otherwise `ff`; `fetch` for an `unassigned` repository without `[repos."<path>"] update`. |
| git commands | `git fetch --no-prune --no-prune-tags --no-tags <primary>` (timeout 120 s; `loomd` fetches no tag, whatever `fetch.pruneTags`, `remote.<name>.pruneTags` or `remote.<name>.tagOpt` say); `git merge --ff-only --no-autostash <commit>` (10 min); `git fetch --no-prune --no-prune-tags --no-tags <primary> <merge ref>:refs/heads/<default branch>` (120 s). At the timeout, `loomd` sends `SIGTERM` to the process group of the command, then `SIGKILL` 5 s later. If a command fails only because another git process holds a lock of the repository (`Unable to create '….lock': File exists`, `cannot lock ref '…': is at … but expected …`), the skip reason is `busy`. `loomd` never removes the lock. |
| git environment | `loomd`'s environment and the user's git configuration and hooks, plus `GIT_TERMINAL_PROMPT=0`, `GIT_CEILING_DIRECTORIES=<parent of the working tree>`, `GIT_SSH_COMMAND="ssh -o BatchMode=yes"` unless `core.sshCommand` (read with `git config --get`), `GIT_SSH_COMMAND` or `GIT_SSH` is set; `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_COMMON_DIR`, `GIT_NAMESPACE` and `GIT_PREFIX` removed. |

`POST /v1/repos/sync` takes a [`SyncRequest`](#syncrequest). It updates the repositories that the request names now, regardless of their schedule and of the backoff. If the request names no repository, it updates every repository of the index. After `loomd` stores every result, it answers with a [`SyncResult`](#syncresult), sorted by slug.

If an update of a repository is in progress, `loomd` updates the repository again when that update ends. The answer can take as long as the updates (each command has its timeout). With `dry_run: true`, nothing runs: `loomd` reads each state now, and the result says what an update would do.

#### Remote check

`POST /v1/diagnostics/remote-check` (no body) checks that each remote endpoint of the index responds to the git of `loomd`. It answers with a [`RemoteCheckResult`](#remotecheckresult) (the checks of `loom repo doctor`).

| Property | Value |
| --- | --- |
| Endpoints | One per distinct endpoint and scheme of the primary remotes of the index (see [`RemoteCheck`](#remotecheckresult)). |
| Repository | One repository of the endpoint: one whose last update did not fail to fetch when there is one, then the lowest slug. |
| git command | `git -C <repository> ls-remote --exit-code <primary remote> HEAD`, with the [git environment](#updates) of the updates: the user's git configuration (`core.sshCommand`, `includeIf`, credential helpers), `GIT_TERMINAL_PROMPT=0`, SSH `BatchMode` unless an SSH command is configured. The check writes nothing to the repository. |
| Timing | Every endpoint at once, each stopped after 20 s (process group `SIGTERM`, then `SIGKILL` 5 s later). |
| Result | `reachable` when git exits with `0`, or with `2` (no `HEAD` on the remote, such as an empty repository); otherwise `failed` with the kind of failure read from git's error output. |

#### Forge check

`POST /v1/diagnostics/forge-check` (no body) checks the forge credential of each context held on this machine, for each forge host ([forges](configuration.md#forges)). It answers with a [`ForgeCheckResult`](#forgecheckresult) (the `forge` checks of `loom repo doctor`).

| Property | Value |
| --- | --- |
| Credentials | `[contexts.<ctx>.forges."<host>"]` of the context, otherwise `[forges."<host>"]`. A context without a credential for a host is not checked for it. |
| Token | `token_cmd` runs through `sh -c` in the environment of `loomd`, with no input, stopped after 10 s (process group `SIGTERM`, then `SIGKILL` 1 s later). Its standard output, trimmed, is the token. `token_env` reads the variable from the environment of `loomd`, trimmed. `loomd` keeps the token in memory, per source: checks of the same source run its command once. `loomd` gets the token again after a `401`. |
| Forge calls | `GET <api>/user` with `Authorization: Bearer <token>`. On a `401`, `loomd` gets the token again and calls once more. GitLab: then `GET <api>/personal_access_tokens/self` for the scopes. Each call stops after 20 s. The header follows a redirect only to the same host over HTTPS. |
| API | `https://api.github.com` for `github.com`, `https://<host>/api/v3` for another GitHub host (GitHub Enterprise Server), `https://<host>/api/v4` for GitLab. |
| Timing | Every credential at once. |
| Secrets | The answer, the logs and the traces never contain the token. The answer and the traces contain the program of `token_cmd` (its first word), not its other text. When `token_cmd` fails, `error` contains the end of its standard error. |

### Search

`GET /v1/search` searches the tasks of the context stores and the repositories of the index. It takes these query parameters. `loomd` ignores an empty `context` or `type`. Any other parameter is an `invalid_request`:

| Parameter | Content |
| --- | --- |
| `q` | Required. The query: words separated by whitespace. |
| `context` | Only the results of this context. A context name (the rule of `-c`), or `unassigned` (repositories only). |
| `type` | Only the results of this kind: `task` or `repo`. |
| `limit` | Most results returned, at least `1`. Default `20`. |

The rules of the match:

- The searched fields of a task are its title, its notes and the keys of its origin and related links (`CORE-1042`, `!88`). When its `task.md` is invalid, the notes are the whole file. The searched fields of a repository are its name, slug, path, remote URLs, context name and branch. The branch is the branch that `HEAD` is on in its stored [state](#repostate), also when it is unborn. The branch is empty when `HEAD` is detached or the state is not read yet.
- Each character that is not a letter or a digit splits a field into words. For example, `loom-api` is `loom` and `api`, `CORE-1042` is `core` and `1042`, and `!88` is `88`. The search ignores case and diacritics: `cafe` finds `Café`, `viet` finds `Việt`.
- A result matches when every word of the query starts a word of one of its fields, not necessarily the same field. The search ignores a query word that has only punctuation. A query without words returns no result.
- Query words are text only. The characters `"`, `*`, `:`, `(`, `)`, `^`, `-`, `+`, `{`, `}` are word separators. `AND`, `OR`, `NOT` and `NEAR` are words to find.

Results are ranked by group first:

1. Tasks `active`, `ready` and `waiting`.
2. Repositories.
3. Tasks `inbox`, tasks whose status was written by a newer loom, and tasks whose `task.md` is invalid.
4. Tasks `done` and `cancelled`.

Within a group, the search ranks the tasks by these keys, in order:

1. How the task matches, in this order, best first:
   1. The query is one of its link keys, case ignored (such as `core-1042` for `CORE-1042`).
   2. Every query word starts a word of its title.
   3. Other cases.
2. Last change, most recent first, invalid files last.
3. Full-text relevance (SQLite FTS5 `bm25`), most relevant first.
4. ID.

The search ranks the repositories by these keys, in order:

1. How the repository matches, in this order, best first:
   1. Its name is the query: the same words, in order (such as `loom-api` for `loom api`).
   2. Every query word starts a word of its name.
   3. Every query word starts a word of its slug.
   4. Other cases.
2. `last_used`, most recent first, never used last.
3. Full-text relevance (SQLite FTS5 `bm25`), most relevant first.
4. Slug.

`loomd` ranks every match, then keeps the first `limit` results. It reads the repositories and the tasks in one snapshot of the index. After `loomd` starts, it reconciles the tasks with the index. During this reconciliation, a search that covers tasks waits for its end. The search waits at most 10 s, then answers from the tasks indexed so far.

### Tasks

The tasks are files in the context stores (see [configuration](configuration.md#context-stores)). `loomd` indexes them in `<loom home>/index.db`. It reconciles them with the index when it starts and when their files change. The reconciliation records the hand edits (see [context stores](configuration.md#context-stores)).

`POST /v1/tasks/reindex` (no body) rebuilds the tasks index from the files of every context store, in one transaction. When `loomd` has written the index, it answers with a [`ReindexResult`](#reindexresult). The rebuild drops the rows of tasks whose directory is gone. It indexes an invalid `task.md` as read-only and lists it, but never writes it again. An ID that the stores of several contexts hold gets one row only: an invalid row, listed once, whose error names the contexts. The rebuild does not choose a copy. The rebuild does not change the repositories of the index.

Before the rebuild, `loomd` reconciles every task: a hand edit not recorded yet gets its `edited` event first. Thus a reindex can append events, and the rebuild never hides an edit. The reconciliation or the rebuild can fail to read the files of a task (permissions, I/O), or fail to list the `tasks/` of a store. `failed` lists such a task or store, and its rows stay as they were. `loomd` indexes the other tasks. Only a failure to write the index answers `500 internal`, and the index then stays unchanged.

#### Task endpoints

`{id}` is a task ID or a prefix of one, from 1 to 8 characters. `loomd` reads it as Crockford base32: case ignored, `i` and `l` read as `1`, `o` as `0`. `loomd` resolves it against every indexed task, of every context. If several tasks start with the prefix, the error is `ambiguous_prefix`, and the error lists these tasks. `/v1/tasks/reindex` is the reindex endpoint, never a task.

Reads (`GET /v1/tasks`, `GET /v1/tasks/{id}`) use a read connection to the index and never wait for a write. After `loomd` starts, a read waits for the reconciliation on start and for the first [archive](configuration.md#archive) pass. The resolution of `{id}` by every endpoint also waits for them. The wait is at most 10 s, then `loomd` answers from the tasks indexed so far. Thus a client should allow more than 10 s for its first request.

Creations, edits, actions, undos and event reads run on the task thread, one at a time. They run after the hand edits and the reindexes queued before them. Thus two requests on one task never interleave, and no request is lost. Their events have the actor `user` and the `id` of [`machine.toml`](configuration.md#machinetoml). A request that changes nothing writes nothing and answers the task as it is. Examples are a pin of a pinned task, and an edit of a field to its value.

`loomd` commits each write to the git repository of its context store before the answer, one commit per request ([store repository](configuration.md#store-repository)). If a commit fails, the request does not fail.

`GET /v1/tasks` takes these query parameters, all optional. `loomd` ignores an empty `context`, `status` or `parent`. Any other parameter is an `invalid_request`:

| Parameter | Content |
| --- | --- |
| `context` | Only the tasks of this context. A context without a store gives an empty list. |
| `status` | Only the tasks with this status: `inbox`, `ready`, `active`, `waiting`, `done`, `cancelled`. Tasks with an invalid `task.md` or an unknown status are not in the list. |
| `ranked` | `true`: the priority list, in its order, each task with its `rank`. `false` (default): every task, sorted by ID. |
| `archived` | `true`: only the archived tasks (in `archive/<yyyy>/`). `false` (default): only the tasks of `tasks/`. `any`: both. The priority list never contains an archived task: `archived=true` with `ranked=true` gives an empty list, and `archived=any` with `ranked=true` gives the priority list of the tasks of `tasks/`. |
| `parent` | Only the tasks whose `relations.parent` is one of these tasks. The value is one full task ID, or several separated by commas; a prefix or any other text is an `invalid_request`. A parent that does not exist keeps no task: the list is empty, not an error. `loomd` ignores an empty value. Tasks with an invalid `task.md` are not in the list. |

The priority list holds the `ready` and `active` tasks, and the `waiting` tasks whose `until` is today or past. Tasks with the status `inbox`, `done`, `cancelled` or an unknown status, and invalid files, are never in it. The list has this order, and each rule breaks the ties of the previous rule:

1. Pinned first.
2. Overdue or due today first, earliest due date first.
3. Importance, `p0` first.
4. Due date, earliest first, none last.
5. Oldest `created` first.
6. ID.

"Today" is the local date of `loomd` (`TZ`, otherwise `/etc/localtime`), read at each request. With `context` or `status`, the list is the priority list of the tasks that these parameters keep.

`{action}` of `POST /v1/tasks/{id}/actions/{action}`:

| Action | Body | Change |
| --- | --- | --- |
| `inbox`, `ready`, `active`, `done`, `cancelled` | none | Moves the task to this status. |
| `accept` | `kind`, `importance`, `due`, all optional | Moves an `inbox` task to `ready` and updates the specified fields. |
| `waiting` | `on`, `until`, both optional | Moves the task to `waiting` (or changes what it waits on and until when). |
| `pin`, `unpin` | none | Pins or unpins the task. |
| `log_note` | `text`, required | Logs a note as a `note_logged` event. |

A task with an open status (`inbox`, `ready`, `active`, `waiting`) can move to any other status, and from `waiting` to `waiting` too. A task `done` or `cancelled` can move only to `ready`. The `accept` action requires an `inbox` task and moves it to `ready` while updating any specified `kind`, `importance`, or `due` fields in a single event. If `kind` changes without `importance`, `importance` resets to the default of the new kind; if `kind` is unchanged, existing importance is kept. An `accept` call on a task not in `inbox` returns `400 illegal_transition`. Any other action name is a `not_found`. An empty body (or a body of only whitespace) is no body. A field that the action does not take is an `invalid_request`.

`POST /v1/tasks/{id}/undo` reverts the last change of the task. It appends an `undone` event with the inverse changes. A change is an edit, a status change, a pin, a hand edit or an undo, but not a note or the creation. The undo of an undo does the change again. `loomd` computes the inverse from the log of the task. The request has no body. A move is not undone: if the last change is a move, the request fails with `conflict` and the message names the context to move back to.

`POST /v1/tasks/{id}/move` moves the task to the store of another context. The task keeps its ID, its files and its events. `loomd` appends an `edited` event with a `context` field change, after any hand edit that was not recorded yet, and answers the task in its new context. It copies the task directory to the target store, then removes it from the source store. If the removal fails, `loomd` removes the copy and answers `500` with `internal`: the message says that the task stays in its context. If that rollback fails too, the message names the two contexts that hold the task. Then the task is a duplicate ID, read-only, until one copy is removed by hand. A move to the context the task is in is refused with `400`, so a repeated move is not idempotent: compare the context first. The move needs a store for the target context on this machine. A task with a parent, or with children (archived children too), is not moved. Archived tasks and read-only tasks are not moved. `loomd` commits the target store (`edited(<id>): context`) and the source store (`moved(<id>): moved to <context>`).

`POST /v1/tasks/{id}/mr` (`loom pr`) opens a draft MR for a branch that the client pushed, and links it to the task. `loomd` uses the forge credential of the context of the task for `host` ([forges](configuration.md#forges)). It does not run git. It sends these requests:

1. The open MRs of the branch: GitHub `GET /repos/<project>/pulls?head=<owner>:<head>&state=open`, GitLab `GET /projects/<project>/merge_requests?source_branch=<head>&state=opened` (the project path percent-encoded).
2. Only when the branch has no open MR: GitHub `POST /repos/<project>/pulls` with `title`, `head`, `base`, `body` and `draft: true`; GitLab `POST /projects/<project>/merge_requests` with `source_branch`, `target_branch`, `description` and the title prefixed with `Draft: `.

The title is the title of the task. The description is the body template of the kind of the task ([task file](task-file.md)), then a `## Links` section with the `origin` and `related` links of the task. The notes of the task are never sent. Open MRs from another project (a fork) with the same branch name are ignored. If the branch has open MRs, `loomd` links the first one and opens none. Then `loomd` appends to the log of the task an `edited` event that adds the MR as a `delivery` link, and an `mr` event. If the task has the link already, an MR found again records no event. With `dry_run: true`, `loomd` sends nothing and changes nothing: the result lists the two requests, with `outcome` `planned`. The dry run checks that the task can change and that the context has a credential for `host`; it does not resolve the token. A `401` resolves the token again, once. A refusal of the forge is a `502 forge` error, with the validation errors of the forge in the message.

`GET /v1/tasks/{id}/events` answers the event log of the task as `events.jsonl` holds it, with the lines this version cannot replay listed apart.

`GET /v1/tasks/backup` answers with the backup state of every context store held on this machine ([`BackupReport`](#backupreport)). `loomd` does not run git for this answer. The backup state is the last commit failure and the last sync of each store since loomd started.

`POST /v1/tasks/sync` with a [`BackupRequest`](#backuprequest) body syncs the stores ([store backup](configuration.md#store-backup)). When every sync ended, it answers with the stores synced. The answer is `400 invalid_request` when `context` names a context without `store.backup.remote`, a context not held on this machine, or an unknown context. The answer is also `400 invalid_request` when the body is invalid. A sync that fails is in the answer, not an error status.

### Change events

`GET /v1/events` sends a Server-Sent Events stream (`text/event-stream`, chunked). The stream stays open. A client reads the changes from the stream, then gets only the data that changed. It does not poll.

`loomd` sends an event after it writes the change to the task files and to the index. Before the event, `loomd` also tries to commit the change to the git repository of the store. If the commit fails, `loomd` sends the event and tries the commit again later.

```text
id: 1792051200000-7
event: task.changed
data: {"type":"task.changed","id":"k3q7zm2a","context":"work"}

```

| Field | Content |
| --- | --- |
| `id` | `<run>-<seq>`. `run` is the start time of the `loomd` process, in milliseconds since the Unix epoch. `seq` is the number of the event in the process, from 1. |
| `event` | The `type` of the event. |
| `data` | The [`ChangeEvent`](#changeevent) as JSON, with its `type`. |

Events that `loomd` sends:

| `type` | When |
| --- | --- |
| `task.changed` | An API request creates, edits, moves, pins, unpins or undoes a task, and writes a [task event](#taskevents). A request that changes nothing sends no change event. Also when `loomd` reconciles a hand edit of the task files: a new task, a changed task, or a removed task directory. Also when a [schedule](configuration.md#schedules) creates a task, and when the `replace` policy cancels the task of an earlier occurrence. |
| `task.archived` | The [archive](configuration.md#archive) pass moves a closed task to `archive/<yyyy>/`. `loomd` sends the event after the move, the index write and the commit attempt. A directory that you move by hand sends `task.changed`. |
| `resync` | `loom reindex` builds the index again. A reconciliation of all the tasks changes one or more tasks (the watcher can miss events). A sync moves a context store forward to the commits of another machine. Also the first event to a client that cannot resume, and the last event to a client that is too far behind (see below). |

`repo.changed` and `counts.changed` are in the schema. `loomd` does not send them yet.

Query parameter `context`: the stream has only the events of this context, and all the `resync` events. An invalid name or an unknown parameter gives `400 invalid_request`.

Resume:

- A client can connect again with the `Last-Event-ID` header. The header contains the `id` of the last event that the client received.
- If the buffer of the last 1,024 events of the process has the next events, the client gets them.
- If the buffer does not have them, the first event is `resync`. The same occurs if the `id` is from another `loomd` process (a restart), or if it is not an `id`. This `resync` has the `id` of the latest event. The client then gets all the data that it shows again.

A client can be a maximum of 256 events behind. If it is more than 256 events behind, it gets a `resync` event and its stream stops. This `resync` has no `id`, so that the client can resume from its last `id`.

While a stream has no events, it gets a `: heartbeat` comment every 15 s. When `loomd` stops, all the streams stop.

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
| Task: body not a valid `NewTask`, `TaskEdit`, `TaskActionRequest` or `TaskMoveRequest` (unknown field, absent `text` or `context`, field the action does not take); move to the context the task is in, to a context without a store on this machine, of a task with a parent or with children; empty title or note, unknown kind, format or status, no store for the `context` of a new task, invalid `status`, `ranked` or `archived`; `relations.parent` of a create or an edit that names no indexed task, a task of another context, the task itself or one of its descendants | `400` | `invalid_request` |
| Task: unknown `{action}` | `404` | `not_found` |
| Task: change of an archived task (any action, edit, undo or MR, also with `dry_run`), or of a task whose kind, format or status is unknown, or whose `task.md` is invalid | `409` | `read_only`, `details`: [`TaskReadOnly`](#problem-details) |
| Task: status change the transitions forbid | `409` | `illegal_transition`, `details`: [`IllegalTransition`](#problem-details) |
| Task: undo of a change whose field changed since (a hand edit not recorded yet) | `409` | `conflict`, `details`: [`UndoConflict`](#problem-details) |
| Task: undo of a task without a change | `409` | `nothing_to_undo` |
| MR: body not a valid `MrOpenRequest`, an empty field, `head` equal to `base`, a `project` with fewer than 2 segments or a `.` or `..` segment, or no forge credential of the context of the task for `host` | `400` | `invalid_request` |
| MR: the token cannot be resolved, the forge refuses a request or does not answer | `502` | `forge` |
| Task: undo of a move | `409` | `conflict`, `details`: [`UndoConflict`](#problem-details) with `field` `context` |
| Task: a move failed and was rolled back, or failed halfway and left the task in two stores | `500` | `internal` |
| Task: the task files cannot be read or written | `500` | `internal` |
| The index cannot be read or written | `500` | `internal` |
| The daemon cannot read its clock as a timestamp | `500` | `internal` |
| The daemon stops before it answers a scan, register, remove, refresh, sync, reindex or task write request | `503` | `unavailable` |

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
curl --unix-socket ~/.loom/run/loomd.sock -X POST http://loomd/v1/diagnostics/forge-check
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

The JSON Schema (draft 2020-12) of each type is generated from the Rust types and committed under [`schemas/`](schemas/). If a committed schema is different from the types, a test fails. `mise run docs` regenerates the schemas.

| Type | Schema |
| --- | --- |
| `AmbiguousPrefix` | [`schemas/ambiguous_prefix.json`](schemas/ambiguous_prefix.json) (`details` of `ambiguous_prefix`) |
| `ApiError` | [`schemas/api_error.json`](schemas/api_error.json) |
| `ApiVersion` | [`schemas/api_version.json`](schemas/api_version.json) |
| `ContextAssignment` | [`schemas/context_assignment.json`](schemas/context_assignment.json) (in `RepoSummary`) |
| `DaemonDiagnostics` | [`schemas/daemon_diagnostics.json`](schemas/daemon_diagnostics.json) |
| `DoctorReport` | [`schemas/doctor_report.json`](schemas/doctor_report.json) (`loom repo doctor --json`, no endpoint) |
| `ErrorCode` | [`schemas/error_code.json`](schemas/error_code.json) |
| `ForgeCheckResult` | [`schemas/forge_check_result.json`](schemas/forge_check_result.json) |
| `IllegalTransition` | [`schemas/illegal_transition.json`](schemas/illegal_transition.json) (`details` of `illegal_transition`) |
| `LastUpdate` | [`schemas/last_update.json`](schemas/last_update.json) (in `RepoSummary` and `SyncResult`) |
| `NewTask` | [`schemas/new_task.json`](schemas/new_task.json) |
| `RegisterRequest` | [`schemas/register_request.json`](schemas/register_request.json) |
| `ReindexResult` | [`schemas/reindex_result.json`](schemas/reindex_result.json) |
| `BackupReport` | [`schemas/backup_report.json`](schemas/backup_report.json) |
| `TaskStoreHealth` | [`schemas/task_store_health.json`](schemas/task_store_health.json) |
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
| `TaskMoveRequest` | [`schemas/task_move_request.json`](schemas/task_move_request.json) |
| `UpdateState` | [`schemas/update_state.json`](schemas/update_state.json) (file `state/update.json`, see [configuration](configuration.md#state)) |
| `VersionInfo` | [`schemas/version_info.json`](schemas/version_info.json) |

### API versioning

`ApiVersion` is a closed enum serialized as a string.

| Value | Meaning |
| --- | --- |
| `v1` | First version of the API. Current version. |

A change to any API type that breaks compatibility adds a new value. Compatible changes (new optional fields, new endpoints) keep the current value.

### Unknown fields

| Message | Unknown fields |
| --- | --- |
| Request | Rejected (`#[serde(deny_unknown_fields)]`, or key by key for `NewTask` and `TaskEdit`): `RegisterRequest`, `NewTask`, `TaskEdit`, `TaskActionRequest`, and the query parameters of `GET /v1/repos`, `GET /v1/repos/{context}/{name}`, `POST /v1/repos/scan` and `GET /v1/tasks`. |
| Response | Ignored, so that an older client can read a newer daemon. |

Unknown values of these enums are rejected: `api_version`, `code`, `channel`, `channel_source`, `profile`, `policy`, `kind`, `operation`, `state`, `status`. The exception is the `kind`, `format` and `status` of a task, which a newer version can write. The task is then read-only.

### Scalar encodings

| Kind | Encoding | Example |
| --- | --- | --- |
| Instant (`Timestamp`) | RFC 3339 string in UTC, whole seconds: `YYYY-MM-DDTHH:MM:SSZ`, years 1970 to 9999. Any other form is rejected: fractional seconds, offsets other than `Z`, lowercase `t` or `z`, leap seconds (`:60`). | `"2026-10-12T08:00:00Z"` |
| Duration | Unsigned integer of whole seconds; the field name ends in `_secs`. | `90` |
| Path | String. Paths that are not valid UTF-8 cannot be serialized. | `"/run/user/1000/loom/loomd.sock"` |

`VersionInfo.commit_date` existed before the `Timestamp` type. It is the git committer date with its original offset (`2026-10-12T10:00:00+02:00`), or `unknown`.

### `StatusResponse`

State of a loomd that runs.

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
| `update` | [`UpdateStatus`](#updatestatus) or `null` | Auto-upgrade policy and state. Always sent by this version. Optional for a reader, so that a client can read a daemon older than this field. |
| `disk` | [`DiskUsage`](#diskusage) or `null` | Disk usage of the loom home. Sent by this version (`null` only when the measure failed unexpectedly). Optional for a reader, so that a client can read a daemon older than this field. |

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
| Symbolic links | Counted as links (their own blocks), never followed. The loom home itself can be a symbolic link, which the walk follows once. |
| Hard links | A file with several links counts once. |
| Bounds | Only entries under the loom home are read. Directories mounted under it are walked like any other. |
| Errors | Never an error response. A directory or entry that cannot be read counts as 0 and adds a warning, also logged at `warn` level. An absent `versions/`, `log/`, `state/` or `contexts/` counts as 0 without a warning. An entry deleted during the walk is skipped. |
| Cache | Measured on a blocking thread at the first `GET /v1/status`, then reused for 5 minutes. Measured again earlier when the modification time of `versions/` changed (a version saved, restored or pruned by an upgrade or a rollback). A new `loomd` (after an upgrade, a rollback or a restart) starts without a measure. |

### `UpdateStatus`

| Field | Type | Content |
| --- | --- | --- |
| `policy` | `auto` \| `notify` \| `off` | Effective `[update] policy` (configured, or the default of the build channel). |
| `channel` | `stable` \| `dev` \| `null` | Effective channel: `[update] channel`, otherwise the channel chosen with `loom upgrade --channel`, otherwise the build channel ([channel resolution](configuration.md#channel-resolution)); `null` for a local build without a configured or chosen channel: nothing is checked. |
| `channel_source` | `config` \| `chosen` \| `build` \| `null` | Where `channel` comes from; `null` when `channel` is. Always sent by this version. Optional for a reader, so that a client can read a daemon older than this field. |
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

The state of a repository (its card). `loomd` reads it with `gix`, does not start git, and does not write to the repository.

Every field but `refreshed_at` and `error` is either `{"known": <value>}` or the string `"unknown"`. A field is unknown when its time budget ended or when `loomd` could not read it. `error` then gives the reason. A known `null` (`{"known": null}`) is a value: no operation, no upstream, and so on.

The status walk (`dirty`), the untracked count and the ahead/behind counts share a time budget of 2 seconds per repository. The budget starts at the start of the read. A value not computed in time is unknown, and `loomd` still reads the other fields. The git configuration of the user (`HOME`, `XDG_CONFIG_HOME`, `GIT_CONFIG_*`) and of the system applies (`core.excludesFile`, `status.renames`, `core.abbrev`…). The configuration of the git installation, which only git itself can find, does not apply.

| Field | Known value | Content |
| --- | --- | --- |
| `refreshed_at` | `Timestamp` | When the state was read. |
| `head` | object | What `HEAD` points to, tagged by `kind`: `{"kind": "branch", "name": "main"}`, `{"kind": "unborn", "name": "main"}` (a branch without commits) or `{"kind": "detached", "commit": "abc1234"}` (commit id abbreviated per `core.abbrev`, at least 7 digits). A rebase in progress detaches `HEAD`. |
| `dirty` | integer (u64) | Tracked paths with a change between `HEAD`, the index and the working tree: modified, added, deleted, renamed (one path), type-changed or conflicted, staged or not, each path once. A submodule follows its `ignore` configuration. Untracked files are not counted. `loomd` never runs filter drivers (`filter.<driver>.clean` or `.process`, such as git-lfs). Built-in conversions (end of line, `ident`, `working-tree-encoding`) apply. When a file changed on disk has a `filter` attribute that names a configured driver, its content cannot be compared. Then `dirty` is unknown, and `error` names the driver and the file. |
| `untracked` | object | `count`: untracked files, each counted (an untracked directory counts its files, an untracked nested repository counts as one), `.gitignore`, `.git/info/exclude` and `core.excludesFile` applied, empty directories ignored; at most 1,000. `capped`: `true` when the count stopped at 1,000. |
| `operation` | string \| `null` | Operation in progress, from the markers of the git directory of the working tree: `rebase` (either backend), `merge`, `cherry-pick`, `revert`, `bisect`, `am`. When several markers exist, the first in this order wins: `am`, `rebase`, `cherry-pick`, `merge`, `bisect`, `revert`. |
| `upstream` | object \| `null` | Upstream of the `HEAD` branch: `name` (short ref name, such as `origin/main`), `ahead` and `behind` (commits of `HEAD` not in the upstream and the reverse, each a known integer or `"unknown"`). `null` when `HEAD` is detached, the branch has no upstream configured, or the upstream ref does not exist (never fetched, or pruned). |
| `default_branch` | string \| `null` | The branch `refs/remotes/<primary remote>/HEAD` points to; otherwise `main`, then `master`, when the branch exists locally or on the primary remote; otherwise `null`. |
| `last_commit` | `Timestamp` \| `null` | Committer date of `HEAD`'s commit; `null` on an unborn branch. |
| `last_fetch` | `Timestamp` \| `null` | Modification time of `FETCH_HEAD`, the latest of the working tree's git directory and of the main one (a linked worktree has its own); `null` when neither exists. |
| `error` | string \| `null` | Why fields are unknown, one reason per field, joined with `; `, such as `dirty: time budget of 2s exceeded`. A repository that cannot be opened has every field unknown except `last_fetch`, and the error `cannot open the repository: …`. |

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
| `fetched` | | The primary remote was fetched. The branch of `HEAD` was not behind its upstream, or the policy is `fetch`. |
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
| `busy` | `step` (`fetch`, `fast_forward`, `move_default_branch`), `stderr_tail` | Another git process (an editor's automatic fetch, a command of the user) held a lock of the repository during this step. Either `index.lock` or another `*.lock` file existed, or a ref moved under the fetch. Retried at the next interval, without [backoff](#updates). The lock is never removed. Any other failure, or a lock along with another error, stays `failed`. |

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
| `dry_run` | boolean, optional | `true`: nothing runs, and the result says what an update would do. Default `false`. |

Unknown fields are rejected.

### `SyncResult`

| Field | Type | Content |
| --- | --- | --- |
| `repos` | list of objects | One per repository, sorted by slug: `slug`, and `result`, tagged by `kind`. |

| `kind` | Other fields | Content |
| --- | --- | --- |
| `updated` | `update`: [`LastUpdate`](#lastupdate) | The update ran. The result is also stored as the repository's `last_update`. |
| `planned` | `policy`, `actions` | Dry run. `actions`, in order, each tagged by `kind`: `skip` (with a `reason`, as in `LastUpdate`), `fetch`, `decide_after_fetch` (whether a branch moves depends on what the fetch brings). With policy `ff`, a `skip` replaces `decide_after_fetch` when the working tree already prevents a fast-forward of the branch `HEAD` is on. The causes are an operation, a tracked change, a detached or unborn `HEAD`, or one of these unknown. The default branch is never predicted. |

### `RepoList`

| Field | Type | Content |
| --- | --- | --- |
| `repos` | list of [`RepoSummary`](#reposummary) | The repositories, sorted by slug. |

### `SearchResults`

| Field | Type | Content |
| --- | --- | --- |
| `results` | list of `SearchResult` | The results, best first (see [search](#search)), at most `limit`. |

`SearchResult`: an object whose `kind` says what matched, `repo` or `task`.

A repository (`kind` `repo`, the v0.2 shape):

| Field | Type | Content |
| --- | --- | --- |
| `kind` | string | `repo`. |
| `slug` | string | Slug of the repository. |
| `name` | string | Directory name of the repository. |
| `context` | string | Context name, `unassigned` for a repository without context. |
| `path` | string | Absolute path of the repository. |
| `score` | number | Full-text relevance: SQLite FTS5 `bm25` with its sign flipped, higher is more relevant. Only one of the keys that rank the results: the order of `results` is the final order. |
| `matched_fields` | list of strings | Fields that hold at least one query word: `name`, `slug`, `path`, `remotes`, `context`, `branch`, in this order. |

A task (`kind` `task`):

| Field | Type | Content |
| --- | --- | --- |
| `kind` | string | `task`. |
| `id` | string | ID of the task. |
| `context` | string | Context of its store. |
| `title` | string | Title; empty when its `task.md` is invalid. |
| `status` | string or `null` | Status as written in `task.md`, a status of a newer loom included; `null` when `task.md` is invalid. |
| `link_keys` | list of strings | Keys of the origin and related links, in file order. |
| `score` | number | As for a repository. |
| `matched_fields` | list of strings | Fields that hold at least one query word: `title`, `notes`, `link_keys`, in this order. |

`GET /v1/tasks/health` answers with the problems of the task stores ([`TaskStoreHealth`](#taskstorehealth)). It lists the task files that `loomd` cannot read, as the last scan or change found them. A file that is fixed leaves the list when `loomd` reads it again.

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

### `TaskStoreHealth`

| Field | Type | Meaning |
| --- | --- | --- |
| `watching` | boolean | `true` when `loomd` watches all the task directories. `false` when the watcher did not start or when `unwatched` is not empty: a change made outside `loom` is not seen until the next reindex. |
| `unwatched` | array | The `tasks/` and `archive/` directories that the watcher could not watch (a missing directory, a permission, the system limit of watchers). |
| `unwatched[].path` | string | The directory. |
| `unwatched[].error` | string | What failed. |
| `invalid` | array | The task files that cannot be read, sorted by context then ID. |
| `invalid[].context` | string | The context of its store. |
| `invalid[].id` | string | The task ID, the name of its directory. |
| `invalid[].path` | string | The directory of the task: `<store>/tasks/<id>`, or `<store>/archive/<yyyy>/<id>` for an archived task. Absent from the answer of an older loomd: `loom` reads it as empty and shows `unknown location`. |
| `invalid[].error` | string | Why the file is invalid, with the line when known. |
| `failed` | array | The places that `loomd` could not scan or read. |
| `failed[].path` | string | The task directory, or `tasks/` when the store could not be listed. |
| `failed[].error` | string | What failed. |

### `TaskMoveRequest`

Body of `POST /v1/tasks/{id}/move`. Unknown fields are rejected.

| Field | Type | Meaning |
| --- | --- | --- |
| `context` | string | Required. The context to move the task to. |

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
| `context` | string | Context of the store that holds the task. |
| `id` | string | Task ID, the name of its directory. |
| `path` | path | The directory of the task: `<store>/tasks/<id>`, or `<store>/archive/<yyyy>/<id>` for an archived task. Absent from the answer of an older loomd: `loom` reads it as empty and shows `unknown location`. |
| `error` | string | Why the file cannot be read. It starts with `line <n>: ` when the line is known. Causes: not UTF-8, no `+++` frontmatter, invalid TOML, an absent or invalid key, an `id` that is not the directory name, a `context` that is not the store's, an ID that the stores of several contexts hold (`duplicate ID across contexts`, with their names). |

`FailedTaskFile`:

| Field | Type | Content |
| --- | --- | --- |
| `path` | path | The task directory, or the `tasks/` directory of a store that could not be listed. |
| `error` | string | What failed, with the file when it is an I/O error. |

### `ChangeEvent`

An object. Its `type` field identifies the event ([schema](schemas/change_event.json)):

| `type` | Fields |
| --- | --- |
| `task.changed` | `id` (task ID), `context` |
| `task.archived` | `id`, `context` |
| `repo.changed` | `slug` (`<context>/<name>`) |
| `counts.changed` | `context`, `inbox`, `waiting` (numbers of tasks) |
| `resync` | none |

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
| `context` | string | Context of the store that holds the task. |
| `task` | `Task` \| `null` | The task, as `task.md` holds it, with `updated` set to the time of its last event; `null` when `task.md` is invalid. Fields: `id`, `title`, `kind` and `format`, `context`, `status`, `waiting_on` and `waiting_until`, `importance`, `due`, `pinned`, `links`, `relations`, `created`, `updated` (see the [task file](task-file.md)). |
| `read_only` | `TaskReadOnly`, optional | Why the task cannot be changed; absent when it can. |
| `rank` | object, optional | With `ranked=true` only: `position` (from 1) and `reason`, the rule that placed the task after the previous one: `first`, `pinned`, `urgent`, `importance`, `due_date`, `age`, `id`. |

`TaskReadOnly` is tagged by `reason`, with the name, the error or the year in `value`. The reasons are `archived` (the year of `archive/<yyyy>/`), `unknown_kind`, `unknown_content_format`, `unknown_status` (written by a newer version of loom), and `invalid_file` (why `task.md` cannot be read). If a task is archived, the reason is always `archived`. An archived task has no `actions`.

### `TaskDetail`

Answer of `GET /v1/tasks/{id}` and of every task write: the fields of `TaskSummary` (without `rank`), and:

| Field | Type | Content |
| --- | --- | --- |
| `body` | string | The notes after the frontmatter; the whole file when `task.md` is invalid. |
| `actions` | list of `Action` | What the task accepts, empty when it is read-only: `{"action": "set_status", "status": …}` for each status it can move to, `{"action": "accept"}` when in inbox, then `{"action": "edit"}` (`PATCH`), `{"action": "pin"}` or `{"action": "unpin"}`, `{"action": "log_note"}`. |

### `TaskEvents`

Answer of `GET /v1/tasks/{id}/events`.

| Field | Type | Content |
| --- | --- | --- |
| `id` | string | Task ID. |
| `events` | list of `EventRecord` | The events, in log order: `v`, `id`, `at`, `actor` (tagged by `type`: `user`, `file`, `daemon`, `agent` with `name`), `machine_id`, and the event tagged by `type`: `created` (`task`), `edited` (`changes`; `field` is one of `title`, `kind`, `status`, `importance`, `due`, `pinned`, `links`, `relations`, `context`), `status_changed` (`from`, `to`), `note_logged` (`text`), `notes_changed` (`from`, `to`: the SHA-256 of the notes, in hex), `undone` (`of`, `changes`), `mr` (`action`: `opened` or `linked`; `mr`: `provider`, `key`, `url`). Each change has `field`, `from` and `to`. |
| `skipped` | list of objects | Lines not replayed (newer schema version, unknown type, invalid): `line`, `raw`, `reason`. |

### `MrOpenRequest`

Body of `POST /v1/tasks/{id}/mr`. Unknown fields are an `invalid_request`.

| Field | Type | Content |
| --- | --- | --- |
| `host` | string | The forge host of the remote, such as `github.com`. |
| `project` | string | The project path on the host, such as `acme/shop`. |
| `head` | string | The source branch. |
| `base` | string | The target branch. |
| `dry_run` | boolean, optional | `true`: send nothing, change nothing. Default `false`. |

### `MrOpenResult`

Answer of `POST /v1/tasks/{id}/mr`.

| Field | Type | Content |
| --- | --- | --- |
| `task` | string | Task ID. |
| `forge` | string | `github` or `gitlab`. |
| `outcome` | string | `opened`: a new draft MR. `linked`: an open MR of the branch. `planned`: a dry run. |
| `mr` | object, optional | The MR as the `delivery` link holds it: `provider` (`github`, `gitlab`), `key` (`acme/shop#412`, `company-a/checkout!88`), `url`. Absent for a dry run. |
| `requests` | list of objects | The requests sent, or planned for a dry run: `method`, `url`, `body` (JSON, absent for a `GET`). |

`loom pr --json` prints a `PrReport`: `dry_run`, `push` (`remote`, `branch`, `command`, `pushed`) and `result`, an `MrOpenResult`.

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

`loomd` draws the ID. No task of the index, in any context, has this ID.

### `TaskEdit`

Body of `PATCH /v1/tasks/{id}`: the fields to change, each optional. An absent field stays as it is.

| Field | Type | Content |
| --- | --- | --- |
| `title` | string | Not blank. |
| `kind` | object | `{"kind": …, "format": …}`, as in `NewTask`; the importance is kept. |
| `importance` | string | `p0` to `p3`. |
| `due` | date \| `null` | `null` removes it. |
| `links` | list | Replaces every link. |
| `relations` | object | Replaces every relation. |

[Actions](#task-endpoints) change the status, accept inbox tasks, pin, unpin, or log notes. `status` or `pinned` in the body is an `invalid_request`.

### `TaskActionRequest`

Body of `POST /v1/tasks/{id}/actions/{action}`, optional.

| Field | Type | Content |
| --- | --- | --- |
| `on` | string, optional | `waiting` only: what or whom the task waits on. |
| `until` | date, optional | `waiting` only: hidden from the priority list until this date. |
| `text` | string | `log_note` only, required: the note, not blank. |
| `kind` | object, optional | `accept` only: `{"kind": …, "format": …}`, as in `NewTask`. Resets importance to default unless `importance` is also set. |
| `importance` | string, optional | `accept` only: `p0` to `p3`. |
| `due` | date \| `null`, optional | `accept` only: due date; `null` removes it. |

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
| `path` | path | Absolute path of the repository or of a directory inside it. `.` components and a `/` at the end are ignored. `..` is rejected. |
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

The environment of `loomd` that its git operations depend on, read at each request. The answer contains no secret value.

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

### `ForgeCheckResult`

Answer of [`POST /v1/diagnostics/forge-check`](#forge-check).

| Field | Type | Content |
| --- | --- | --- |
| `forges` | list of `ForgeCheck` | One per context and forge host with a credential, sorted by context, then host. Empty when no context has a credential. |

`ForgeCheck`:

| Field | Type | Content |
| --- | --- | --- |
| `context` | string | The context. |
| `host` | string | The forge host, such as `github.com`. |
| `kind` | string | `github` or `gitlab`. |
| `api` | string | The API base `loomd` called. |
| `origin` | string | `context` (`[contexts.<ctx>.forges."<host>"]`) or `host` (`[forges."<host>"]`). |
| `source` | object | Tagged by `kind`: `command` (`token_cmd`), or `env` with `name` (`token_env`). |
| `result` | object | Tagged by `state`, see below. |

`result`:

| `state` | Fields | When |
| --- | --- | --- |
| `authenticated` | `login`; `access`, tagged by `kind`: `write` or `read_only` with `scopes`, `unknown` with `reason` | `GET /user` accepted the token. GitHub: `write` when `x-oauth-scopes` holds `repo`, `read_only` otherwise, `unknown` without the header (fine-grained token). GitLab: `write` when the token has the `api` scope, `read_only` otherwise, `unknown` when `GET /personal_access_tokens/self` fails (not a personal access token). |
| `token_unresolved` | `problem`, `error`, `program` (for `command_not_found` only) | `problem`: `command_not_found` (exit status 127 of `sh`), `command_failed` (another non-zero status, or `sh` cannot start), `timeout`, `empty` (no output, or an empty variable), `env_unset`. |
| `rejected` | `status`, `error` | The forge answered `GET /user` with an error status. `error` is the `message` (or `error`) of its body. |
| `sso_required` | `url` (absent when the header has none) | GitHub answered `GET /user` with `403` and an `x-github-sso` header: the token is not authorized for the SAML single sign-on of an organization. GitHub applies single sign-on to the resources of the organization, so `GET /user` can accept a token that the organization refuses. |
| `unreachable` | `error` | No answer, or an answer that cannot be read. |

```json
{
  "forges": [
    {
      "context": "personal",
      "host": "github.com",
      "kind": "github",
      "api": "https://api.github.com",
      "origin": "host",
      "source": { "kind": "command" },
      "result": {
        "state": "authenticated",
        "login": "carol",
        "access": { "kind": "write", "scopes": ["read:org", "repo"] }
      }
    },
    {
      "context": "company-a",
      "host": "gitlab.company-a.example",
      "kind": "gitlab",
      "api": "https://gitlab.company-a.example/api/v4",
      "origin": "host",
      "source": { "kind": "env", "name": "GITLAB_TOKEN" },
      "result": {
        "state": "token_unresolved",
        "problem": "env_unset",
        "error": "the environment variable `GITLAB_TOKEN` is not set for loomd (a service does not see the variables of your shell: use token_cmd)"
      }
    }
  ]
}
```

### `DoctorReport`

Output of `loom repo doctor --json`. The checks and their order are in the [CLI conventions](cli/conventions.md#loom-repo-doctor).

| Field | Type | Content |
| --- | --- | --- |
| `checks` | list of objects | `check` (string: `config`, `roots`, `contexts`, `loomd`, `index`, `unassigned`, `ssh-agent`, `home`, `remote`, `remote <endpoint> (<scheme>, via <slug>)`, `updates`), `status` (`ok`, `warn`, `fail`), `detail` (string), `fix` (string, absent when there is nothing to fix; may hold several lines). |

### `ApiError`

Error returned instead of a response body.

| Field | Type | Required | Content |
| --- | --- | --- | --- |
| `code` | `ErrorCode` | yes | Machine-readable category. Clients make decisions from it, never from `message`. |
| `message` | string | yes | Human-readable explanation, in English. |
| `details` | any JSON value | no | Structured context. Its shape depends on `code` and the endpoint. Omitted when empty. |

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
| `invalid_request` | The request is malformed: unparsable body, unknown or absent field, invalid value. | No: the same request fails the same way. |
| `not_found` | The addressed resource or endpoint does not exist. | No. |
| `version_mismatch` | The client asked for an API version the daemon does not serve. | No: upgrade the client or the daemon. |
| `unavailable` | The daemon stops and cannot serve the request. The same request can succeed after loomd starts again. | Yes, later. |
| `internal` | The daemon failed while it handled a valid request: a bug or an unexpected I/O failure. | Not expected to help. |
| `ambiguous_prefix` | A task ID prefix matches several tasks; `details` lists them. | With a longer prefix. |
| `read_only` | The task cannot be changed: archived, unknown kind, format or status, or an invalid `task.md`. | No. An archived task stays read-only. For the other reasons, fix the file or upgrade loom. |
| `illegal_transition` | The task cannot move from its status to the requested one. | No. |
| `conflict` | An undo found a field changed since the change it reverts. | After the hand edit is recorded, the undo reverts it instead. |
| `nothing_to_undo` | The task has no change to undo. | No. |
| `forge` | The forge refused a request, or did not answer. `message` gives the host, the forge and the reason. | After the cause is fixed (token, scopes, network). |
