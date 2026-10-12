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

`uptime_secs` is computed, and `<loom home>/state/update.json` read for `update`, when the request is handled. An unreadable state file is logged and reported as an empty state.

### Errors

Every error response has an [`ApiError`](#apierror) body.

| Case | HTTP status | `code` |
| --- | --- | --- |
| No endpoint at the path | `404` | `not_found` |
| Endpoint exists, method not accepted | `405` (with an `allow` header) | `invalid_request` |
| The daemon cannot read its clock as a timestamp | `500` | `internal` |

```bash
curl --unix-socket ~/.loom/run/loomd.sock http://loomd/v1/status
```

## Types

The JSON Schema (draft 2020-12) of each type is generated from the Rust types and committed under [`schemas/`](schemas/). A test fails when a committed schema differs from the types; `mise run docs` regenerates them.

| Type | Schema |
| --- | --- |
| `ApiError` | [`schemas/api_error.json`](schemas/api_error.json) |
| `ApiVersion` | [`schemas/api_version.json`](schemas/api_version.json) |
| `ErrorCode` | [`schemas/error_code.json`](schemas/error_code.json) |
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
| Request | Rejected (`#[serde(deny_unknown_fields)]`). No request type exists yet. |
| Response | Ignored, so that an older client can read a newer daemon. |

Unknown enum values (`api_version`, `code`, `channel`, `profile`, `policy`, `kind`) are rejected.

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
| `channel` | `stable` \| `dev` \| `null` | Effective `[update] channel`; `null` for a local build without one: nothing is checked. |
| `check_interval_secs` | integer (u64) | Effective interval between checks, before jitter. |
| `last_check` | `Timestamp` \| `null` | `last_check` of the state. |
| `available` | string \| `null` | `available` of the state, `null` when it is not newer than the daemon's version. |
| `pending` | object \| `null` | `pending` of the state: `from`, `to`, `started_at`. |
| `failed_versions` | list of strings | `failed_versions` of the state. |
| `last_result` | object \| `null` | `last_result` of the state, tagged by `kind` ([kinds](configuration.md#state)). |

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
