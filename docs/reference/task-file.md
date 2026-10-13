# Task files reference

> Formats of the files of a task directory, as implemented in `loom-core`.

No command reads or writes these files yet: the formats are implemented as pure functions (`parse_events`, `render_event`) for the task store.

Path: `<loom home>/contexts/<context>/tasks/<id>/`.

## `events.jsonl`

Append-only. One JSON object per line, LF line endings; CRLF is accepted when reading. Blank lines are ignored.

```json
{"v":1,"id":"e0000002","at":"2026-10-12T09:14:00Z","actor":{"type":"agent","name":"reviewer"},"machine_id":"laptop","type":"status_changed","from":{"status":"active"},"to":{"status":"waiting","waiting_on":"Alice","waiting_until":"2026-10-20"}}
```

### Common fields

| Field | Type | Values |
| --- | --- | --- |
| `v` | integer | Schema version. Current: `1`. |
| `id` | string | Event ID, 8 Crockford base32 characters, unique within the log. |
| `at` | string | Instant, RFC 3339 in UTC, whole seconds. |
| `actor` | object | `{"type":"user"}`, `{"type":"daemon"}` or `{"type":"agent","name":"<agent>"}`. |
| `machine_id` | string | Name of the machine the event was recorded on. |
| `type` | string | Event type, see below. |

### Event types

| `type` | Payload fields | Meaning |
| --- | --- | --- |
| `created` | `task`: the task, as below | The task was created, as it was then. |
| `edited` | `changes`: array of field changes | Fields other than the status changed. |
| `status_changed` | `from`, `to`: status objects | The status changed. |
| `note_logged` | `text`: string | A note was logged. |
| `undone` | `of`: event ID; `changes`: array of field changes | A previous change was reverted. |

A task, in `created`, is the JSON form of the task: `id`, `title`, `kind`, `format`, `context`, `status`, `waiting_on`, `waiting_until`, `importance`, `due`, `pinned`, `links` (objects with a `role`), `relations`, `created`, `updated`; dates and instants as strings.

A status object is `{"status":"<status>"}`, with `waiting_on` and `waiting_until` when the status is `waiting`.

A field change is `{"field":"<field>","from":<value>,"to":<value>}`, with `field` one of `title`, `kind`, `status`, `importance`, `due`, `pinned`, `links`, `relations`. `kind` and `status` values are objects (`{"kind":"content","format":"talk"}`, a status object); `links` and `relations` values are the whole array or table.

```json
{"v":1,"id":"e0000001","at":"2026-10-12T09:14:00Z","actor":{"type":"user"},"machine_id":"laptop","type":"edited","changes":[{"field":"due","from":null,"to":"2026-10-15"}]}
```

### Versioning

| Line | Behavior |
| --- | --- |
| `v` at most the current version, known `type` | Replayed. |
| `v` newer than the current version | Kept as is, not replayed, reported as `newer_version`. |
| Unknown `type` | Kept as is, not replayed, reported as `unknown_type`. |
| Not JSON, not an object, `v` or `type` missing, or invalid payload | Kept as is, not replayed, reported as `invalid`. |

A line that is not replayed is never an error for the rest of the log. Lines are never rewritten.
