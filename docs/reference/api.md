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
| `GET` | `/v1/repos/{context}/{name}` | `200` | [`RepoSummary`](#reposummary) |
| `DELETE` | `/v1/repos/{context}/{name}` | `200` | [`RepoSummary`](#reposummary) of the removed repository |
| `POST` | `/v1/repos/scan` | `200` | [`ScanResult`](#scanresult) |
| `POST` | `/v1/repos/register` | `200` | [`RepoSummary`](#reposummary) |

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

`POST /v1/repos/scan` walks the roots (`[repos] roots`, then every `match.paths`, `~` expanded with `HOME`) and reads every registered repository in place, then applies the changes to the index in one transaction:

- a repository found is added, or updated when its identity changed (remotes, context, forge or slug);
- a repository no longer found under a scanned root is removed, unless an error hides it (an unreadable directory above it, an unreadable `.git` or config); the repositories under other roots are kept;
- a registered repository is kept while its `.git` exists, even deeper than `max_depth`, excluded, or outside the roots, and removed once its `.git` is gone;
- contexts are recomputed for every repository, so slugs follow a rule change.

Scans run one at a time. A scan request made while a scan runs waits for that scan and receives its result. Registration and removal requests wait for a running scan.

`POST /v1/repos/register` takes a [`RegisterRequest`](#registerrequest). The repository is the path itself or its nearest ancestor with a `.git` entry (directory, file or symbolic link); symbolic links in the path are kept. It is identified like a scanned repository, added or updated, and marked `registered`. A repository already indexed keeps its slug while its context and name do not change; otherwise it gets `<context>/<name>`, or the lowest free `<context>/<name>-N` (N ≥ 2).

`DELETE /v1/repos/{context}/{name}` removes the repository from the index, registered or not. A scan adds a repository found under a root again.

### Errors

Every error response has an [`ApiError`](#apierror) body.

| Case | HTTP status | `code` |
| --- | --- | --- |
| No endpoint at the path | `404` | `not_found` |
| Endpoint exists, method not accepted | `405` (with an `allow` header) | `invalid_request` |
| No repository with the slug of `/v1/repos/{context}/{name}` | `404` | `not_found` |
| Invalid query parameter, unknown parameter or invalid `context` | `400` | `invalid_request` |
| Register: body not a valid `RegisterRequest`, path not absolute or with a `..` component, no repository at or above the path, or a repository whose `.git` or config cannot be read | `400` | `invalid_request` |
| The index cannot be read or written | `500` | `internal` |
| The daemon cannot read its clock as a timestamp | `500` | `internal` |
| The daemon stops before answering a scan, register or remove request | `503` | `unavailable` |

```bash
curl --unix-socket ~/.loom/run/loomd.sock http://loomd/v1/status
curl --unix-socket ~/.loom/run/loomd.sock 'http://loomd/v1/repos?context=personal&query=loom'
curl --unix-socket ~/.loom/run/loomd.sock -X POST http://loomd/v1/repos/scan
curl --unix-socket ~/.loom/run/loomd.sock -X POST -H 'content-type: application/json' \
  -d '{"path": "/home/me/src/tool"}' http://loomd/v1/repos/register
curl --unix-socket ~/.loom/run/loomd.sock -X DELETE http://loomd/v1/repos/unassigned/tool
```

## Types

The JSON Schema (draft 2020-12) of each type is generated from the Rust types and committed under [`schemas/`](schemas/). A test fails when a committed schema differs from the types; `mise run docs` regenerates them.

| Type | Schema |
| --- | --- |
| `ApiError` | [`schemas/api_error.json`](schemas/api_error.json) |
| `ApiVersion` | [`schemas/api_version.json`](schemas/api_version.json) |
| `ContextAssignment` | [`schemas/context_assignment.json`](schemas/context_assignment.json) (not returned by any endpoint or command yet) |
| `ErrorCode` | [`schemas/error_code.json`](schemas/error_code.json) |
| `RegisterRequest` | [`schemas/register_request.json`](schemas/register_request.json) |
| `RepoIdentity` | [`schemas/repo_identity.json`](schemas/repo_identity.json) (inlined in `RepoSummary`) |
| `RepoList` | [`schemas/repo_list.json`](schemas/repo_list.json) |
| `RepoSummary` | [`schemas/repo_summary.json`](schemas/repo_summary.json) |
| `ScanResult` | [`schemas/scan_result.json`](schemas/scan_result.json) |
| `StatusResponse` | [`schemas/status_response.json`](schemas/status_response.json) |
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
| Request | Rejected (`#[serde(deny_unknown_fields)]`): `RegisterRequest`, and the query parameters of `GET /v1/repos`. |
| Response | Ignored, so that an older client can read a newer daemon. |

Unknown enum values (`api_version`, `code`, `channel`, `channel_source`, `profile`, `policy`, `kind`) are rejected.

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
  "registered": false
}
```

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
