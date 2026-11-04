# Prototypes

> Working mockups of the desktop app, on demo data, written before the code.

A prototype settles behaviour: what each page shows, the keys, and how many keystrokes a job takes. Open the file in a browser. It needs no build and no server.

| Prototype | Covers |
| --- | --- |
| [app/index.html](app/index.html) | Today, task, Inbox, Reviews, review, Agents, run, Meetings, meeting, Reading, Knowledge, Later, Outbox, My day, Settings |

All the screens share one file, because they share state and keys. A screen moves to its own `docs/prototypes/<screen>/` folder only if it needs more than this file gives it.

## What is normative

- **Normative:** pages, sections, keys, confirmations, write states and the wording of warnings.
- **Not normative:** the look. Colours, type and the three looks in Settings (Desk, Ledger, Focus) are drafts. The target look is native macOS, see [UI style](../design/ui-style.md#desktop-app-target).

If a design doc and the prototype differ, the design doc wins.

## Known differences from the design

- A meeting's action item goes to Planned in the prototype. In the design it is a proposal in the Inbox.
- A write shown during its hold changes the row text in the prototype. In the design a held row says Held and nothing else changes.
- Undo restores a snapshot of the whole page state in the prototype. In the design it reverses one local action through its own inverse.
- There is no conception kind, pipeline or plan map yet.
- The Later bar uses `t` and `m`, not a typed date. The Outbox shows no Held rows.
- A task's Jira link says status changes update Jira at once. In the design they go through the hold.
- After closing a prompt, `g` still starts a page chord.

## Lifecycle

A prototype is deleted once its screens are built, when real screenshots replace it.
