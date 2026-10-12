# Domain model

> Tasks, events and notes; kinds, sources, granularity, status, priority and contexts.

Three entities.

| Entity | What it is | Lifecycle |
| --- | --- | --- |
| **Task** | Something I still have to do | Has a done state, is prioritized |
| **Event** | Something that happened (status change, note, commit, MR, agent session, meeting, gate result) | Append-only, timestamped |
| **Note** | A learning | Scoped `context` or `portable` |

The priority list is the set of open tasks. The ledger ("anything I do shall be written") is the event stream.

### Task kinds

Kind = nature of the work. It drives the body template, the done condition and the default importance. Source = where it came from. They are independent.

| Kind | Typical sources | Done when |
| --- | --- | --- |
| Story | Jira, GitHub/GitLab issues | Delivered (MR merged, remote closed) |
| Bug | Jira, issues, spawned from support | Fix merged and verified |
| Chore | Me, Jira tech tasks, cron | Done |
| Research | Me | Conclusion written (findings + decision) |
| Investigation | Datadog alert, Freshservice incident, Slack | Cause identified + disposition (spawned task, noise, upstream) |
| Review | MR/PR assigned to me | Verdict posted for the current head SHA; a new push reopens it |
| Support | Freshservice request, Slack | Requester has an answer |
| Presentation | Me | Delivered at a date |
| Admin | Me, cron (timesheets, expenses, staffing, interviews) | Done |

- No Incident kind: an incident is an Investigation with high severity and a due date.
- Epic / project = `parent` relation, not a kind.
- Meetings are events (from the calendar), not tasks. Preparation is a task; action items are tasks with `spawned_from: <event>`.
- Open: Presentation may become Content (talks, articles, trainings) with a subtype.

### Sources

GitLab/GitHub MRs and issues, Jira, Freshservice, Slack, Datadog, calendar, cron, me.

- **Dedup key**: `(provider, host, external_id)`. Ingestion is idempotent. A flapping Datadog monitor reopens its task (key: monitor id + group).
- **Cron**: a schedule generates one task per occurrence. RRULE (RFC 5545) rather than cron syntax, to express e.g. "last working day of the month" (`FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1`). Dedup key `(schedule_id, occurrence_date)`. Missed occurrences: only the latest is created. Previous occurrence still open: per-schedule policy `skip | stack | replace`.
- **Slack**: no ticket object, ingestion needs a trigger (reaction, saved item, or `loom add --from <url>`). Client workspaces usually require admin approval for apps: start manual.
- **Client trackers** (Jira, Freshservice): API token availability and write scope depend on client policy. Manual fallback.

### Granularity rule

A new task is created whenever the done condition differs.

- Jira bug + my MR + its CI = one task with several links.
- Slack report -> Freshservice -> Jira bug = two tasks: Support (done when answered) and Bug (done when merged), linked by `spawned_from`.
- Reviewing a colleague's MR = its own task.
- Delegating part of a task = creating a child task and delegating it fully.

### Status

`inbox` (to triage, default for ingested items) · `ready` · `active` · `waiting { on, until }` · `done` · `cancelled`.

- Archived is a location, not a status: a task moves to the archive N days after `done`/`cancelled`.
- Task status (my work) and issue status (the tracker's) are separate. A terminal remote state (merged, closed, resolved) closes the local task; otherwise neither drives the other. Changing an issue status is an explicit edit of the issue mirror (see [Tracker sync](tracker-sync.md)). Rules like "task active -> issue In Progress" can come later.

### Priority

Inputs are stored, the score is computed:

- `importance` p0–p3 (default per kind/source), `due` (or source SLA), `pinned`, number of tasks blocked, age.
- `waiting` tasks with a future `until` are hidden from the list.
- `loom ls --why` explains the ranking.

### Context

Exactly one per task. Resolution order: `-C` -> `LOOM_CONTEXT` -> current repo remote -> `match.paths` -> `default_context`. No global "current context" file: two terminals can be in two contexts.

Git identity stays managed by the dotfiles.

```toml
[contexts.company-a]
match.hosts = ["gitlab.com/company-a"]
match.paths = ["~/work/company-a"]
store.path = "~/.loom/contexts/company-a"
store.backup = { remote = "git@…", encrypt = false }
secrets.gitlab_token_cmd = "op read op://…"
secrets.datadog_token_cmd = "op read op://…"
```
