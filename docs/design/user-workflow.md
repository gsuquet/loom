# User workflow

> Gestures, daily loop, TUI views, task inspection and code review.

### Principles

- Every TUI action has a CLI equivalent (scriptable, testable). Available actions per task are computed by the core, not the UI.
- Keyboard first, single-key verbs, vim navigation, `/` to search everywhere, `?` for help.
- Each context has a color, shown on every task, run and action: posting to the wrong client must be visually obvious.
- Local actions are undoable (they are events). Pushed actions are not: they always show a diff and ask for confirmation.
- Loom interrupts me only when it needs me: approval, clarification, failure, conflict.

### Daily loop

| Moment | Gesture | Surface |
| --- | --- | --- |
| Capture | `loom add "…"` from any shell, context inferred from cwd; global hotkey later (Raycast/Alfred calling the CLI) | CLI |
| Start of day | `loom` opens the Today view | TUI |
| Triage | On each inbox item: `a` accept (kind, importance, due), `d` delegate, `z` snooze (`waiting until`), `m` merge into an existing task, `s` split, `x` cancel | TUI |
| Focus | `loom go <id>` / `g`: set active, cd to the repo or worktree, open the editor. One focus task at a time; delegated tasks run in the background | CLI + shell integration |
| Note | `loom log "…"` attaches to the focus task | CLI |
| Delegate | `d` -> suggested template -> readiness check -> pack preview (editable) -> budget -> run starts | TUI |
| Watch | Run view: live stream, attach/detach, send a message, cancel | TUI |
| Approve | Approval queue: proposed action + diff + gate results + cost; approve, reject with a reason (feeds metrics), edit, approve all of a class | TUI |
| Create a ticket | `n` from the template in `$EDITOR`, or `N` with the ticket-writer agent; drafts view; `P` publishes after lint and preview | TUI + editor |
| Edit a ticket | `e` opens the issue mirror in `$EDITOR`; `p` shows the diff against remote and pushes | TUI + editor |
| Close | `c` done, optional "learned" prompt, archived after N days | TUI / CLI |
| End of day | Journal summary and timesheet suggestion per context | TUI / CLI |

### TUI views

```
┌ Today ─────────────── all contexts ▾ ─ inbox 4 · approvals 2 · runs 3 ┐
│ ● p0 [company-a] Investigation  checkout 5xx spike         due 11:00 │
│ ● p1 [company-a] Bug            login timeout mobile       active    │
│ ◐ p1 [ippon]     Review         !412 auth refactor         agent ▸   │
│ ○ p2 [personal]  Chore          renew certs                due fri   │
├ Runs ───────────────────────────────────────────────────────────────┤
│ ▸ reviewer@3  !412   stage review   2m14  $0.41  streaming…         │
│ ⚠ implementer@5 CORE-1042  needs clarification (2 questions)        │
└ a accept  d delegate  g go  e edit  p push  / search  ? help ────────┘
```

Views: Today (priority list), Inbox, Runs, Approvals, Task detail, Review, Journal, Search.

Nothing requires leaving the TUI to read: every task, issue, MR, run and file is inspectable in place, archived ones included (read-only). `$EDITOR` stays available, never required.

### Task detail

Tabs:

| Tab | Content |
| --- | --- |
| Overview | Fields (kind, status, importance, due, context, parent) + `task.md` body rendered as Markdown |
| Issues | Each issue mirror rendered: fields, description, comment thread, local changes not yet pushed |
| Relations | Linked tasks, issues, MRs, repos; navigable (`enter` follows a link, `backspace` goes back) |
| Events | Timeline filterable by type and actor (me, agent, daemon, rule) |
| Runs | Runs of this task; opens the run view (live, or replayed from the stored stream) |
| Files | Artifacts with preview: Markdown, text, diffs; images through the terminal graphics protocol when supported (Ghostty, Kitty) |

Editing in place: fields through pickers (status, kind, importance, due, context), text (body, comments, issue description) in an embedded editor. `$EDITOR` is an alternative for long texts.

### Code review

```
┌ Review !412 auth refactor ── [ippon] ── head a1b2c3 · since last review: 3 files ┐
│ ✓ src/auth/mod.rs   +12 -4 │ @@ -40,7 +40,9 @@ fn verify(token: &str)          │
│ ● src/auth/jwt.rs   +88 -31│  40   let claims = decode(token)?;                 │
│   src/api/login.rs  +5  -2 │  41 - if claims.exp < now() {                       │
│   Cargo.lock (collapsed)   │  41 + if claims.exp <= now() {                      │
│                            │  ┆ ⚑ agent: issue (blocking): clock skew not handled│
│                            │  ┆ 💬 alice: why not leeway?          [1 reply]     │
└ ]/[ hunk  }/{ file  v viewed  c comment  y/n finding  r reply  P publish review ┘
```

- **Layout**: file tree (change counts, viewed mark, findings and threads count), diff (unified, or side by side when the terminal is wide enough), threads and findings inline under their line.
- **Data, all local**: diff per head SHA, base and head file contents from the fetched refs (expand context around a hunk), forge threads pulled into the MR mirror, agent findings from reviewer runs.
- **Gestures**: `]`/`[` hunks, `}`/`{` files, `v` mark file viewed, `c` comment on a line or `V` selection, with a Conventional Comments label and decoration picker, `s` suggestion block (GitHub/GitLab suggestion syntax), `r` reply, `R` resolve, `o` open the file at the line in `$EDITOR` (optional), `w` check the MR out in a worktree to run it.
- **Agent findings**: typed (path, line range, side, label, decorations, body, optional suggestion, confidence), shown inline. `y` turns a finding into my draft comment, `n` dismisses it, `e` edits it. Accept and dismiss rates are the reviewer template's precision metric.
- **Drafts until publish**: comments stay local; `P` publishes them as a single review with a verdict (approve, request changes, comment), through the forge's batch review APIs. Publishing is team-visible: autonomy policy applies.
- **Re-review after a push**: "since last review" shows only the diff between the last reviewed SHA and the new head. Viewed marks reset only on files that changed. Draft comments are re-anchored, or flagged when their line disappeared.
- **Large MRs**: lazy rendering; generated and vendored files collapsed (patterns per repo in the repo card).
- Candidate crates (to validate): `ratatui`, `syntect` (highlighting), `tui-markdown`, `tui-textarea`, `ratatui-image`.

### Notifications

- Desktop notifications for: approval needed, clarification needed, run failed, sync conflict, p0 ingested.
- Shell prompt segment: current context and focus task.
- Remote push for night shifts later (e.g. ntfy or a Slack DM), so a blocked run is not discovered in the morning.
