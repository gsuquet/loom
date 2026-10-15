# Task files reference

> Formats of `task.md` and `events.jsonl`, the two files of a task directory, as implemented in `loom-core`.

`loomd` reads these files to index the tasks (`loom reindex`). No command creates or changes a task yet.

Path of both files: `<store>/tasks/<id>/`, where `<store>` is the context's `store.path`, `<loom home>/contexts/<context>` by default (see [context stores](configuration.md#context-stores)). An archived task is in `<store>/archive/<yyyy>/<id>/`, with the same two files. `<yyyy>` is the four-digit year of its closing.

| Location | Read | Changed |
| --- | --- | --- |
| `tasks/<id>/` | Yes | Yes |
| `archive/<yyyy>/<id>/` (`<yyyy>` from `1970` to `9999`) | Yes: listed, searched, shown | No: each change is refused (`read_only`) |

`<yyyy>` is the year in UTC. For example, a task closed on 1 January at 00:30 in Paris (31 December, 23:30 UTC) is in the directory of the previous year.

An archived task stays archived, whatever its file contains. If a hand edit sets an open status, the task stays read-only and out of the priority list. To open it again, move its directory back to `tasks/`. `loomd` then indexes it there immediately, or at its next start.

A task ID has one location. If a directory with the same ID is in `tasks/` and in the archive, `tasks/<id>/` is the task, and `loomd` ignores the archived copy. Between two archive years, the earlier year is the task.

## `task.md`

A TOML frontmatter between two `+++` lines, then free Markdown notes.

```markdown
+++
id = "k3q7zm2a"
title = "Login timeout on mobile checkout"
kind = "bug"
context = "company-a"
status = "active"
importance = "p1"
due = 2026-10-15
created = 2026-10-12T09:14:00Z

[[links]]
role = "origin"
provider = "jira"
key = "CORE-1042"
url = "https://jira.example/browse/CORE-1042"

[[links]]
role = "repo"
repo = "company-a/checkout"

[relations]
parent = "h8w2nc4d"
spawned_from = []
blocks = []
+++

Free notes, kind template sections.
```

### Layout

