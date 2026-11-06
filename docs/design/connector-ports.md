# Connector ports

> The ports, instances, poll loop and sync rules that the Datadog, Jira and Freshservice adapters share. What each tool brings and the write levels are in [Connectors](connectors.md). The delivery order is in the [roadmap](../roadmap.md#connector-tasks).

Vendor facts below were read on 2026-11-05 from the pages listed under [Sources](#sources). Anything those pages leave open is in [Real checks](#real-checks), and an adapter treats it as unknown until I run that check.

## What exists today

- **Forges**: `loom_forge::client` has `Forge`, `OpenForge`, `GithubForge`, `GitlabForge`. They are blocking, and `ForgeError` and `http::classify` are specific to GitHub and GitLab.
- **Poll**: `crates/loomd/src/reviews.rs` is one thread that polls every context and host in turn. `mr_worker` (#213) runs after each poll on its own thread. #218, still open, would add queue starts on the poll thread.
- **Writes**: nothing in the code holds a write. There is no Outbox, and no Held, Sent or Not sent state in `crates/`, after #213 or in #218. `held` in the code means a context store on this machine, a lock, or, in #218, an entry that cannot move. `loom pr` writes straight to the forge. So [Write states](connectors.md#write-states) is still target behaviour for every write, and the Outbox is a task of its own.
- **Connectors**: no code has a connector type, `remote/` directory, connector status or `issues/` mirror.
- **Reaching me**: no notification is delivered today, `loom ls` hides `inbox` tasks (`is_listed`), and the app comes later. See [Urgent](#urgent).
- **Doctor**: the only one is `loom repo doctor`. There is no `loom doctor` command.

## Ports

Small traits in a new crate, `loom-connector`. Calls block, as the forge clients do, and run on the thread of their instance. Decisions are pure functions, with I/O only in each adapter's `client` module.

```rust
pub trait Connector {
    fn capabilities(&self) -> Capabilities;                    // pure, constant per kind
    fn check(&self) -> Result<Identity, ConnectorError>;       // I/O: doctor and first poll
    fn tracker(&self) -> Option<&dyn TrackerPort> { None }     // pure: Jira and Freshservice give themselves
}

pub trait IngestPort: Connector {
    fn list(&self, known: &Known) -> Result<Listing, ConnectorError>;      // what I own, every page
    fn pool(&self, known: &Known) -> Result<Listing, ConnectorError> {     // the team pool
        Ok(Listing::empty())
    }
    fn read(&self, item: &ItemRef) -> Result<ItemRead, ConnectorError>;    // one item that left a list
}

pub trait TrackerPort: Connector {
    fn pull(&self, item: &ItemRef) -> Result<MirrorRead, ConnectorError>;  // full remote state of one issue
    // `push` comes with the writes, see Delivery. It is not stubbed before.
}
```

| Port | Implemented by | When |
| --- | --- | --- |
| Ingest | Datadog, Jira, Freshservice | Read-only tasks |
| Tracker | Jira, Freshservice | Read-only tasks: `pull`. Writes later: `push` |
| Forge | The existing `Forge` traits | Unchanged |
| Agent data | Datadog first | Not built here. It starts with the first [agent tool](agents.md#external-data-for-agents), as `fn query(&self, query: AgentQuery) -> Result<AgentData, ConnectorError>` over closed enums, on the client the adapter already has |

**`Forge` stays as it is.** MRs have reasons, verdicts, drafts and refs that no ticket has, and #218 is changing that code now. A shared trait would only hold `user()`. GitLab and GitHub report into [Connector status](#connector-status) and do nothing else here.

**Types**, in `loom-connector`, with no I/O:

| Type | Holds |
| --- | --- |
| `Listing` | `items`, `complete`, `truncated`, `wait`: the rate limit wait when a 429 held required reads inside `list`. `complete` is false when a page cap or an incomplete read stopped it, and an incomplete list never means an item left. `truncated` is `pool_limit` only |
| `Item` | `ItemRef` (key, `remote_id`), title, web URL, proposed `Kind`, proposed importance, due, `RemoteState`, remote `updated`, the normalized snapshot, and `detail_read`: whether the read this item needs for a task happened |
| `RemoteState` | `Open`, `Terminal`, `Firing { groups }`, `NotMine { assignee }` |
| `ItemRead` | `Found(Item)` or `Gone` (404) |
| `Known` | By key, what the loop kept from the last polls: remote `updated`, the last `Item` (its snapshot holds Datadog's priority, tags and groups), and how old the last detail read is. It lives in memory, so a restart reads each item in full once, as ETags do today |
| `Identity` | The account: Jira `accountId`, Freshservice `agent_id` from the config, none on Datadog. `check` gives it once and again after a refusal, not on each poll. The loop passes it when it builds the adapter, and an adapter compares it to the assignee to answer `NotMine` |
| `MirrorRead` | `raw.json` bytes, issue fields, description as Markdown and as its source, comments, remote `updated` |
| `ConnectorError` | Closed: `Credential { which: Secret }` (`Token`, `AppKey`), `Forbidden`, `RateLimited { wait }`, `Deferred`, `InvalidFilter`, `NotFound`, `Status`, `Transport`, `Parse`. `Deferred`: the adapter skipped this `read` or `pull`, by the 50 reads or the low limit guard, and the instance goes on |

Each adapter has its own pure `classify(status, headers, body)`. They cannot share `http::classify`: Datadog answers 403 to a bad key, and the reset header has three meanings: seconds to wait on Datadog, an ISO 8601 time on Jira, a Unix time on GitHub. `RateLimitWait` already holds these cases and is reused.

**Capabilities** are constant per kind, and tell `loom` and the app what to offer:

| | Datadog | Jira | Freshservice |
| --- | --- | --- | --- |
| `pool` | no | yes | yes |
| `mirror` | no | yes | yes |

Forges report `pool: false`, `mirror: false`: their MR mirror is another thing. `writes` joins them with the Outbox (W1), as the [table of Connectors](connectors.md) lists. `Capabilities` and `ConnectorKind` are in `loom-api`, under C1, since `ConnectorStatus` holds them and `loom-api` cannot depend on `loom-connector`: that crate reaches it through `loom-forge`.

### What is extracted

| From | To | Change |
| --- | --- | --- |
| `loom_forge::client::http::Http` | A `Transport` below it: HTTPS only, base URL guard, timeout, body cap, redaction | `Http` wraps it with Bearer auth, same-host redirects and `classify`. No change of behaviour |
| `loom_forge::client::{Token, RateLimitWait}` | Used as they are | `loom-connector` depends on `loom-forge` |
| `loomd::forge::token::TokenResolver` | Used as it is | It keys on `TokenSource`, so two secrets of an instance work, and a stuck command holds only its own source |
| `next_poll` and `PollOutcome` in `review_sync`, with `schedule::{backoff, jittered}` | `loomd::poll`, with a small `PollState` (interval, next, failures, rate limit, last success) | `reviews.rs` imports them. Its thread stays, and changes only after #218 merges |
| The byte encoding in `MrKey::dir_name` | A public function in `loom-forge`, extracted by C3 | Same bytes, lowercase, so one name on a file system that ignores case. So the mirror of `CORE-1042` is `jira-core-1042`, and `sync.json` keeps the key as the tracker writes it |

Not extracted: `Conditional<T>`, since no vendor here documents a conditional read, and the review poller itself.

**Connector transports follow no redirect** (`max_redirects(0)`), and a 3xx is a `Status` error. `RedirectAuthHeaders::SameHost` only protects `Authorization`: ureq strips that and `Cookie` on a cross-host redirect, so `DD-API-KEY` and `DD-APPLICATION-KEY` would follow it. Redaction covers each secret and the Base64 of `email:token` and `api_key:X`.

### Who owns what

| Piece | Shape | Task |
| --- | --- | --- |
| `Known`, `Identity`, `tracker()` | Above | C3 |
| `Capabilities`, `ConnectorKind`, `Secret` | In `loom-api`, with `ConnectorHealth`. `ConnectorKind`: `gitlab`, `github`, `datadog`, `jira`, `freshservice` | C1 |
| `TrackedItem` | `{ key, status, listed, noted }`, what a poll knows of an item with a task. C4 owns the struct. C6 and D2 supply the reader, from `sync.json` or the snapshot | C4, C6, D2 |
| Poll thread | Reads what it needs through `Reader` and files, as `Poller::keys` does: a `TrackedItem` for each item with a task. Calls `check` when it has no `Identity`, fills `Known`, decides what left a list | C4 |
| `ConnectorBatch` | `context`, `connector`, `kind`, `started`, `identity`, `list: Option<Listing>`, `pool: Option<Listing>`, `reads: Vec<(ItemRef, ItemRead)>`, `mirrors: Vec<(ItemRef, MirrorRead)>`. `None`: not asked or failed, which changes nothing | C4 owns the type, C5 applies it |
| `decide_ticket`, `decide_alert` | Pure, in `loomd::connector_sync`. They run on the task thread, in `tasks/connectors.rs`, as `tasks/reviews.rs` does, since only it may read a task and write in one step. It answers `ConnectorApplied`: `created`, `changed`, `failed`, `notify`. The poll thread calls `NotifyPort` with `notify`, after the batch | C5, C6, D2 |
| `state/connectors.json` | Written by one `ConnectorStatuses` in loomd, behind a mutex, by atomic rename, when an instance reports. Holds `last_attempt` and `last_success` per instance | C1 |
| Doctor | `loom connector doctor`: for each instance, secrets resolved and `check`; names tasks of an unconfigured connector (that clause: C5). `loom repo doctor` stays for forges | C4 |

## Connector instances

A connector belongs to a context, so the table sits under the context. Secrets are never shared across contexts, which is why there is no top-level table as there is for `[forges]`.

```toml
[contexts.company-a.connectors.datadog]
kind = "datadog"
site = "datadoghq.eu"
token_cmd = "op read op://company-a/datadog/api-key"
app_key_cmd = "op read op://company-a/datadog/app-key"
query = 'tag:"team:payments" env:prod'    # required; Datadog monitor search syntax, passed as written
states = ["alert", "warn"]                # default; "no data" can be added
priorities = [1, 2, 3]                    # absent: all, including none
urgent_priorities = [1, 2]                # default
flap_window = "30m"                       # default
poll_interval = "1m"                      # default for this kind

[contexts.company-a.connectors.jira]
kind = "jira"                             # Jira Cloud only, see Jira facts
site = "company-a.atlassian.net"
email = "me@company-a.example"
token_cmd = "op read op://company-a/jira/token"
list_jql = "assignee = currentUser() AND statusCategory != Done ORDER BY key"    # default
pool_jql = "project = OPS AND assignee is EMPTY AND statusCategory != Done ORDER BY created DESC, key"
poll_interval = "5m"                      # default for this kind
pool_interval = "15m"                     # default
pool_limit = 200                          # default

[contexts.company-a.connectors.freshservice]
kind = "freshservice"
site = "company-a.freshservice.com"
token_env = "LOOM_FRESHSERVICE_TOKEN"
agent_id = 12000000001
group_ids = [12000000002]                 # the pool; absent: no pool
workspace_id = 2                          # absent: the primary workspace only
open_statuses = [2, 3]                    # default
extra_query = "tag:'payments'"            # added to the built query with AND
```

- **Name**: the table key, `[a-z0-9_]+`, unique in its context. No `-`: in `<connector>-<key>`, the first `-` ends the name, and Jira keys hold one. I name it after the tool unless a context has two.
- **Rename**: the name is part of the identity of every item. `loom connector rename <old> <new>` runs on the task thread: it rewrites `origin.connector` and renames the mirror, snapshot and pool files, with one store commit. Until then, an instance creates nothing for an item whose key matches a task that is not archived, of an unconfigured connector of the same kind and host. The item counts as `Partial`, naming that connector. So a rename in the file alone duplicates nothing, closed tickets and dismissed alerts included, and removing one Datadog instance never stops another. A rename does not reach archived tasks: they are never changed, so an item still listed under an archived task of the old name counts as new, once.
- **Keys** are parsed per `kind`, and an unknown key is refused with its line, as for `[forges]`.
- **Two contexts on one site**: a token sees the same items in both, so an item would land in both stores. Each must then say what is its own: `list_jql` written out on Jira, `workspace_id` or `extra_query` on Freshservice. The default is refused at load. Datadog's `query` is always required.
- **Credentials**: `token_cmd` or `token_env`, with exactly the rules of [Credentials](reviews.md#credentials): `TokenSource`, `sh -c`, 10 s, trimmed, kept in memory, resolved again after a refusal. Datadog also needs `app_key_cmd` or `app_key_env`. Jira's `email` is no secret and stays in the file. `Config::token_env_names()` of #217 lists the connector names too, so git never inherits them.
- **`machines`**: the context's key, with no override per instance. The first machine polls, as `runner()` decides for reviews and schedules. With `machines` absent, every machine polls and loomd warns, since two machines would both create each task with its own random ID.
- **Overlap with `[forges]`**: none. `[forges]` and `[contexts.<ctx>.forges]` keep working unchanged, and a forge credential shows in status as an instance named after its host. A new config key does make an older `loomd` refuse the file (`deny_unknown_fields`), so all machines must be upgraded before a connector is added.
- **Removed instance**: its tasks stay and are no longer synced. Its pool entries are deleted at the next start.
- **API base**: Datadog `https://api.<site>`, Jira `https://<site>/rest/api/3`, Freshservice `https://<site>/api/v2`. `site` is a bare hostname: no scheme, port, path or user. It is checked against its kind's rule at load, so a token only goes to that host: Datadog's `site` must be one of the [published sites](#datadog-facts), Jira's must end in `.atlassian.net`, and Freshservice's must end in `.freshservice.com`.

## Poll loop

**Shared code, one thread per instance.** `loomd::connectors` has one generic loop, and each instance gets a thread running it.

- A review poll runs hosts in turn, so one slow host delays the others by up to 30 s per call. An urgent alert must not wait behind a ticket list, so the connector threads do not share. Instances are few, so threads cost little.
- Each poll: resolve the secrets, build the adapter, `list`, `pool` when due, `read` for what left a list, `pull` for what changed, then hand one batch to the task thread.
- **Single writer**: threads only read the store, through `Reader` and files. All writes go through `TaskHandle::sync_connector(batch)`, a command of the task thread, like `sync_reviews`. A file is written by atomic rename only when its bytes change, and a poll with no change commits nothing. One commit per poll per context.
- **Intervals**: `PollInterval` as for forges, minimum 1 min. Defaults: Datadog 1 min, Jira and Freshservice 5 min, pools 15 min. Jitter ±10 %, first poll 30 s after the start. `POST /v1/connectors/sync` wakes the threads and answers what each did.
- **Backoff**: `next_poll` as it is: interval × 2^failures, at most × 8. A rate limit waits for `max(wait, interval)` and is not a failure.
- **Rate limits**: 429 stops this instance only. Datadog: `X-RateLimit-Reset`, seconds. Jira: `Retry-After`. Freshservice: `Retry-After`. No wait given: 60 s. Before that, an adapter answers `Deferred` to optional reads (`read`, `pull`, the hourly monitor read) when Datadog or Freshservice `X-RateLimit-Remaining` is under 20 % of the limit, or Jira sends `X-RateLimit-NearLimit`. The Freshservice limit is shared by the whole account, including colleagues, so loom must not use it up. What was deferred waits for the next poll and counts as `Partial`. At most 50 such reads per poll: the loop orders the calls, a new ticket's `pull` first, and the adapter, built for one poll, counts them and answers `Deferred`.
- **A new firing monitor's read is required**, not optional: neither the guard nor the 50 reads skip it, since no task or notification exists without it. The Datadog adapter does it inside `list`, monitors with a group in `alert` before `warn`-only ones. A 429 there does not fail the list: `list` returns its items, the unread ones with `detail_read: false`, and the wait in `Listing`. The loop hands over the batch, health is `RateLimited { until }`, `Known` keeps what was read, and the next poll reads only the rest. If a firing monitor stays unread for 10 min, whatever the cause, the poll thread [notifies](#urgent) once per instance that alerts may be missing. It keeps that time in memory, and the notice re-arms when nothing is left unread.
- **Conditional reads**: none. No vendor page here documents one. The full filtered list on each poll is what shows an item leaving, so there is no cursor to keep, and comparing the remote `updated` decides what to read in full. Datadog's list has no `updated`; its groups are compared instead.
- **Failure**: one instance's failure touches only that instance. A failed or incomplete list changes no task and no entry. A failed `pool` leaves `list` applied, and the status says partial. A token refused: `invalidate`, and the next poll resolves it again.
- **Datadog 403**: it can be either key, or a permission, and two more calls tell. `GET /api/v1/validate` checks the API key: a 403 is `Credential { which: Token }`. Then `GET /api/v2/validate_keys` checks both: a 401 or 403 is `Credential { which: AppKey }`. If both pass, the 403 was a missing permission: `Forbidden`. A 429 or 5xx from either call is that error, and invalidates nothing. `TokenResolver::invalidate` then gets only that secret's `TokenSource`, and the adapter keeps the answer for that poll. Two `op` commands per poll would ask me to unlock each time.
- **Jira anonymous access**: the search and issue reads "can be accessed anonymously", so a dead token might give an empty success. The Jira adapter's `list` calls `GET /myself` first, which answers 401, and stops there on a failure. An empty list then means an empty list. This is not `check`, which the loop calls once.
- **Shutdown**: one shared stop flag, checked before each vendor call, and one shared `JOIN_DEADLINE` for all threads together. They stop with the review poller, before `task_store`, since they wait on it. A call still running is left behind: its batch has no task thread to go to.
- **Last successful sync**: the start time of the last poll whose `list` was complete. It is kept in `state/connectors.json`, machine-local, so it survives a restart and makes no store commit.

## From item to task

### Identity

The identity of an item is `(connector, remote_id or key)` inside one store.

- The name of the instance is in it, not the host. A Datadog site holds many organizations, so a monitor ID does not name a monitor by host alone. Two contexts on one Jira site are two instances and two stores.
- **Overlap**: two instances in one context with the same `kind` and `site` are refused at load, except Datadog, whose site says nothing of the organization. I widen the filter of the one instance instead.
- `origin` is the existing `ExternalRef` with two more optional keys. Older versions keep unknown link keys, so this stays compatible:

```toml
[[links]]
role = "origin"
provider = "jira"
connector = "jira"
key = "CORE-1042"
remote_id = "10042"            # Jira only: a moved issue changes its key, not its ID
url = "https://company-a.atlassian.net/browse/CORE-1042"
```

- Lookup covers `origin` and `related` links: an open task first, then the last closed active one, then the last archived one. The edges in the tables below follow `origin` only: a `related` link only prevents a new task. Several tasks of one monitor over time are normal: an alias then picks the open one, and asks for an ID when none or several are open.
- A task with a connector `origin` cannot move to another context, as for Review tasks.

### Where it goes

| What | Becomes | Shows on |
| --- | --- | --- |
| MR waiting on me | [Queue entry](reviews.md#waiting-on-me) | Reviews, "Waiting on you" |
| Ticket assigned to me | Task in `inbox` | Inbox |
| Ticket in the team pool | Queue entry | The pool view of the Inbox, not its count, not Today |
| Alert | Task in `inbox` | Inbox |
| Alert at an urgent priority | Task in `inbox` with `urgent = true` | Urgent, the strip, and a notification |

- **Why assigned tickets are tasks and pool tickets are not**: an assigned ticket is mine to do, which is what a task is. A pool ticket is not mine until I take it. It is the same reasoning as an MR waiting on me: browsing must not fill the Inbox or the ledger.
- **`urgent`**: a new `task.md` key. Today's Urgent section holds the tasks with it in `inbox` or `ready`. `active` moves one to In progress, `waiting` keeps it on the strip only, and closing ends it. It stays visible in the Inbox, as [Today](user-workflow.md#today) asks.
- **Kind**: Datadog `investigation`. Freshservice `Incident` is `investigation` and `Service Request` is `support`. Jira `Bug` is `bug`, and any other type is `story`.
- **Importance**: the kind's default, except Datadog priority 1 is p0, 2 is p1, 3 is p2, 4 and 5 are p3, and Freshservice priority 4 is p0, 3 is p1, 2 is p2, 1 is p3. Only Datadog's `urgent_priorities` routes to Urgent, and a monitor with no priority is not urgent.
- **Due**: Jira `duedate`, Freshservice `due_by`, at creation.

### Urgent

The app comes later, so the read-only phase has to reach me without it.

- **`loom ls`** lists an urgent task in `inbox` or `ready`, ahead of every other rule (`is_listed` and `compare` in `loom-core`).
- **`NotifyPort`** in loomd: `fn notify(&self, notification: &Notification) -> Result<(), NotifyError>`, with context and title. The first adapter uses the OS, with text only as arguments: `osascript -e '<fixed script reading argv>' -- <context> <title>` on macOS, and `notify-send -- <summary> <body>` on Linux, with `& < >` escaped since the body can carry markup. Without `--`, a title that starts with `-` is read as an option and the notification is lost. Text is capped at 200 characters, and the process at 5 s, so a stuck one cannot hold a poll thread. The poll thread calls it. The app takes over later behind the same port. A failure is logged and never stops a sync.
- **It fires on**: a new urgent task, `urgent` being raised, an open task at an urgent priority with a new episode (see the table below), a firing monitor unread for 10 min, and a refused credential or permission ([status](#connector-status)). Once each. A flap inside the window does not notify.

### What the sync may change

- **At creation only**: title, kind, `origin`, importance, due, `urgent`.
- **Afterwards**: the status on the edges below, each preceded by a daemon note giving the reason; `urgent` raised when a group of an urgent monitor becomes `alert`, once per task; `origin.key` and `url` when a Jira issue moved.
- **Always mine**: importance, due, pinned, notes, title, relations, `urgent` once set. An archived task is never changed, not even by a rename.
- **Mirrors and snapshots** follow the remote freely, except as below. A remote title change updates the mirror, not the task.

### Tickets: closing and reopening

`list` selects what I own and that is open. An item that leaves a complete list is read by key, and only that read decides, because a Jira search can lag behind a change. It is read at once, then at most hourly while the answer is "open and mine", within the 50 reads.

A new task needs its `pull`: without it, the item creates nothing, counts as `Partial` and waits for the next poll. `listed` is kept in the mirror's `sync.json`. It becomes true when the item is in the list and open. It becomes false only on a read that says terminal or not mine, or when the list itself shows it terminal, so a lagging search changes nothing. The rules are edges, as for reviews, in a pure `decide_ticket`.

| Task | Seen | Action |
| --- | --- | --- |
| None | In the list | New task in `inbox`, with its mirror |
| Last one archived, its frozen `listed` false, or the remote changed after archiving: `updated` newer than its frozen `sync.json` | In the list | New task in `inbox`, with its mirror |
| Last one archived, its frozen `listed` true, no newer remote `updated` | In the list | Nothing: a ticket I closed by hand does not come back when its task is archived |
| Open | In the list | Mirror follows when remote `updated` moved |
| Open | In the list or read: terminal | `done`, with a note. `listed` false |
| Open or closed | Read: still open and mine | Nothing. `listed` stays true |
| In `inbox` | Read: not mine | `cancelled`, with a note naming the new assignee |
| `ready`, `active` or `waiting` | Read: not mine | A note, once. The task stays |
| Open | Read: 404 | A note, once, and no more reads until it is in a list again. The task stays. It counts as failed in status until the note is written: a 404 can be a lost access |
| `done` or `cancelled` | Read: terminal, not mine or 404 | `listed` false. Nothing else, and no more reads |
| `done` or `cancelled` | In the list and open, `listed` false at the last poll | Back to `ready`, with a note, as reviews do and as `is_legal_transition` requires |
| `done` or `cancelled` | In the list and open, `listed` true at the last poll | Closing or dismissing sticks. When the remote `updated` moved, the mirror and `updated` in `sync.json` follow it, one commit per real change, so only a change after archiving counts above |

`sync.json` and the snapshot hold `noted`, a closed set (`not_mine`, `gone`, `recovered`, and on an alert `urgent_raised`), so "a note, once" holds across restarts. It clears when the item is in a list again, or fires again, except `urgent_raised`, set too when a task is created urgent: it stays with the task, so a flap never undoes my clearing `urgent`.

Terminal: Jira `status.statusCategory.key` is `done`. Freshservice `status` is 4 (Resolved) or 5 (Closed), or the ticket is `deleted` or `spam`.

### Alerts: grouped by monitor

One task per monitor episode, not per group. The snapshot is `tasks/<id>/remote/<connector>-<monitor id>.json`, daemon-owned: monitor ID, name, priority, tags, web URL, per group its status and `last_triggered_ts`, `quiet_since` and `noted`.

- **List**: `GET /api/v1/monitor/groups/search` with `query` and `group_status`, every page, grouped by `monitor_id`. It returns no priority, so `GET /api/v1/monitor/{id}` gives it once per new monitor, and again each hour. `priorities` is applied locally.
- **Quiet**: a monitor with no group in the list is read with `group_states=all`. It is quiet only when every group in that read is `OK`. Anything else, such as an alert that became `No Data`, means no action, and it is read again after `flap_window`. `quiet_since` is the newest `state.groups[].last_resolved_ts`, or the poll that first saw it quiet, since the group search has no such field. It is read again when the window ends, and a firing clears it.
- **A new task needs its priority**: a monitor whose [required read](#poll-loop) failed creates nothing. Its health follows [the mapping](#connector-status): a 429 `RateLimited`, `Forbidden`, `Status` or `Parse` `Partial`, and `Credential` or `Transport` stop the poll. Otherwise an unread priority 1 monitor would stay a task that is not urgent.
- **A firing** is a group whose `last_triggered_ts` is newer than the snapshot's, a group the snapshot does not hold, or a group that was `warn` in the snapshot and is `alert` now, so a dismissed warning cannot swallow an escalation. `last_triggered_ts` is an `Option<i64>`: without one, only the other two count. This works from a timestamp rather than from what two polls saw, so it holds across a restart and catches a flap between polls.

Pure `decide_alert`:

| Task | Seen | Action |
| --- | --- | --- |
| None, or the last one archived and the firing is newer than its snapshot | A firing | New task. `urgent` when the state is alert and the priority is in `urgent_priorities` |
| Open | A firing | Same task, one row: the snapshot follows, and the row counts firings and groups. No event per flap |
| Open, not urgent, no `urgent_raised` | A group in `alert` that was not `alert` in the snapshot, at an urgent priority | `urgent` raised, and it [notifies](#urgent) |
| Open, at an urgent priority | A group in `alert`, with `quiet_since` older than `flap_window` | Same task. It notifies once and leaves `urgent` as I set it: clearing it does not mute a new episode, and a flap does not notify |
| `inbox`, not urgent | Quiet for `flap_window`, confirmed by one read | `cancelled`, with a note: it cleared itself. Not `done`, since an Investigation is done when its cause is identified |
| `inbox`, urgent | Same | Stays, and the row says it recovered: I should see that a p0 happened. A note, once |
| `ready`, `active` or `waiting` | Same | A note, once: the cause is still mine to find |
| Open | Monitor read: 404 | A note, once. `inbox` and not urgent: `cancelled`. Urgent stays visible |
| `done` or `cancelled` | A firing newer than the snapshot | New task: "a monitor that fires again comes back as a new row" |
| `done` or `cancelled` | Still firing, and no firing as defined above | Nothing, and the snapshot is left unchanged: dismissing holds while it stays down |

Dismissing is `d` on the row, which cancels the task and clears every group on it. The snapshot keeps the timestamps the next firing is compared with.

### Team pool entries

- **Files**: `contexts/<ctx>/queue/pool/<connector>-<key>.json`, daemon-owned, deleted when the entry leaves, written only when bytes change, as for MR entries. It holds `v`, connector, key, `remote_id`, title, web URL, priority, group, remote `updated`. It holds no countdown or other field that moves by itself.
- **Failures**: a failed or incomplete list changes no entry.
- **`pool_limit`**: the read stops at the limit and stays complete for the pool: entries become those first N in the order of the query, others are removed, and status says `truncated`.
- **No double row**: an item with a task in any status that is not archived has no entry, the rule #218 proposes for MR entries.
- **Taking one**: in the read-only tasks, `o` opens it at the source. Once assigned there, the next poll moves it to a task and removes the entry in the same batch. With writes, `y` assigns it with the hold, as [Keys](user-workflow.md#keys) says.

## Mirrors

Jira and Freshservice mirror under `tasks/<id>/issues/<connector>-<key>/`, in the [layout of Tracker sync](tracker-sync.md#mirror-layout). The directory takes its name at creation and keeps it when a Jira key moves. Datadog has no mirror.

So that read-only does not paint the two-way sync into a corner:

- **The files already exist as the two-way sync needs them.** `issue.md`, `comments.md`, `.base/issue.md` and `raw.json` have their final format. `drafts/` is not created.
- **Pull writes `issue.md` and `.base/issue.md` together** when `issue.md` equals `.base/issue.md`. When I have edited `issue.md`, the pull leaves both alone and only updates `raw.json` and `sync.json`. `.base` is then still the true common ancestor, so the later merge has all three sides, and nothing of mine is lost. Status says the mirror has local changes. Read-only tasks never merge and never push.
- **`raw.json`** is the last full payload, written only when the remote `updated` moved. Jira's `*all` fields can include what changes when someone merely views an issue, so bytes alone would commit noise.
- **`.base/description.<format>`** keeps the source of the description at base: `adf.json` on Jira, `html` on Freshservice. Unsupported ADF nodes become placeholders that name a node in that file, so a later push can restore them. The Markdown is never the only copy.
- **`sync.json`**: `v`, `key`, `remote_id`, remote `updated`, `listed`, `noted`, `assignee_id`, and `converter`, the version of the Markdown conversion. A new converter renders `.base` and the remote with the same version before comparing, so a loom upgrade alone never looks like an edit.
- **IDs**: `assignee_id` is the account or agent ID of the name in `issue.md`. Status and transition IDs are read from `raw.json`.
- **Comments**: read when the remote `updated` moved (Jira `GET /issue/{key}/comment`, Freshservice `GET /tickets/{id}/conversations`).
- **The changelog is not read.** The sync needs the current state and its edges, not the history. Conflict messages for pushes can read it later.

## Linking an alert and an incident (later)

Nothing is built now. Three rules keep it open:

- A task can hold several snapshots and mirrors, one per item, each named by its connector and key.
- Lookup covers `related` links, so an item attached to another task never makes a new one, and its edges do not act on that task.
- Freshservice's `raw.json` keeps `custom_fields` and the description, so the later linker can find a monitor ID in tickets already mirrored, with no new fetch.

The later task adds the match, which turns the second origin into a `related` link and closes the duplicate task. The tenant's field that carries the monitor is a per-instance setting then, since the docs define no such field.

## Connector status

Every adapter and forge credential reports this shape, in `loom-api`, with `serde` and `JsonSchema`:

```rust
pub struct ConnectorStatus {
    pub context: ContextName,
    pub name: String,                // the instance; a forge: its host
    pub kind: ConnectorKind,         // gitlab | github | datadog | jira | freshservice
    pub host: String,
    pub polled_by: PolledBy,         // this_machine | other { machine } | every_machine
    pub health: ConnectorHealth,
    pub stale: bool,                 // pure: no success in 3 intervals, or never
    pub last_attempt: Option<Timestamp>,
    pub last_success: Option<Timestamp>,
    pub next_poll: Option<Timestamp>,
    pub items: ConnectorCounts,      // listed, pool, failed, truncated, local_changes
    pub capabilities: Capabilities,
}

pub enum ConnectorHealth {
    NotPolledYet,
    Ok,
    Partial { failed: Vec<ConnectorFailure> },    // key and reason; the rest applied
    RateLimited { until: Timestamp },
    Credential { which: Secret, error: String },  // never the token
    Forbidden { error: String },                  // the secrets are good, a permission is missing
    InvalidFilter { error: String },              // bad JQL or query: fix the config, retry won't help
    Unreachable { error: String, failures: u32 },
}
```

- **Read**: `GET /v1/connectors`, `loom connector status [--json]`. A change sends a change event, for the app. With #218, an empty `loom review queue` cannot tell a failed poll from nothing to do: it will read this.
- **Another machine**: `polled_by: other` makes no claim on health. This machine does not know.
- **Forges**: `ReviewSync` maps onto it. `Synced` with `failed` or `failed_lists` is `Partial`, `RateLimited` as it is, and a `Failed` from a 401 is `Credential`. An entry of `failed_lists` with `kind: forbidden`, on each poll, can give `Forbidden`: a token that cannot read its to-dos. Connector status has to retain `failed_lists`: only the answer of `POST /v1/reviews/sync` holds it today, and a scheduled poll logs it and drops it.
- **From `ConnectorError`**, on `list`: `Credential`, `Forbidden`, `RateLimited` and `InvalidFilter` give the health of that name; `NotFound`, `Status`, `Transport` and `Parse` give `Unreachable`. Each stops the poll. `check` is the same. On `pool`: `Credential` and `RateLimited` are the same, and any other error is `Partial` with `list` applied. On one item's `read` or `pull`: `Credential`, `RateLimited` and `Transport` stop the poll the same way; `NotFound` is `ItemRead::Gone`; `Forbidden`, `Deferred`, `Status` and `Parse` are `Partial`, and the poll goes on.
- **Notifications**: `Credential` or `Forbidden` on `list`, twice in a row, raises "expired token" with the reason, counted in memory by the poll thread. The second try is there because a refusal first resolves the token again. A dead key or a revoked permission would otherwise mean no alerts and no signal.

## Fixtures and tests

- **Fixtures**: `fixtures/connectors/{datadog,jira,freshservice}/` with a `README.md` as in `fixtures/forge/`: request, status, headers, file, and the doc each shape comes from. Composed from the published specs, never captured. Users `alice`, `bob`, `carol`. Contexts and hosts `company-a.example`, `company-a.atlassian.net`, `company-a.freshservice.com`. Projects `CORE`, `OPS`. No real name, no key-shaped string.
- **Server**: the one fake server, `crates/loom-forge/tests/common/fake_forge.rs`, included by `#[path]` as today. `Route::fixture` learns the connector root. `LOOM_TEST_CONNECTOR_API`, debug builds only like `LOOM_TEST_FORGE_API`, points an instance at it as `<context>/<name>=<base>`.
- **Pure, test first**: `classify`, query builders, payload parsing, `decide_ticket`, `decide_alert`, ADF and HTML to Markdown, config validation, each as table tests.
- **Every adapter passes**: nominal list and pages; a poll with no change writes and commits nothing; a failed list and an incomplete list change nothing; 429 with that vendor's headers; a refused token resolved again; an item leaving is read once; a restart reads in full and writes nothing; no secret in any log, error or fixture; another instance keeps polling while this one fails.
- **Loop**: tested with an in-memory `IngestPort`, so the adapters run in parallel without waiting on each other.

## Security

- **What leaves my machine**: the whole store goes to `store.backup.remote` in plaintext, so with connectors that means alert names, tickets with requesters' words, comments, `raw.json` and pool entries. Encryption is a target of [Storage](storage.md) that is not built, and `store.backup.encrypt` does not exist today.
- **Backup rule**: a connector is refused at load on a context with a backup remote until `store.backup.allows_client_data = true` says I checked that the client allows this remote. No remote: no question. Once built, encryption is the other way to satisfy this rule.
- **Connector text is untrusted**: tickets and comments are written by anyone. Scripts and remote loads are stripped when HTML becomes Markdown, as the [app](architecture.md#desktop-app) requires of all such content. Control characters are stripped from CLI output and notifications. Text loaded into an agent follows the same rule as any source.
- **URLs and paths**: a web URL is built from `site` and the key, never taken from a payload. A key from a payload is encoded before it names a file, an API path or a web URL, so it cannot leave its directory or its path.
- **Secrets**: as in [Credentials](reviews.md#credentials), never logged, never given to git or to an agent. Their names join `token_env_names`.

## Real checks

I run these once per adapter, with my token, and record the result in its PR. They are what the docs leave open. Each is written so that the adapter has a safe behaviour until then.

**Shape checks come first.** Those that can change the code (**bold** below) are in `fixtures/connectors/check.sh`, which I run before D2, J3 and F2 (tasks D0, J0, F0). The rest are checked at the end of each adapter.

The script reads only, and I must be able to share all it prints:

- **Requests**: GET, plus the Jira search `POST`, which reads. `curl -q --proto '=https' --max-redirs 0`. `site` follows the [rule of its kind](#connector-instances). Tokens come from the environment, through `curl -K -`, never as an argument. The JQL body is in a file or an argument.
- **Prints**: statuses; rate limit header names only; counts; timestamps; a fixed allowlist of JSON paths, with map keys as `<key>`, since the keys of `MonitorState.groups` are hosts and tags and those of `custom_fields` are tenant fields; a closed list of enum values (`type`, `status`, ADF node `type`), which answers Freshservice 1 and 2 and Jira 6; and a salted hash for a group's identity between runs, with the salt in a local file.
- **Never prints**: the query, a URL, an error body, a name, title, tag or text.
- **Jira 8**: the wrong token gets one attempt only, since a triggered CAPTCHA would lock the REST API out.

### Datadog

1. **Does `groups/search` accept** `tag:"team:x"`, `team:x`, `env:` and `muted:false`? The [search page](https://docs.datadoghq.com/monitors/manage/search/) lists no `team` or `priority` attribute, and [Teams](https://docs.datadoghq.com/account_management/teams/) says to read the search bar for the terms it adds.
2. **`group_status:(alert OR warn)`**: is the form right, and is `priority:` usable to save the detail reads?
3. **Does `last_triggered_ts` move** on each re-trigger and on a warn that becomes an alert, in which unit, and does it hold on a monitor with no groups? Run twice, around a re-trigger. The whole firing rule depends on it. If it does not move, the fallback is a group seen absent, then present, across polls.
4. The largest `per_page`, and the rate limit bucket of `groups/search` and `monitor/{id}`: the docs give neither.
5. The web URL of a monitor on each site, and whether a custom subdomain needs a `web_url` key.
6. An application key limited to `monitors_read`: is it enough for both reads?
7. Is a monitor ID unique in one organization only? The search result carries `org_id`, and the docs do not say.

### Jira

1. **`assignee is EMPTY` and `statusCategory != Done`**: the JQL reference is not on `developer.atlassian.com`, the REST spec only shows an `empty` operand, and only that domain was read. Team fields (`"Team[Team]"`) are unverified too, so `pool_jql` is mine to write.
2. Does a classic API token keep working against `https://<site>`? The page does not cover scoped tokens or the `api.atlassian.com` gateway.
3. Which limits an API token really meets. The page says token traffic stays on burst limits (100 GET per second) and does not say the hourly points quota never applies to it.
4. The largest useful `maxResults` for the chosen `fields`, and whether `warnings` on a truncated search needs `complete = false` (the field is marked experimental).
5. Is `lastViewed` in `*all` the only field that moves without `updated`? Does a new comment move `updated`?
6. **Whether `description` in a v3 response is always ADF**: the structure page names comments and textarea custom fields, and the v3 spec has `fields` as an open map.
7. Do `read` on a moved key and the search return the new key with the same `id`, as the issue page says?
8. **What an expired or wrong token returns** on `/myself` and on the search: the auth page does not say, and the search can be read anonymously.

### Freshservice

1. **`type`**: the attribute table says it supports only `incident`, while the list filter shows `type=Service+Request`. Are requests returned, with which value?
2. **Custom statuses**: the docs list 2 to 5 only. If the tenant has more, `open_statuses` must hold them.
3. **Does `agent_id:null AND group_id:<id>` work** as the pool? Does the list cap pages? The page gives 30 per page and warns against pages over 500, and no cap.
4. There is no documented "me" endpoint, so `agent_id` is configured: can my role read `/agents` to help me find it, or must I read it from the UI?
5. Does a new conversation move `updated_at`?
6. Does the filter accept a full time on `updated_at`? The notes say `yyyy-mm-dd` and the table says a time. The design does not rely on it.
7. `workspace_id`: a tenant with workspaces returns only the primary one by default, so the wrong value looks like an empty queue. Check that the status shows items.
8. Does a monitor ID or link really appear on incidents, and in which field? The [connectors page](connectors.md) says it does, and no doc can confirm a tenant's setup.

## Vendor facts

Only what an adapter needs, each from the pages under [Sources](#sources).

### Datadog facts

- **Auth**: reads need both `DD-API-KEY` and `DD-APPLICATION-KEY`. `GET /api/v1/validate` checks only the API key and answers 403 when invalid. `GET /api/v2/validate_keys` checks both and answers 401 or 403 otherwise, but its `x-permission` is `OPEN`, so it says nothing of `monitors_read`: `check` runs it, then one `groups/search` with a small `per_page`. The monitor endpoints declare the `monitors_read` permission.
- **Sites**: `datadoghq.com`, `us3.datadoghq.com`, `us5.datadoghq.com`, `ap1.datadoghq.com`, `ap2.datadoghq.com`, `uk1.datadoghq.com`, `datadoghq.eu`, `ddog-gov.com`, `us2.ddog-gov.com`. API host `api.<site>`. "Each site is completely independent."
- **Monitors**: `priority` is an integer from 1 (high) to 5 (low), so there is no P0. Only the full monitor has it: `MonitorSearchResult` and `MonitorGroupSearchResult` do not. States: `Alert`, `Ignored`, `No Data`, `OK`, `Skipped`, `Unknown`, `Warn`.
- **Groups**: `GET /api/v1/monitor/groups/search` gives `monitor_id`, `monitor_name`, `group`, `group_tags`, `status`, `last_triggered_ts`, `last_nodata_ts`. `page` starts at 0, `per_page` defaults to 30, and `metadata` gives `page_count`.
- **Team**: a handle shows as `team:<handle>`, and a handle without a defined team acts like a tag.
- **Limits**: 429, with `X-RateLimit-Limit`, `-Period`, `-Remaining`, `-Reset` (seconds until reset), `-Name`. They vary by endpoint, with no numbers published.
- **Events**: `GET /api/v2/events` exists with cursors. Not used: groups give what the rules need. Its docs warn that the `aggregation_key` now includes the group and recommend `@monitor.id`.
- **Mutes, later**: `POST /api/v2/downtime`, since v1 downtime is deprecated. `POST /api/v1/monitor/{id}/mute` still exists.

### Jira facts

**Target: Jira Cloud, REST v3.** Data Center is out: it has wiki markup rather than ADF ([Tracker sync](tracker-sync.md#provider-specifics)) and its own API reference. Its reference pages gave nothing readable when this was written, so nothing else about it is stated here. This is an assumption I have to confirm for each client. A Data Center tenant would be its own `kind`, reusing `decide_ticket` and the mirror.

- **Auth**: `Authorization: Basic` with `email:token` in Base64, on `https://<site>`, as `site` is the full host. A triggered CAPTCHA shows as `X-Seraph-LoginReason: AUTHENTICATION_DENIED`, and is reported as `Credential`.
- **Search**: `POST /rest/api/3/search/jql`. `/rest/api/3/search` is being removed. `fields` defaults to `id`, so I always send the list. Pages use `nextPageToken`, which is absent on the last page. There is no total. `jql` must be bounded. `orderBy` takes at most 7 fields. `maxResults` says "It returns max 5000 issues", and fewer per page as more fields are asked.
- **Consistency**: "Recent updates might not be immediately visible" in search, hence the read by key. `reconcileIssues` takes at most 50 IDs and is not used.
- **Issue**: `GET /rest/api/3/issue/{key}` finds moved issues and returns the key it found. `id` and `key` are both returned. Comments: `GET .../comment`, with `startAt`. Later: `POST .../transitions`, `PUT .../assignee`, `PUT .../{key}` with `notifyUsers`.
- **Changelog**: `GET .../changelog` with `startAt`, and `POST /changelog/bulkfetch` for up to 1,000 issues. Not read now.
- **Limits**: 429 with `Retry-After`, `RateLimit-Reason`, `X-RateLimit-NearLimit` under 20 %, and `X-RateLimit-Reset` as an ISO 8601 time, on 429 only. GET bursts of 100 per second. "API token-based traffic is not affected by this change, and will continue to be governed by existing burst rate limits."
- **ADF**: a JSON `doc` with `version` and `content`, block nodes, inline nodes (including `mention`, `status`, `inlineCard`, `mediaInline`) and marks. The page says nothing on unknown nodes, so a converter treats any unknown type as a placeholder. No document may fail on one.
- **Conditional reads**: the spec never mentions `ETag` or `If-None-Match`.

### Freshservice facts

- **Auth**: Basic with the API key as user and any password: `api_key:X`. "supports Basic Access Authorization only with API key." What a key can see depends on the agent's permissions.
- **Base**: `https://<domain>.freshservice.com/api/v2/`.
- **Filter**: `GET /tickets/filter?query="..."`, URL encoded, at most 512 characters, 30 per page, with the total and `page`. Fields include `agent_id`, `group_id`, `status`, `priority`, `tag`, `updated_at`. `null` selects no agent or group. Only the primary workspace unless `workspace_id` is sent. 0 means all workspaces with global fields only.
- **List**: `GET /tickets` returns the last 30 days by default, which is why it is not used.
- **Ticket**: `GET /tickets/{id}`, with `responder_id`, `group_id`, `status` (2 Open, 3 Pending, 4 Resolved, 5 Closed), `priority` (1 Low to 4 Urgent), `type`, `updated_at`, `due_by`, `deleted`, `spam`, `custom_fields`, `description` (HTML), `description_text`.
- **Conversations**: `GET /tickets/{id}/conversations`. Later: `POST /tickets/{id}/reply`, `PUT /tickets/{id}`.
- **Limits**: per minute, account-wide, even invalid requests. Overall 100 to 500 by plan, ticket lists 40 to 140, and a view 50 to 160. 429 with `Retry-After`. `X-RateLimit-Total`, `-Remaining`, `-Used-CurrentRequest`, also `X-RateLimit-Account-*` and `-Sublimit-*`. `include` costs extra credits, so it is not used.
- **Errors**: 401 for the auth header, 403 when the agent may not make the call.
- **Conditional reads**: none in the docs.

### Sources

- Datadog: [Monitors API](https://docs.datadoghq.com/api/latest/monitors/) and the spec it is generated from ([v1](https://docs.datadoghq.com/resources/json/full_spec_v1.json), [v2](https://docs.datadoghq.com/resources/json/full_spec_v2.json)), [Search monitors](https://docs.datadoghq.com/monitors/manage/search/), [Events API](https://docs.datadoghq.com/api/latest/events/), [Rate limits](https://docs.datadoghq.com/api/latest/rate-limits/), [Authentication](https://docs.datadoghq.com/api/latest/authentication/), [Sites](https://docs.datadoghq.com/getting_started/site/), [Teams](https://docs.datadoghq.com/account_management/teams/), [Monitor configuration](https://docs.datadoghq.com/monitors/configuration/).
- Jira: [REST v3 spec](https://developer.atlassian.com/cloud/jira/platform/swagger-v3.v3.json), [Issue search](https://developer.atlassian.com/cloud/jira/platform/rest/v3/api-group-issue-search/), [Issues](https://developer.atlassian.com/cloud/jira/platform/rest/v3/api-group-issues/), [Rate limiting](https://developer.atlassian.com/cloud/jira/platform/rate-limiting/), [Basic auth](https://developer.atlassian.com/cloud/jira/platform/basic-auth-for-rest-apis/), [ADF structure](https://developer.atlassian.com/cloud/jira/platform/apis/document/structure/), [Data Center REST](https://developer.atlassian.com/server/jira/platform/rest/).
- Freshservice: [API reference](https://api.freshservice.com/), sections Authentication, Rate Limit, Pagination, Filter Tickets, View List of Tickets, Ticket attributes, Conversations, Error handling.
