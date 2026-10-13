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
| `POST` | `/v1/repos/scan` | `200` | [`ScanResult`](#scanresult) |
| `POST` | `/v1/repos/register` | `200` | [`RepoSummary`](#reposummary) |
| `POST` | `/v1/repos/sync` | `200` | [`SyncResult`](#syncresult) |

`uptime_secs` is computed, and `<loom home>/state/update.json` read for `update`, when the request is handled. An unreadable state file is logged and reported as an empty state.

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

Scans run one at a time. A scan request made while a scan runs waits for that scan and receives its result. Registration and removal requests wait for a running scan.

`POST /v1/repos/register` takes a [`RegisterRequest`](#registerrequest). The repository is the path itself or its nearest ancestor with a `.git` entry (directory, file or symbolic link); symbolic links in the path are kept. It is identified like a scanned repository, added or updated, and marked `registered`. A repository already indexed keeps its slug while its context and name do not change; otherwise it gets `<context>/<name>`, or the lowest free `<context>/<name>-N` (N ≥ 2).

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
| Backoff | After a failed fetch, the repository's next update waits 2, 4, then 8 times the interval (never more), counting consecutive failures. Every repository of the same host (`host[:port]` of the primary remote, or its URL for a local path) waits as long, unless a fetch of that host that started earlier succeeded in the meantime. A successful fetch clears the host's backoff. Other hosts are not affected. |
| Policy | `[repos."<path>"] update`, otherwise the context's `update`, otherwise `ff`; `fetch` for an `unassigned` repository without `[repos."<path>"] update`. |
| git commands | `git fetch --no-prune <primary>` (timeout 120 s; tags as git's automatic tag following and `remote.<name>.tagOpt` decide, an existing local tag is never overwritten); `git merge --ff-only --no-autostash <commit>` (10 min); `git fetch --no-prune <primary> <merge ref>:refs/heads/<default branch>` (120 s). On timeout, the command's process group gets `SIGTERM`, then `SIGKILL` 5 s later. |
| git environment | `loomd`'s environment and the user's git configuration and hooks, plus `GIT_TERMINAL_PROMPT=0`, `GIT_CEILING_DIRECTORIES=<parent of the working tree>`, `GIT_SSH_COMMAND="ssh -o BatchMode=yes"` unless `core.sshCommand` (read with `git config --get`), `GIT_SSH_COMMAND` or `GIT_SSH` is set; `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_COMMON_DIR`, `GIT_NAMESPACE` and `GIT_PREFIX` removed. |

`POST /v1/repos/sync` takes a [`SyncRequest`](#syncrequest) and updates the repositories it names (every repository of the index when it names none) now, regardless of their schedule and of the backoff, then answers with a [`SyncResult`](#syncresult), sorted by slug, after every result is stored. A repository already being updated is updated again once that update ends. The answer may take as long as the updates (each command has its timeout). With `dry_run: true`, nothing runs: each state is read now and the result says what an update would do.

### Errors

Every error response has an [`ApiError`](#apierror) body.

| Case | HTTP status | `code` |
| --- | --- | --- |
| No endpoint at the path | `404` | `not_found` |
| Endpoint exists, method not accepted | `405` (with an `allow` header) | `invalid_request` |
| No repository with the slug of `/v1/repos/{context}/{name}`, or with a slug of a `SyncRequest` | `404` | `not_found` |
| Sync: body not a valid `SyncRequest` | `400` | `invalid_request` |
| Invalid query parameter, unknown parameter, invalid `context`, or `refresh` neither `true` nor `false` | `400` | `invalid_request` |
| Register: body not a valid `RegisterRequest`, path not absolute or with a `..` component, no repository at or above the path, or a repository whose `.git` or config cannot be read | `400` | `invalid_request` |
| The index cannot be read or written | `500` | `internal` |
| The daemon cannot read its clock as a timestamp | `500` | `internal` |
| The daemon stops before answering a scan, register, remove, refresh or sync request | `503` | `unavailable` |

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
```

## Types

The JSON Schema (draft 2020-12) of each type is generated from the Rust types and committed under [`schemas/`](schemas/). A test fails when a committed schema differs from the types; `mise run docs` regenerates them.

| Type | Schema |
| --- | --- |
| `ApiError` | [`schemas/api_error.json`](schemas/api_error.json) |
| `ApiVersion` | [`schemas/api_version.json`](schemas/api_version.json) |
| `ContextAssignment` | [`schemas/context_assignment.json`](schemas/context_assignment.json) (not returned by any endpoint or command yet) |
| `ErrorCode` | [`schemas/error_code.json`](schemas/error_code.json) |
| `LastUpdate` | [`schemas/last_update.json`](schemas/last_update.json) (in `RepoSummary` and `SyncResult`) |
| `RegisterRequest` | [`schemas/register_request.json`](schemas/register_request.json) |
| `RepoIdentity` | [`schemas/repo_identity.json`](schemas/repo_identity.json) (inlined in `RepoSummary`) |
| `RepoList` | [`schemas/repo_list.json`](schemas/repo_list.json) |
| `RepoState` | [`schemas/repo_state.json`](schemas/repo_state.json) (in `RepoSummary`) |
| `RepoSummary` | [`schemas/repo_summary.json`](schemas/repo_summary.json) |
| `ScanResult` | [`schemas/scan_result.json`](schemas/scan_result.json) |
| `StatusResponse` | [`schemas/status_response.json`](schemas/status_response.json) |
| `SyncRequest` | [`schemas/sync_request.json`](schemas/sync_request.json) |
| `SyncResult` | [`schemas/sync_result.json`](schemas/sync_result.json) |
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
| Request | Rejected (`#[serde(deny_unknown_fields)]`): `RegisterRequest`, and the query parameters of `GET /v1/repos` and `GET /v1/repos/{context}/{name}`. |
| Response | Ignored, so that an older client can read a newer daemon. |

Unknown enum values (`api_version`, `code`, `channel`, `channel_source`, `profile`, `policy`, `kind`, `operation`) are rejected.

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

Every field is required, except `update`.

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
  }
}
```

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
| `last_used` | `Timestamp` or absent | Last time the repository was used. Never set by this version. |
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

### `RegisterRequest`

| Field | Type | Content |
| --- | --- | --- |
| `path` | path | Absolute path of the repository or of a directory inside it. `.` components and a trailing `/` are ignored; `..` is rejected. |

### `ScanResult`

| Field | Type | Content |
| --- | --- | --- |
| `added` | integer (u64) | Repositories added to the index. |
| `updated` | integer (u64) | Repositories whose identity changed (remotes, context, forge or slug). |
| `removed` | integer (u64) | Repositories removed from the index. |
| `errors` | list of objects | Paths that could not be read: `path`, `message`. The repositories they hide are kept unchanged. A repository whose path is not valid UTF-8 is reported here and not indexed. |
| `duration_ms` | integer (u64) | Duration of the scan, in whole milliseconds. |

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
