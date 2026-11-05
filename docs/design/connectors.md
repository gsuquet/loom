# Connectors

> What enters loom from each tool, what loom writes back, and the safeguards on every write.

Adapters are compiled into loom for now, configured and credentialed per context. They use small ports, not one interface: ingest, tracker, forge, agent data. Each adapter declares its capabilities. The ports, the config, the poll loop and the sync rules are in [Connector ports](connector-ports.md).

A connector is an instance, not a tool. Several GitLab, GitHub or Datadog connectors can exist, one or more per context. Status, credentials and filters belong to the instance, and every status names its context.

| Order | Tool | Enters loom | Writes from loom |
| --- | --- | --- | --- |
| 1 | GitLab | MRs waiting on me | Approve, comment, retry a pipeline, upload a review. Merge stays in the forge |
| 2 | Datadog | Filtered by team, alert level or other tags | Mute, confirmed and with an end time |
| 3 | Jira | Assigned to me; unassigned team tickets in a pool to browse | Create, edit, change status |
| 4 | Freshservice | Assigned to me; unassigned team tickets in a pool to browse | Reply, change status |
| 5 | GitHub | As GitLab, for loom itself | As GitLab |
| Later | Google Calendar | Meetings, if possible, attached to their notes or transcript | None |
| Later | Google Meet | Transcripts, if an account allows it, see [Meetings](knowledge.md#meetings) | None |

- No Slack connector: Datadog covers most of that traffic.
- Datadog monitors that trigger on-call create a Freshservice incident, and many others do not. Both connectors are needed. Incidents carry the monitor ID or link, so loom links them automatically into one task with both sources attached.
- Splunk On-Call is a link only.
- "Assigned to me" and team filters are parameters per context and per tool.
- Items from a connector stay in that connector's context. Only captures can change context.

## Ingestion

Everything assigned goes to the Inbox, except alerts from Datadog's two highest priorities, which go straight to Urgent by default, and MRs, which are [queue entries](reviews.md#waiting-on-me) on Reviews. Datadog has priorities 1 to 5 and no P0: priority 1 becomes p0 and priority 2 becomes p1. This can be changed per instance. Flapping alerts are grouped by monitor. Dismissing clears what is there, and a monitor that fires again comes back as a new row. The team pool is made of [queue entries](connector-ports.md#team-pool-entries), not tasks. The rules are in [From item to task](connector-ports.md#from-item-to-task).

Each connector shows its [status](connector-ports.md#connector-status) and last successful sync, so an empty queue never looks like "nothing to do".

## Write levels

| Level | Actions | Confirm |
| --- | --- | --- |
| Immediate | Status changes, assigning a ticket to myself, retrying a pipeline | A letter, protected by the hold |
| Preview | Freshservice replies, Jira create or edit | `Cmd+Enter` |
| Confirm with an end time | Datadog mutes | `Cmd+Enter` |
| Upload | Review upload, push, promotion | `Cmd+Enter`; approving an MR with an upload takes `Cmd+Shift+Enter` |
| Proposal only | Anything from an agent | Approval of the proposal |

`Enter` is a new line in every multi-line field. Key repeat never confirms anything.

A dialog that writes shows the context and what it writes to. A create action only offers contexts with a connector that can do it, and names the instance. Creating a transcript or a ticket asks for the context with no default. A one-line journal entry defaults to the last used, and can be changed.

## Write states

Every write that leaves loom has one state model on every surface: **Held**, **Sent** or **Not sent**.

- **Held:** the write waits 5 seconds with a visible countdown and can be cancelled. While held, a row says so and nothing else changes.
- **Sent:** there is no undo. A Sent record on My day holds the context, target, full text and how to reverse it by hand.
- **Not sent:** this outranks any other row text and stays visible until opened. Failed writes stay in the Outbox with the error and a retry. They are never dropped, and never retried on their own after a conflict.

If the connector is known to be down, there is no countdown: the write goes to the Outbox and says so.

No part of this exists in the code yet, see [What exists today](connector-ports.md#what-exists-today). Writes from a connector wait for the Outbox.

**Lifetime of a hold:** a held write lives in `loomd`, as an Outbox entry with a send time. Closing the window changes nothing. If the machine slept through the send time, the write is not sent on wake: it waits in the Outbox, says why, and asks. A store sync never carries a held write to another machine.

## Undo

Undo reverses one local action through its own inverse, and names what it reverses. It never touches the Outbox, a held write or the Sent record. A completed send does not erase local undo. Anything already delivered to an agent is not undoable.
