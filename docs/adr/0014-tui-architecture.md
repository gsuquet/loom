# ADR-0014: TUI architecture

- Status: proposed
- Date: 2026-10-13

## Context

v0.3 ships the first TUI views (Today, Inbox, task detail). Later versions add views the skeleton must not need rewriting for: MRs to review and the review page (v0.4), drafts and pushes to trackers (v0.5), agent runs, attach and approvals (v0.7), pipelines (v0.10).

## Decision

1. **Three kinds of screens.**
   - **Queues**: lists of things to act on, one shared component (rows, preview pane, keys acting on the selection), reached from the tab bar. Today, Inbox, Waiting (v0.3); Review (v0.4); Outbox (v0.5); Runs, Approvals (v0.7); Pipelines (v0.10).
   - **Object pages**: full screen with their own tabs and keys, opened with `enter` from any queue. Task (v0.3), MR review (v0.4), Run (v0.7).
   - **Modals**: pickers, the command palette, confirmation with a diff before any push.
2. **Layout**: a queue is a list with a preview pane of the selected item on the right (the task overview, later a diff, a stream tail, findings). Under about 120 columns, or on a key, the pane is hidden and the list takes the full width.
3. **Navigation**: a tab bar with counts, number keys for queues, and a `:` command palette (fuzzy) over every view, action and context. `?` lists the bindings of the current layer.
4. **Layered keymap**: global keys (`1`–`9`, `:`, `/`, `?`, `q`), then the keys of the current view, then those of an open modal. A key may mean different things in different views (`c`: done in a queue, comment in a review). Help is generated from the bindings of each layer.
5. **Live data**: the TUI subscribes to loomd's change events (SSE) and refetches what changed; it does not poll.
6. **Colors**: the terminal's 16 ANSI colors only, so the TUI follows the terminal theme (light or dark). Context colors are chosen among them in config.
7. **Attach to an agent run** is structured: the run page renders the run's stream (messages, folded tool calls, diffs, findings, cost), sends a message mid-run when the runtime supports it, and **take over** stops the run and opens a shell in its worktree. No pseudo-terminal: contracts, gates and replayable streams stay intact for every runtime, direct API included.
8. **`g` (go) from the TUI** sets the focus and quits; the `loom` shell function installed by `loom shell-init` changes directory to the task's repo, the way file managers do "cd on quit".

## Consequences

- The v0.3 skeleton includes the queue component with its preview pane, the palette, the layered keymap and the SSE client, before any of them has more than two users.
- loomd needs a change-event stream (`GET /v1/events`, SSE) in v0.3.
- No truecolor theme; context colors are limited to 16.
- Correcting an agent live is limited to messages; anything more means taking over by hand, outside the contract.
- Whether a runtime accepts messages mid-run is a runtime capability, settled with the v0.7 runtime design.