| Part | Rule |
| --- | --- |
| Start line | Exactly `+++`, first line of the file. |
| Frontmatter | [TOML 1.1](https://toml.io/en/v1.1.0). |
| End line | The next line that is exactly `+++`. A `+++` line after it belongs to the notes. |
| Separator | One empty line after the end line, optional. |
| Notes | The rest of the file, free Markdown. May be empty. |
| Line endings | LF or CRLF, taken from the start line. Keys written to a CRLF file use CRLF. |

### Frontmatter keys

| Key | Type | Required | Values |
| --- | --- | --- | --- |
| `id` | string | yes | 8 Crockford base32 characters, the name of the task directory. |
| `title` | string | yes | One line. |
| `kind` | string | yes | `story`, `bug`, `chore`, `research`, `investigation`, `review`, `support`, `content`, `admin`, `interview`, `learning`. |
| `format` | string | no | `kind = "content"` only: `talk`, `article`, `training`, `workshop`. |
| `context` | string | yes | Context name. |
| `status` | string | yes | `inbox`, `ready`, `active`, `waiting`, `done`, `cancelled`. |
| `waiting_on` | string | no | `status = "waiting"` only: what or whom the task waits on. |
| `waiting_until` | local date | no | `status = "waiting"` only: hidden from the priority list until this date. |
| `importance` | string | yes | `p0` (highest) to `p3`. |
| `due` | local date | no | Due date. |
| `pinned` | boolean | no | `true` puts the task first in the priority list. Absent means `false`. |
| `created` | offset date-time | yes | Creation instant, UTC, whole seconds: `2026-10-12T09:14:00Z`. |
| `[[links]]` | array of tables | no | See [links](#links). |
| `[relations]` | table | no | See [relations](#relations). |

A local date is written `2026-10-15` or `"2026-10-15"`. loom reads both forms. loom writes the first form.

The time of the last change is not stored in `task.md`: it is the `at` of the last event in `events.jsonl`.

#### Links

One table per link, distinguished by `role`.

| `role` | Keys | Meaning |
| --- | --- | --- |
| `origin` | `provider`, `key`, `url` (optional) | Where the task came from. |
| `related` | `provider`, `key`, `url` (optional) | An item that belongs to the same done condition: an MR, a CI pipeline. |
| `repo` | `repo` | The repository to work in, a slug of the repo index. |

`links` may also be written as an inline array of inline tables. loom writes it as an array of tables.

#### Relations

| Key | Type | Meaning |
| --- | --- | --- |
| `parent` | string: task ID | The parent task. |
| `spawned_from` | array of task IDs | Tasks this one was spawned from. |
| `blocks` | array of task IDs | Tasks this one blocks. |

Absent keys are empty. `relations` may also be an inline table.

### Notes template

A new task starts its notes with sections that depend on its kind. They are written once, at creation.

| Kind | Sections |
| --- | --- |
| `story` | Context, Acceptance, Notes |
| `bug` | Symptoms, Cause, Fix, Verification |
| `chore`, `admin` | Notes |
| `research` | Question, Findings, Decision |
| `investigation` | Signal, Cause, Disposition |
| `review` | Scope, Verdict |
| `support` | Request, Answer |
| `content` | Audience, Outline, Delivery |
| `interview` | Notes, Feedback |
| `learning` | Goal, Progress, Completion |

Each section is a `## <name>` heading followed by an empty line.

### Rewrites

| Case | Behavior |
| --- | --- |
| No field changed | The file is written back byte for byte. |
| A field changed | Only its keys are rewritten. A rewritten key keeps its place, the comments above it, and the whitespace and comment around its value. |
| A new key | Added after the last top-level key. |
| A field emptied (`due`, `format`, `waiting_on`, `waiting_until`, `pinned = false`, `relations.parent`) | Its key is removed. |
| Links changed | A link that stays keeps its table, comments and unknown keys included. New links get a new table. |
| Unknown key, at any level | Kept, and reported (`estimate`, `relations.duplicates`, `links[0].branch`). |
| Link of an unknown `role` | Kept after the known links, not read, and reported (`links[1]`). |
| Unknown `kind`, `format` or `status` | Read. The task is read-only: the file is never rewritten. |
| Task in `archive/<yyyy>/` | Read. The task is read-only: no command writes the file again. |

### Errors

The file is rejected, and never rewritten, in these cases. Lines start at 1.

| Case | Line | Notes kept |
| --- | --- | --- |
| First line is not `+++` | 1 | The whole file. |
| No end `+++` line | Last line of the file | None located. |
| Invalid TOML | Line of the error | Yes. |
| Required key absent | Line of the end `+++` | Yes. |
| Value of the wrong type or form | Line of the key | Yes. |

Messages:

```text
line <n>: a task file starts with a `+++` line
line <n>: no `+++` line closes the frontmatter opened on line 1
line <n>: invalid TOML: <message>
line <n>: `<key>`: missing
line <n>: `<key>`: <message>
```

## `events.jsonl`

Append-only. One JSON object per line, LF line endings. CRLF is also accepted on read. Blank lines are ignored.

```json
{"v":1,"id":"e0000002","at":"2026-10-12T09:14:00Z","actor":{"type":"agent","name":"reviewer"},"machine_id":"laptop","type":"status_changed","from":{"status":"active"},"to":{"status":"waiting","waiting_on":"Alice","waiting_until":"2026-10-20"}}
```

### Common fields

| Field | Type | Values |
| --- | --- | --- |
| `v` | integer | Schema version. Current: `1`. |
| `id` | string | Event ID, 8 Crockford base32 characters, unique within the log. |
| `at` | string | Instant, RFC 3339 in UTC, whole seconds. |
| `actor` | object | `{"type":"user"}` (CLI, TUI), `{"type":"file"}` (a hand edit of `task.md`, recorded by `loomd`), `{"type":"daemon"}` or `{"type":"agent","name":"<agent>"}`. |
| `machine_id` | string | Name of the machine the event was recorded on: the `id` of [`machine.toml`](configuration.md#machinetoml). |
| `type` | string | Event type, see below. |

### Event types

| `type` | Payload fields | Meaning |
| --- | --- | --- |
| `created` | `task`: the task, as below | The task was created, as it was then. |
| `edited` | `changes`: array of field changes | Fields other than the status changed. |
| `status_changed` | `from`, `to`: status objects | The status changed. |
| `note_logged` | `text`: string | A note was logged. |
| `undone` | `of`: event ID; `changes`: array of field changes | A previous change was reverted. |

In `created`, a task carries the frontmatter keys as JSON, plus `pinned` and `updated`. Dates and instants are strings, and `links` is an array of objects with `role`.

A status object is `{"status":"<status>"}`, with `waiting_on` and `waiting_until` when the status is `waiting`.

A field change is `{"field":"<field>","from":<value>,"to":<value>}`, with `field` one of `title`, `kind`, `status`, `importance`, `due`, `pinned`, `links`, `relations`. `kind` and `status` values are objects (`{"kind":"content","format":"talk"}`, a status object). `links` and `relations` values are the whole array or table.

```json
{"v":1,"id":"e0000001","at":"2026-10-12T09:14:00Z","actor":{"type":"user"},"machine_id":"laptop","type":"edited","changes":[{"field":"due","from":null,"to":"2026-10-15"}]}
```

### Versioning

| Line | Behavior |
| --- | --- |
| `v` at most the current version, known `type` | Replayed. |
| `v` newer than the current version | Kept as is, not replayed, reported as `newer_version`. |
| Unknown `type` | Kept as is, not replayed, reported as `unknown_type`. |
| Not JSON, not an object, `v` or `type` absent, or invalid payload | Kept as is, not replayed, reported as `invalid`. |

A line that is not replayed is never an error for the rest of the log. Lines are never rewritten.
