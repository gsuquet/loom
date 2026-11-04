# User workflow

> The desktop app: pages, daily loop, keys, and the review, run and knowledge flows.

The behaviour below is shown working in the [prototype](../prototypes/README.md).

## Principles

- One window, always open on a secondary screen or reached with AeroSpace. Pages are in a collapsible sidebar.
- Keyboard first, with every action also available by mouse. Actions available on a task are computed by the core, not the app.
- Every row of every page shows its context, by colour and shape. On a page about one item, a solid band at the top names the context. A wrong-client action is the costliest mistake in loom.
- Local actions are undoable. Writes that leave loom follow [Write states](connectors.md#write-states).
- Loom interrupts me only when it needs me.
- The layout works tiled and full width, on a 24 to 27 inch secondary screen and on a single 34 inch screen.

## Daily loop

| Moment | Gesture | Page |
| --- | --- | --- |
| Capture | Global hotkey logs one line from any app; `loom add` from a shell; phone queue for links | Anywhere |
| Start of day | Loom suggests a short list and I confirm it. This is configurable | Today |
| Triage | Accept, send to Reading, dismiss, mute, delegate, with multiple selection | Inbox |
| Work | Open the task beside the list, add a worktree, start an agent | Today, task |
| Review | Launch a reviewer, edit its draft, upload | Reviews, review |
| Approve | Read the proposal, open its diff, approve or reject | Task, Agents |
| Learn | Distil sources into pages, close conceptions | Reading, Knowledge |
| End of day | Timeline for standups, timesheet suggestion per context | My day |

## Pages

| Page | Key | Holds |
| --- | --- | --- |
| Today | `g t` | The sections below, with the selected task's page beside the list |
| My day | `g d` | Timeline, Sent records, timesheet suggestion |
| Inbox | `g i` | Everything assigned, with multiple selection |
| Reviews | `g v` | MRs waiting on me, from all connectors |
| Agents | `g a` | Runs, pipelines and proposals |
| Meetings | `g m` | Imports, and what each one yielded |
| Reading | `g r` | The source queue, with multiple selection |
| Knowledge | `g k` | Pages, status, relations, proposals, promotion |
| Later | `g l` | Tasks moved to a later date, and when each returns |
| Outbox | `g o` | Held and Not sent writes |
| Settings | `g s` | Appearance, shortcut mode, connectors, rules |
| Help | `g h` | Keys of the current page |

## Keys

Single letters are the default, because digits and `? / ] [` need Shift or Option on a macOS AZERTY keyboard.

- **Global:** `g` then a letter for pages, `q` search, `h` sidebar, `w` page filter, `u` undo or cancel a hold, `l` log a line (`Tab` changes its context), `Cmd+K` palette, `Cmd+,` Settings. `Esc` closes the innermost open thing, then returns to the page I came from, which makes it the one back key. In lists, `j k` move and `n p` jump between sections.
- **Today:** `s` start, `z` later, `r` reply, `m` mute, `a` send an agent, `o` open at the source, `d` done, `v` fold a section, `V` unfold all, `y` confirm the short list, `Enter` go into it.
- **Inbox:** `x` select, `X` select a section, `y` accept (on a team pool ticket, assign it to me, with the hold), `z` accept with a date, `r` send a capture to Reading, `d` dismiss, `c` change a capture's context, `a` send an agent, `o` open, `m` mute, `Esc` clear the selection.
- **Task:** `a` send an agent, `o` open, `s` start, `d` done, `w` add a worktree (this replaces the filter here), `f` add a file, `Cmd+Enter` approve the selected proposal, `Enter` follow a link or open a run, `Esc` back to the list.
- **Reviews:** `a` send a reviewer, `o` open at the source, `Enter` open the review.
- **Review:** `j k` lines, `n p` files, `m M` next and previous comment, `c` comment, `e` edit, `x` keep or drop, `a` send a reviewer, `f` viewed or not, `v` unified or side by side, `o` open in the editor, `Cmd+Enter` open the upload bar or, with no comments, approve, `Esc` back. In the upload bar, `Cmd+Enter` uploads and `Cmd+Shift+Enter` approves with the upload.
- **Agents:** `o` open at the source, `Enter` open the run.
- **Run:** `r` answer the question, `s` resume an interrupted run, `v` take over in my terminal (press twice), `x` stop (`Enter` confirms), `o` open, `Cmd+Enter` approve, `Esc` back.
- **Meetings:** `a` send the distiller, `f` add notes or a transcript, `o` open at the source, `Enter` open.
- **Meeting:** `y` accept, `x` reject, `o` open, `Esc` back.
- **Reading:** `x` select, `X` select a section, `y` next stage, `d` dismiss, `a` send an agent, `f` add a link or feed, `o` open, `Enter` expand a feed, `Esc` clear the selection.
- **Knowledge:** `Enter` open a page, `e` edit, `y` accept an update, `x` reject it, `P` promote, `Esc` back.
- **Outbox:** `Cmd+Enter` retry, `o` open, `d` discard.
- **Later:** `y` bring back, `t` today, `m` next week.
- **My day:** `c` copy the standup, `y` copy the timesheet.
- **Settings:** `Enter` change the setting.
- **Pack panel:** `t` say what I want done, `j k` move, `x` include or leave out (a flagged item stays out), `b` run in the background, `v` run in my terminal, `e` next specialist, `f` skip the panel next time, `Enter` send, `Esc` cancel.
- **Dialogs:** short list `j k x Enter`; mute `j k Cmd+Enter`; promote `r k Cmd+Enter`.

**Shortcut display** is a setting with three modes: displayed (the default), hidden, and hidden with a message after a click when a shortcut exists.

In a prompt, `g` closes the prompt. It never triggers one of the prompt's actions.

## Today

Sections, in this order: **Urgent, In progress, Due, Waiting on you, Follow-ups, Planned, With an agent.**

- A task leaves its section when started or moved to later, and returns to it saying it came back.
- Due holds overdue and due today only.
- **Follow-ups:** an automatic flag after N days without movement, plus an optional due date.
- **Urgent** ignores the filter and cannot fold. Anything urgent and untreated shows on a strip wherever it sits, including the Inbox and items moved to later, and a p1 notifies.
- An agent's proposal shows first on its task and on Today.
- **Filter:** `w` shows one context on the current page and clears on leaving it. Counts and search ignore it.
- There is no context lock and no focus task.

## Inbox

Everything assigned arrives here, with multiple selection, except what [Ingestion](connectors.md#ingestion) sends to Urgent. Flapping alerts are grouped by monitor. Dismissing clears what is there; a monitor that fires again comes back as a new row. The team pool of unassigned tickets is browsed from here, and assigning one to myself is an Immediate write.

**Dates** are typed in one field: `fri`, `+3d`, `15 dec` or an ISO date. The field shows the resolved date before I confirm.

**Live updates** follow `loomd`'s change events. Rows never move under the selection: new and moved rows wait behind a count at the top of their section, and apply when I leave the page or choose the count. The selection follows its item. Urgent rows are the exception and appear at once.

## Task page

It opens beside the list. It shows the fields, the Markdown notes, the proposal first when there is one, relations, events, runs and files. Files are secret-scanned when added.

Projects and conceptions are parent tasks. A parent shows its **plan map**: a list of its subtasks with their status, their MRs and their blocks relations, in order. A list comes first; a drawn map only if the list proves insufficient.

Closing a conception asks where each lasting decision goes, see [Closing a conception](knowledge.md#closing-a-conception).

## Review

Data, git and forge side: [Code review](reviews.md).

- **Flow:** launch a reviewer. It creates a local draft review with Conventional Comments. I see it, edit it and approve the upload.
- The diff is unified by default, with a side-by-side view. Draft comments sit in a side column. Files can be marked as viewed.
- Approving is refused while a blocking comment remains. A diff must be opened before its proposal can be approved.
- **Waiting on me:** review requested, assigned, mentions, and my own MRs with new comments, failed pipelines or approval to merge. There is no pool of all team MRs.
- An MR is a [queue entry](reviews.md#waiting-on-me) until I start on it. Opening the review or any reviewer launch creates the Review task, and tasks from a pre-review rule stay out of Today until opened.
- An opt-in rule can pre-review MRs from chosen repos.
- An agent can address comments on my own MRs. Its changes come back as a proposal before any push.
- **Pipeline retry** is an Immediate write: a letter, then the hold.
- **Multi-repo MRs** show as one group row on Reviews, under their task, in order when the task defines one. A merge out of that order shows a warning in the group. Merge itself stays in the forge.

## Agents and runs

The Agents page lists runs across contexts, then proposals. A run page shows the stage strip, the live stream, cost and gate results. The states are running, waiting on me, failed and interrupted, see [Agent orchestration](agents.md#rules).

The panel showing what an agent receives opens the first time for each specialist, and can be skipped afterwards.

## Reading and Knowledge

See [Knowledge](knowledge.md). An RSS feed is one row on Reading, and `Enter` expands it in place. A source's facts (origin, date, stage, quote check, pages that cite it) show in the side panel when it is selected.

## Notifications

Approval needed, clarification needed, run failed, sync conflict, p0 ingested, expired token. They are configurable. New MRs and tickets only update a count.

## Memory of the day

- A timeline for standups and timesheets.
- Recorded automatically: status changes, MRs, reviews, agent runs, hotkey lines, Claude Code sessions, commits.
- Timesheets: a per-context suggestion to copy into the timesheet tool. No connector, no timers, and time is estimated from activity.
