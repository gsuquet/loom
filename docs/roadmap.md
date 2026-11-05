# Roadmap

> Versions, content and acceptance criteria. Only the next version is broken down into tasks.

Versions up to v0.3 are released. The backend of v0.4 goes on; the TUI is removed ([#215](https://github.com/gsuquet/loom/issues/215)), and its open issues are dropped. The delivery order from here replaces the later versions:

| Step | Content | Done when |
| --- | --- | --- |
| 1. Desk | Desktop app, Today, Inbox, task page, palette, global hotkey, MR queue, connector status, GitLab | For one working week, I start the day in loom and do not open the forge to find MRs |
| 2. References | Sources, Reading, phone capture, knowledge pages, glossary, out-of-scope records, pruning agent, the rule on text loaded into agents | Set when the step starts |
| 3. Connectors | Datadog, Jira, Freshservice: read-only first, then writes behind the Outbox | Read-only: for one working week, with no app yet, `loom ls` and an OS notification bring me each urgent alert, and I do not open Datadog, Jira or Freshservice to find my alerts and tickets. Writes: set when they start |
| 4. Review | Diff, reviewer agent, multi-repo worktrees, stage scope, profiles, language detection, run leases | Set when the step starts |
| 5. Later | Other specialists, pipelines, opt-in rules, read-only Kubernetes, GitHub, Google Calendar | Set per item when it starts |

Multi-repo tasks, the Source entity, the conception kind, links, pipelines and the adapter ports change the core model. They are designed in the docs before step 1. The toolkit of the app is chosen before step 1. Spikes: the toolkit, the Android share target and Worker authentication, and local transcription after step 2.

**The next tasks, in this order:**

1. [#212](https://github.com/gsuquet/loom/issues/212): the GitLab queue reasons.
2. Connector status (C1 below).
3. The design of the connector ports: [Connector ports](design/connector-ports.md).
4. Datadog, Jira and Freshservice, read-only (C2 to F3 below).
5. [#186](https://github.com/gsuquet/loom/issues/186) and [#187](https://github.com/gsuquet/loom/issues/187).

So the read-only part of step 3 comes ahead of the Review work of step 4, for these tasks only. The rest of each step keeps its place: the connector writes stay behind the Outbox, and the app waits for its toolkit.

The central instance and unattended delivery stay later options with no date. Unattended delivery needs my go-ahead, see [Autonomy policy](design/agents.md#autonomy-policy).

## Step 1 tasks

Tasks so far, from the gaps found against [Waiting on me](design/reviews.md#waiting-on-me). The app itself is broken down once the toolkit is chosen. The v0.4 tasks below on diffs, drafts and publish (#185 to #189) belong to step 4, and #186 and #187 follow the read-only connectors.

| Issue | Task | Kind | Depends on |
| --- | --- | --- | --- |
| [#211](https://github.com/gsuquet/loom/issues/211) | Queue entries: an MR has no task until I start on it, `loom review queue`, `loom review start` | Story | This design |
| [#212](https://github.com/gsuquet/loom/issues/212) | GitLab reasons: assigned, mentions, my own MRs with new comments, a failed pipeline or approval to merge | Story | #211 |

## Connector tasks

Design: [Connector ports](design/connector-ports.md), [Connectors](design/connectors.md), [Tracker sync](design/tracker-sync.md). Each task becomes a GitHub issue when it starts. Tasks with the owner "Me" need my token on a real tenant and are the [real checks](design/connector-ports.md#real-checks).

### Read-only

| # | Task | Kind | Depends on | Owner |
| --- | --- | --- | --- | --- |
| C1 | Connector status: `ConnectorStatus` in `loom-api`, `GET /v1/connectors`, `loom connector status`, `state/connectors.json`, change event, forges report into it | Story | #218 | Agent |
| C2 | Config: `[contexts.<ctx>.connectors.<name>]` per kind, validation, API base rules, `token_env_names`, reference page | Story | This design, #217 | Agent |
| C3 | `loom-connector`: ports, types, `ConnectorError`, capabilities, stub modules for the three adapters; `Transport` out of `Http`; fake server root; `fixtures/connectors/`; crate row | Story | This design | Agent |
| C4 | Poll loop: `loomd::poll` out of `review_sync`, one thread per instance, `sync_connector` on the task thread, `POST /v1/connectors/sync`, `LOOM_TEST_CONNECTOR_API`; tested with an in-memory `IngestPort` | Story | C1, C2, C3, #218 | Agent |
| C5 | Items: `origin` keys `connector` and `remote_id`, lookup, `ConnectorBatch` and `tasks/connectors.rs`, `loom connector rename`, `UnconfiguredOrigin`; no move of a connector task | Story | C4 | Agent |
| C6 | Tickets: `decide_ticket`, mirror writer with `.base` and `sync.json`, `queue/pool/` entries, `loom connector pool` | Story | C5 | Agent |
| C7 | `fixtures/connectors/check.sh`: the read-only shape checks | Chore | C3 | Agent |
| D1 | Datadog client and parsing: group search, monitor read, `classify`, sites, `check`, fixtures | Story | C3 | Agent |
| D0 | Datadog shape checks: run `check.sh`, adjust the design | Chore | C7 | Me |
| D2 | Datadog sync: `decide_alert`, snapshots in `remote/`, `urgent` in `task.md`, priority mapping | Story | C5, D0, D1 | Agent |
| D2b | Reaching me: urgent first in `loom ls`, `NotifyPort` and its OS adapter | Story | D2 | Agent |
| D3 | Datadog real check | Chore | D2b | Me + agent |
| J1 | Jira client and parsing: `search/jql`, issue, comments, `classify`, `check`, fixtures | Story | C3 | Agent |
| J2 | ADF to Markdown (pure): placeholders, unknown nodes, `converter` version | Story | C3 | Agent |
| J0 | Jira shape checks: run `check.sh`, adjust the design | Chore | C7 | Me |
| J3 | Jira sync: list, pool, mirror, moved keys | Story | C6, J0, J1, J2 | Agent |
| J4 | Jira real check | Chore | J3 | Me + agent |
| F1 | Freshservice client and parsing: filter, ticket, conversations, `classify`, `check`, fixtures | Story | C3 | Agent |
| F0 | Freshservice shape checks: run `check.sh`, adjust the design | Chore | C7 | Me |
| F2 | Freshservice HTML to Markdown (pure), and the sync: list, pool, mirror | Story | C6, F0, F1 | Agent |
| F3 | Freshservice real check | Chore | F2 | Me + agent |

Order: C1 to C5, with C2 and C3 side by side. Then three parallel tracks, each in its own module and fixture directory: {D0, D1 -> D2 -> D2b -> D3}, {C6; J0, J1, J2 -> J3 -> J4}, {C6; F0, F1 -> F2 -> F3}. D1, J1, J2 and F1 can start as soon as C3 is merged. Datadog does not wait for C6.

### Writes (later)

| # | Task | Kind | Depends on | Owner |
| --- | --- | --- | --- | --- |
| W1 | Outbox: Held, Sent, Not sent in loomd, machine-local, send time, wake rule, Sent record, `loom outbox`; nothing in the code holds a write today | Story | C4 | Agent |
| W2 | Push pipeline (pure + API): merge `issue.md`, `.base` and the remote, changed fields only, diff preview, conflict to the Inbox, `loom push` | Story | W1, J3 or F2 | Agent |
| W3 | Jira writes: `push` on `TrackerPort`, transitions, edit, comment, assign, Markdown to ADF | Story | W2 | Agent |
| W4 | Freshservice writes: reply, status, assign | Story | W2 | Agent |
| W5 | Datadog mute: confirmed, with an end time | Story | W1, D2 | Agent |
| W6 | Link a Datadog alert and a Freshservice incident into one task | Story | D2, F2, F3 | Agent |
| W7 | `loom pr` and the review upload behind the Outbox | Story | W1, #187 | Agent |

## v0.1 tasks

Done: released as [v0.1.0](https://github.com/gsuquet/loom/releases/tag/v0.1.0) on 2026-10-12. Issue numbers match task numbers up to 14. Each task is a GitHub issue in the `v0.1` milestone, labelled `agent-ready`, written with the [agent task template](../.github/ISSUE_TEMPLATE/agent-task.md).

| # | Issue | Task | Kind | Depends on | Owner |
| --- | --- | --- | --- | --- | --- |
| 1 | [#1](https://github.com/gsuquet/loom/issues/1) | Spike: `cargo-dist` (build, GitHub releases, installers, updater) vs custom pipeline -> ADR | Research | – | Agent |
| 2 | [#2](https://github.com/gsuquet/loom/issues/2) | Cargo workspace skeleton (`loom`, `loomd`, `loom-api`, `loom-update`), workspace lints, CI on macOS + Linux, mise tasks from scratch (`check` gate), `.gitignore` cleanup (commit `Cargo.lock`, drop old `.loom/` entries) | Chore | – | Agent |
| 3 | [#3](https://github.com/gsuquet/loom/issues/3) | `loom version`: version, commit, target, channel embedded at build time | Story | 2 | Agent |
| 4 | [#4](https://github.com/gsuquet/loom/issues/4) | Release pipeline: tag -> stable, merge to `main` -> `dev` prerelease, 4 targets (macOS/Linux × arm64/x86_64), checksums, signature | Story | 1, 3 | Agent + me (secrets, keys) |
| 5 | [#5](https://github.com/gsuquet/loom/issues/5) | `install.sh`: platform detection, download, checksum verification, idempotent install into `~/.local/bin` | Story | 4 | Agent |
| 6 | [#6](https://github.com/gsuquet/loom/issues/6) | `loom-api` types: status, versions, errors; serde + JSON Schema | Story | 2 | Agent, close review |
| 7 | [#7](https://github.com/gsuquet/loom/issues/7) | `loomd` skeleton: config, single-instance lock, HTTP over unix socket, `GET /status` | Story | 6 | Agent |
| 8 | [#8](https://github.com/gsuquet/loom/issues/8) | `loom status`: queries loomd, starts it if missing, flags CLI/daemon version mismatch | Story | 7 | Agent |
| 9 | [#9](https://github.com/gsuquet/loom/issues/9) | `loom service install\|uninstall\|status`: launchd user agent / systemd user unit, opt-out in config | Story | 7 | Agent |
| 10 | [#10](https://github.com/gsuquet/loom/issues/10) | Update decision logic (pure): channel, current version, releases, policy -> action | Story | 6 | Agent |
| 11 | [#11](https://github.com/gsuquet/loom/issues/11) | `loom upgrade [--channel] [--from <path>]`, `loom rollback`: download, verify, atomic swap, keep previous binary | Story | 4, 10 | Agent |
| 12 | [#12](https://github.com/gsuquet/loom/issues/12) | Auto-upgrade in loomd: periodic check, apply, restart through the service manager, health check, automatic rollback | Story | 9, 11 | Agent |
| 13 | [#13](https://github.com/gsuquet/loom/issues/13) | otel: `tracing` + configurable OTLP exporter (off by default), spans for CLI commands and daemon operations, `mise` task for a local backend | Story | 7 | Agent |
| 14 | [#14](https://github.com/gsuquet/loom/issues/14) | E2E in CI: fake release server -> install -> upgrade -> failed upgrade -> rollback, macOS and Linux | Story | 5, 12 | Agent |
| 15 | [#22](https://github.com/gsuquet/loom/issues/22) | CLI conventions ([ADR-0010](adr/0010-cli-conventions.md)): global flags, confirmations, broken pipe, exit codes, CLI reference generated from `clap` | Story | 3 | Agent |

Parallel tracks once 2 and 6 are merged: {3, 15, 4, 5}, {7, 8, 9, 13}, {10}. 11, 12 and 14 join them.

## v0.2 tasks

Each task is a GitHub issue in the `v0.2` milestone, written with the [agent task template](../.github/ISSUE_TEMPLATE/agent-task.md). Design: [Repositories](design/repos.md), [ADR-0012](adr/0012-repo-index.md).

Released as [v0.2.0](https://github.com/gsuquet/loom/releases/tag/v0.2.0) on 2026-10-13, with tasks 1 to 10. Task 11 (a week of real use, then deleting the dotfiles inventory scripts) continues on v0.2.x. Load-sensitive tests are tracked in [#57](https://github.com/gsuquet/loom/issues/57).

| # | Issue | Task | Kind | Depends on | Owner |
| --- | --- | --- | --- | --- | --- |
| 1 | – | ADR-0012 (repo index, contexts, automatic updates) and `docs/design/repos.md` | Research | – | Me + agent |
| 2 | [#36](https://github.com/gsuquet/loom/issues/36) | `loom-repo` (pure): contexts, repo identity, `[contexts]` / `[repos]` / `[forges]` config | Story | 1 | Agent |
| 3 | [#37](https://github.com/gsuquet/loom/issues/37) | Repo scanner: roots, depth, exclusions, worktrees, removals, no git process | Story | 2 | Agent |
| 4 | [#38](https://github.com/gsuquet/loom/issues/38) | `loom-store`: SQLite index (`index.db`, FTS5), repo API endpoints in loomd | Story | 3 | Agent |
| 5 | [#39](https://github.com/gsuquet/loom/issues/39) | Repo state with `gix`: head, dirty, operation, ahead/behind, default branch, last commit, last fetch | Story | 4 | Agent |
| 6 | [#40](https://github.com/gsuquet/loom/issues/40) | `loom repo ls\|show\|scan\|register\|forget\|doctor`, repo resolution, registration on use | Story | 5 | Agent |
| 7 | [#41](https://github.com/gsuquet/loom/issues/41) | `loom search` for repos, ranking, < 100 ms | Story | 6 | Agent |
| 8 | [#42](https://github.com/gsuquet/loom/issues/42) | Automatic updates in loomd: fetch, ff-only on clean trees, scheduling, backoff, `loom repo sync` | Story | 5 | Agent |
| 9 | [#43](https://github.com/gsuquet/loom/issues/43) | `loom shell-init zsh\|bash`: registration on `cd` | Story | 6 | Agent |
| 10 | [#44](https://github.com/gsuquet/loom/issues/44) | Repo safety suite: every risky state, nothing lost, macOS + Linux | Chore | 8 | Agent |
| 11 | – | Rename the Python CLI to `loom-py`, update the dotfiles, a week of real use, delete the inventory scripts | Chore | 6, 8 | Me |

Parallel tracks once 5 is merged: {6, 7, 9} and {8, 10}.

## v0.3 tasks

Each task is a GitHub issue in the `v0.3` milestone, written with the [agent task template](../.github/ISSUE_TEMPLATE/agent-task.md). Design: [Domain model](design/domain-model.md), [Storage](design/storage.md), [User workflow](design/user-workflow.md), [ADR-0013](adr/0013-task-store.md).

| # | Issue | Task | Kind | Depends on | Owner |
| --- | --- | --- | --- | --- | --- |
| 1 | [#68](https://github.com/gsuquet/loom/pull/68) | Done: ADR-0013 (task store, kinds, priority, focus, backup) and design docs | Research | – | Me + agent |
| 2 | [#69](https://github.com/gsuquet/loom/issues/69) | `loom-core` (pure): task, kind, status, importance, links and relations, IDs, shortest unique prefix, transitions and available actions, events, undo | Story | 1 | Agent |
| 3 | [#70](https://github.com/gsuquet/loom/issues/70) | Priority (pure): lexicographic order, hidden `waiting` tasks, `--why` | Story | 2 | Agent |
| 4 | [#71](https://github.com/gsuquet/loom/issues/71) | Task file format (pure): `task.md` TOML frontmatter round trip with unknown keys kept, `events.jsonl`, body template per kind | Story | 2 | Agent |
| 5 | [#72](https://github.com/gsuquet/loom/issues/72) | `loom-store`: `StorePort`, files adapter per context, atomic writes, tasks index and FTS (migration 0005), `loom reindex`, hand-edit detection | Story | 4 | Agent |
| 6 | [#73](https://github.com/gsuquet/loom/issues/73) | loomd tasks API: create, read, edit, transitions, ranked list, events, undo | Story | 3, 5 | Agent |
| 7 | [#74](https://github.com/gsuquet/loom/issues/74) | `loom add\|ls\|show\|edit\|set\|done\|cancel\|snooze\|undo`, `--json`, context from the working directory | Story | 6 | Agent |
| 8 | [#75](https://github.com/gsuquet/loom/issues/75) | `loom go`, focus per context, `repo` link, `cd` through `shell-init`, `loom log` | Story | 7 | Agent |
| 9 | [#76](https://github.com/gsuquet/loom/issues/76) | Recurring tasks: RRULE schedules, dedup per occurrence, `skip\|stack\|replace`, latest missed occurrence | Story | 6 | Agent |
| 10 | [#77](https://github.com/gsuquet/loom/issues/77) | Archive after N days, read-only, readable and searchable | Story | 6 | Agent |
| 11 | [#78](https://github.com/gsuquet/loom/issues/78) | Backup: git repository per context store, unsigned commit per change, push and fast-forward pull, `loom sync`, `machine.toml`, doctor | Story | 5 | Agent |
| 12 | [#79](https://github.com/gsuquet/loom/issues/79) | `loom search` over tasks (title, body, links), < 100 ms | Story | 5 | Agent |
| 13 | [#80](https://github.com/gsuquet/loom/issues/80) | TUI skeleton: ratatui, API client, context colors, keymap, `?` help, `/` search | Story | 6 | Agent |
| 14 | [#81](https://github.com/gsuquet/loom/issues/81) | TUI Today and Inbox, triage (`a`, `z`, `x`, `c`, `g`), group by parent | Story | 3, 13 | Agent |
| 15 | [#82](https://github.com/gsuquet/loom/issues/82) | TUI task detail, page shell and Overview (split into 15 and 21 to 23; Files moved to v0.4, [#155](https://github.com/gsuquet/loom/issues/155)) | Story | 13, 19 | Agent |
| 16 | [#83](https://github.com/gsuquet/loom/issues/83) | Store safety suite: crash during a write, invalid hand edit, reindex equal to the files, version skew | Chore | 5, 11 | Agent |
| 17 | – | Enter my tasks, a week of starting the day with `loom` | Chore | 14, 15, 21, 22 | Me |
| 18 | [#84](https://github.com/gsuquet/loom/issues/84) | Disk usage of the loom home in `loom status` and `--json`, doctor checks (kept versions, WAL, free space) | Story | – | Agent |
| 19 | [#94](https://github.com/gsuquet/loom/issues/94) | loomd change events over SSE (`GET /v1/events`), followed by the TUI ([ADR-0014](adr/0014-tui-architecture.md)) | Story | 6 | Agent |
| 20 | [#113](https://github.com/gsuquet/loom/issues/113) | `loom service restart`: restart loomd (service manager or signal), config checked first, wait for the new loomd | Story | – | Agent |
| 21 | [#152](https://github.com/gsuquet/loom/issues/152) | TUI task detail: field pickers (status, kind, importance, due, context) | Story | 15 | Agent |
| 22 | [#153](https://github.com/gsuquet/loom/issues/153) | TUI task detail: body in `$EDITOR` | Story | 15 | Agent |
| 23 | [#154](https://github.com/gsuquet/loom/issues/154) | TUI task detail: Relations and Events tabs | Story | 15 | Agent |
| 24 | [#97](https://github.com/gsuquet/loom/issues/97) | Record notes-only hand edits of `task.md` | Story | 5 | Agent |
| 25 | [#98](https://github.com/gsuquet/loom/issues/98) | Doctor checks for task stores | Story | 5 | Agent |

Released as [v0.3.0](https://github.com/gsuquet/loom/releases/tag/v0.3.0) on 2026-10-27, with tasks 1 to 16 and 18 to 25. Task 17 (a week of starting the day with `loom`) continues on v0.3.x.

Parallel tracks: 18 and 20 from the start; after 2, {3} and {4, 5}; after 5, {11, 12, 16}; after 6, {7, 8}, {9, 10}, {19 -> 13, 14, 15}.

## v0.4 tasks

Each task is a GitHub issue in the `v0.4` milestone, written with the [agent task template](../.github/ISSUE_TEMPLATE/agent-task.md). Design: [Code review](design/reviews.md), [User workflow](design/user-workflow.md#review), [ADR-0016](adr/0016-mr-review-mirror.md). Forge fixtures: [`fixtures/forge/`](../fixtures/forge/README.md).

| # | Issue | Task | Kind | Depends on | Owner |
| --- | --- | --- | --- | --- | --- |
| 1 | [#175](https://github.com/gsuquet/loom/pull/175) | Done: ADR-0016 (MR mirror, fetched refs, publish, forge credentials), `design/reviews.md`, forge API spike and fixtures | Research | – | Me + agent |
| 2 | [#178](https://github.com/gsuquet/loom/issues/178) | `loom-forge` (pure): MR, version, thread, anchor, draft and verdict model; GitHub and GitLab payload parsing from the fixtures; MR keys; context of an MR; crate row in `architecture.md` | Story | 1 | Agent |
| 3 | [#179](https://github.com/gsuquet/loom/issues/179) | Forge credentials: `[forges."<host>"]` and `[contexts.<ctx>.forges."<host>"]` `token_cmd` / `token_env`, resolution in loomd, redaction, `forge <host>` doctor check, configuration reference, how-to to connect a forge | Story | 1 | Agent |
| 4 | [#180](https://github.com/gsuquet/loom/issues/180) | Fake forge server (serves `fixtures/forge/`, records requests) and the GitHub REST client (`ureq`): pagination, ETags, rate limits, errors | Story | 2 | Agent |
| 5 | [#181](https://github.com/gsuquet/loom/issues/181) | GitHub GraphQL client: review threads, pending reviews, the publish mutations, GraphQL errors | Story | 4 | Agent |
| 6 | [#182](https://github.com/gsuquet/loom/issues/182) | GitLab REST client: MRs, versions, diffs, discussions, draft notes, `bulk_publish`, approve, pagination, errors | Story | 4 | Agent |
| 7 | [#183](https://github.com/gsuquet/loom/issues/183) | Spike: the GraphQL publish flow (pending review, reply into it, submit, resolve) on a real test repository; update ADR-0016 and the fixtures if it differs | Research | 5 | Me + agent |
| 8 | [#184](https://github.com/gsuquet/loom/issues/184) | MR sync in loomd: poll MRs to review and open Review tasks per context, Review tasks (create, reopen on a new head, close), mirror files (`mr.json`, `description.md`, `versions.json`, `threads.json`), `loom review sync` | Story | 3, 5, 6 | Agent |
| 9 | [#185](https://github.com/gsuquet/loom/issues/185) | MR refs and diffs: clone and remote of the target project, fetch into `refs/loom/mr/…`, merge base, `diffs/<sha>.patch`, "since last review" by normalized per-file diff, `reviewed` ref, cleanup on archive, no-clone mode from forge patches | Story | 8 | Agent |
| 10 | [#186](https://github.com/gsuquet/loom/issues/186) | Review drafts (pure + API): `review.json`, comments, replies, resolves, verdict, viewed marks, re-anchoring after a new version; loomd endpoints, `loom review ls\|show` | Story | 9 | Agent |
| 11 | [#187](https://github.com/gsuquet/loom/issues/187) | Publish plan (pure): pre-checks, steps per forge, intents, reconciliation with what the forge holds, resume; `review` event | Story | 10 | Agent |
| 12 | [#188](https://github.com/gsuquet/loom/issues/188) | Publish to GitHub: GraphQL steps, `loom review publish`, verified once on a real test repository | Story | 7, 11 | Agent + me (test repo, token) |
| 13 | [#189](https://github.com/gsuquet/loom/issues/189) | Publish to GitLab: draft notes, `bulk_publish` with `reviewer_state`, approve with `sha`, verified once on a real test project | Story | 6, 11 | Agent + me (test project, token) |
| 14 | [#190](https://github.com/gsuquet/loom/issues/190) | `ui-style.md`: review components (file tree row, diff hunk, inline thread and finding, publish modal) and their Web mapping | Research | 1 | Agent |
| 15 | [#191](https://github.com/gsuquet/loom/issues/191) | **Dropped by [#215](https://github.com/gsuquet/loom/issues/215).** TUI Review queue: list, preview (changed files, threads), `enter` opens the Review page | Story | 8, 14 | Agent |
| 16 | [#192](https://github.com/gsuquet/loom/issues/192) | **Dropped by [#215](https://github.com/gsuquet/loom/issues/215).** TUI Review page: file tree, unified diff, `]`/`[` `}`/`{`, `v` viewed, "since last review", inline threads, reduced mode without clone | Story | 9, 15 | Agent |
| 17 | [#193](https://github.com/gsuquet/loom/issues/193) | **Dropped by [#215](https://github.com/gsuquet/loom/issues/215).** TUI Review page: side-by-side diff, highlighting, context expansion, lazy rendering, collapsed generated files | Story | 16 | Agent |
| 18 | [#194](https://github.com/gsuquet/loom/issues/194) | **Dropped by [#215](https://github.com/gsuquet/loom/issues/215).** TUI review gestures: `c`/`V` comment with the Conventional Comments picker, `s` suggestion, `r` reply, `R` resolve, `P` publish modal with verdict, `w` worktree, `o` editor | Story | 10, 12, 13, 16 | Agent |
| 19 | [#195](https://github.com/gsuquet/loom/issues/195) | Repo forge metadata on the repo card: project ID, forge default branch, visibility, archived, web URL, open MRs | Story | 4, 6 | Agent |
| 20 | [#196](https://github.com/gsuquet/loom/issues/196) | `loom pr`: push the branch, draft MR linked to the task (`delivery` link, `mr` event), existing MR linked | Story | 4, 6 | Agent |
| 21 | [#155](https://github.com/gsuquet/loom/issues/155) | **Dropped by [#215](https://github.com/gsuquet/loom/issues/215).** TUI task detail: Files tab (moved from v0.3) | Story | v0.3 task 15 (done) | Agent |
| 22 | [#197](https://github.com/gsuquet/loom/issues/197) | Review safety suite: force push, rebase, removed and renamed files, line gone, 409 and 422 from the forge, loomd stopped after each forge write before its record, revoked token | Chore | 12, 13 (18 dropped) | Agent |
| 23 | – | A week of reviews from loom, without the forge inbox | Chore | app Review, step 4; 20 | Me |

Parallel tracks once 1 is merged: {2 -> 4 -> {5 -> 7}, {6}}, {3}, {14}. Once 5 and 6 are merged: {19, 20} and 8 -> 9 -> 10 -> 11 -> {12, 13}; after 8 and 14, {15 -> 16 -> 17}; 18 joins them. 21 at any time. Tasks 15 to 18 and 21 are dropped: the TUI is removed ([#215](https://github.com/gsuquet/loom/issues/215)).
