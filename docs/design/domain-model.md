# Domain model

> Tasks, events, sources and knowledge pages; kinds, granularity, status, order and contexts.

Four entities.

| Entity | What it is | Lifecycle |
| --- | --- | --- |
| **Task** | Something I still have to do, a conception included | Has a done state, is prioritized |
| **Event** | Something that happened (status change, note, commit, MR, agent session, meeting, gate result) | Append-only, timestamped |
| **Source** | Something I can cite (article, book, feed item, file, meeting, test result, experience) | `captured → to read → read → distilled → reference`, or `dismissed` |
| **Knowledge page** | What is true on a topic, each statement linked to its sources | Living, scoped to a context or shared |

Source and Knowledge page replace Note. See [Knowledge](knowledge.md).

The ledger ("anything I do shall be written") is the event stream.

### Task kinds

Kind = nature of the work. It drives the body template, the done condition and the default importance. Source = where it came from. They are independent.

| Kind | Typical sources | Done when |
| --- | --- | --- |
| Story | Jira, GitHub/GitLab issues | Delivered (MR merged, remote closed) |
| Bug | Jira, issues, spawned from support | Fix merged and verified |
| Chore | Me, Jira tech tasks, cron | Done |
| Research | Me | Conclusion written (findings + decision) |
| Investigation | Datadog alert, Freshservice incident | Cause identified + disposition (spawned task, noise, upstream) |
| Review | MR/PR I started reviewing; before that it is a [queue entry](reviews.md#waiting-on-me) | Verdict posted for the current head SHA; a new push reopens it |
| Support | Freshservice request | Requester has an answer |
| Content | Me | Delivered or published (optional `format`: talk, article, training, workshop) |
| Admin | Me, cron (timesheets, expenses, staffing) | Done |
| Interview | Me, staffing | Feedback written |
| Learning | Me | Course or certification completed |
| Conception | Me, a meeting, a ticket | Decisions agreed and subtasks approved |

- No Incident kind: an incident is an Investigation with high severity and a due date.
- Epic / project = `parent` relation, not a kind nor an entity. Children see the parent's `files/` (design docs, ADRs). A parent is a task of the child's context, never the child itself nor one of its descendants; loomd checks it when a parent is set (a hand edit is recorded as written).
- A conception is a design that creates subtasks. It is a task, not an entity. See [Conceptions](knowledge.md#conceptions).
- Meetings are events and sources, not tasks. Preparation is a task. An action item is a proposal in the Inbox; once accepted, it is a task with a `source` link to the meeting.
- A task can span several repos: refinement lists them as `repo` links, and more can be added. A task can hold files, stored in the context's store and secret-scanned when added.

### Links and relations

- `relations` holds task IDs only: `parent`, `spawned_from`, `blocks`, `supersedes`. `spawned_from` is never an event.
- `[[links]]` reaches everything else, by role: `origin`, `repo`, `source`, `knowledge`, `claim`, `person`.
- A parent is never archived while it has open children.

### Ingestion origins

GitLab/GitHub MRs and issues, Jira, Freshservice, Datadog, calendar, cron, me.

- **Dedup key**: `(connector, external_id)` in one store, for a connector instance ([Identity](connector-ports.md#identity)); a host does not name a Datadog organization. MRs keep `(provider, host, key)`. Ingestion is idempotent. A flapping Datadog monitor is grouped by monitor in the Inbox: one task per monitor, its groups in the snapshot. If any group fires again after a dismissal, it is a new row.
- **Cron**: a schedule generates one task per occurrence. RRULE (RFC 5545) rather than cron syntax, to express e.g. "last working day of the month" (`FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1`). Dedup key `(schedule_id, occurrence_date)`. Missed occurrences: only the latest is created. Previous occurrence still open: per-schedule policy `skip | stack | replace`.
- **No Slack connector**: `loom add --from <url>` stays the manual way in.
- **Client trackers** (Jira, Freshservice): API token availability and write scope depend on client policy. Manual fallback.

### Granularity rule

A new task is created whenever the done condition differs.

- Jira bug + my MR + its CI = one task with several links.
- Freshservice request -> Jira bug = two tasks: Support (done when answered) and Bug (done when merged), linked by `spawned_from`.
- Reviewing a colleague's MR = its own task.
- Delegating part of a task = creating a child task and delegating it fully.

### Status

`inbox` (to triage, default for ingested items) · `ready` · `active` · `waiting { on, until }` · `done` · `cancelled`.

- Archived is a location, not a status: a task moves to the archive N days after `done`/`cancelled`.
- Task status (my work) and issue status (the tracker's) are separate. A terminal remote state (merged, closed, resolved) closes the local task; otherwise neither drives the other. Changing an issue status is an explicit edit of the issue mirror (see [Tracker sync](tracker-sync.md)). Rules like "task active -> issue In Progress" can come later.

### Order

Today shows sections, not one ranked list: Urgent, In progress, Due, Waiting on you, Follow-ups, Planned, With an agent. See [Today](user-workflow.md#today).

Inputs are stored, and the order inside a section is computed:

- `importance` p0–p3 (default per kind/source), `due` (or source SLA), `pinned`, number of tasks blocked, age.
- Order is lexicographic, no weights ([ADR-0013](../adr/0013-task-store.md)): pinned, then overdue or due today (earliest first), then importance, then due date (none last), then age (oldest first).
- `inbox` has its own page. `waiting` tasks are hidden while their `until` is in the future or unset, and listed again from the `until` date, saying they came back.
- `loom ls --why` names the rule that ordered each task after its predecessor.

Several tasks can be in progress at once, in several contexts. There is no focus task. `loom go <id>` sets a task `active` and changes directory to its worktree or repo. `loom log` without an ID goes to the day's journal, in the last-used context, shown before saving.

### Context

Exactly one per task. Resolution order: `-c`/`--context` -> `LOOM_CONTEXT` -> current repo remote -> `match.paths` -> `default_context`. No global "current context" file: two terminals can be in two contexts. Repos are assigned to contexts by the same `match` rules ([Repositories](repos.md#contexts)). A capture moves to another context with `loom set <id> context=<name>` or `c`, and an item from a connector stays in that connector's context. A moved task keeps its ID and history ([ADR-0015](../adr/0015-move-a-task-between-contexts.md)). A task with a parent or children does not move, since a parent is in the context of its children.

Git identity stays managed by the dotfiles.

```toml
[contexts.company-a]
match.hosts = ["gitlab.com/company-a"]
match.paths = ["~/work/company-a"]
store.path = "~/.loom/contexts/company-a"
machines = ["laptop"]                       # absent: every machine
store.backup.remote = "git@…"               # encryption is not built, see storage.md

[contexts.company-a.forges."gitlab.com"]     # see reviews.md
token_cmd = "op read op://…"

[contexts.company-a.connectors.datadog]      # see connector-ports.md
kind = "datadog"
site = "datadoghq.eu"
token_cmd = "op read op://…"
app_key_cmd = "op read op://…"
```
