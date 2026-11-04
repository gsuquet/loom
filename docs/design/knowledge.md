# Knowledge

> What loom keeps, how it stays true, and how a decision outlives the task that made it.

Knowledge is the reason loom is more than a task list. Neither a forge nor a tracker keeps what I read, what I tested, or why I rejected an option.

## Layers

Each layer has one job. History never changes; only living layers are edited or pruned.

| Layer | Holds | Changes |
| --- | --- | --- |
| Event log (`events.jsonl`) | What happened | Append-only |
| Sources and task `files/` | Raw evidence: local copies, transcripts, results | Kept as they came in |
| Conception task | What was decided then, and why | Frozen once archived |
| Knowledge page | What is true now, with evidence | Living, and pruned |
| Repo docs | What a team and the agents in that repo need | Through a proposed MR |
| Graph in `index.db` | Links, derived from the files | Rebuilt, never a source of truth |

## Sources

A **Source** is something I can cite: article, book, feed item, file, meetup notes, test result, meeting, or client experience. Experience has no URL, stays anonymous, and the quote check does not apply to it.

- **Capture:** from sites, LinkedIn, newsletters, RSS feeds and colleagues, mostly from the phone. See [Phone queue](#phone-queue).
- **Stages:** `captured → to read → read → distilled → reference`, or `dismissed`.
- **Reading** happens outside loom. Loom keeps a local copy, which the quote check runs against.
- **Investigate:** research on a source, which may need tests, is a task of kind `research`.
- The Reading page shows the queue with a count, separately from Today. It takes multiple selection, and an RSS feed is one row that expands in place.

## Knowledge pages

A **Knowledge page** is an exploitable page on a topic, drawn from official docs, my tests, client experience, community articles and meetups. It replaces the Note entity.

A page has:

- its overall approach, since the practices depend on each other;
- practices by section, each linked to its sources;
- a status on each claim: **tested**, **applied**, **docs only** or **said**;
- the setup a practice depends on, when it does;
- a "tried and rejected" section.

A portable page cites client sources without naming them. A said claim is one person's word from a meeting, not evidence. A mechanical quote check verifies quoted text against the local copy of its source.

The most useful content comes from experience, so distilling cannot be fully delegated. A distiller's update is one row that I accept or reject.

## Meetings

What is said in meetings is a source.

- A meeting belongs to one context. Its raw transcript or notes never leave that context. Only knowledge distilled from it can be promoted.
- Pasted notes and imported transcript files come first. Google Meet is the main source: an exported Doc import, then a connector. Loom does not record.
- A separate local transcription tool is being studied, in a spike after delivery step 2. It would stay outside loom and drop a `.vtt` file in a watched folder, and I would start it deliberately, because local capture is invisible to the other participants.
- A secret scan runs on import.
- The meeting distiller extracts decisions, action items, open questions and claims, each pointing to its place in the source. The quote check also applies to transcripts.
- Action items are proposals in the Inbox.
- An agent receives the summary by default, and the full transcript only if added.
- Meetings appear on My day, for standups and timesheets.

An import saved in the wrong context can be deleted and redone until the distiller first runs. This is recorded on My day.

## Conceptions

A **conception** is a design: the problem, the decisions, the rejected options and the open questions. It is a task of kind `conception`, not a separate entity. The grilling specialist writes it with me, and it creates its subtasks as ordinary child tasks.

- **Done when** its decisions are agreed and its subtasks are approved. Subtasks have their own done conditions, which follows the [granularity rule](domain-model.md#granularity-rule).
- **Realised** is derived: all its subtasks are closed. It is not a status.
- **Creating subtasks:** the specialist's output lists them, and I approve them before any task exists. Running it again creates no duplicates.
- **Changing it** flags its open subtasks. They are never rewritten automatically.
- **Agent context:** an implementer receives its subtask and the decisions that apply to it, not the whole document.

## Closing a conception

The task stays the record of the decision. An archived task is read-only, so without this step decisions end up buried.

Closing asks where each lasting decision goes:

| Destination | For |
| --- | --- |
| Knowledge page | Reusable knowledge, with the task as its source |
| Repo docs | What the team needs, as a proposed MR |
| Out-of-scope record | Rejected options, with the reason, so agents do not propose them again |
| Nowhere | It stays in the task's history. This is valid and common |

The repo docs target is set per context, because it depends on the setup:

| Setting | Target |
| --- | --- |
| None | No repo docs from this context |
| Applied repo | The repo the decision applies to |
| Docs repo | A dedicated repository |
| Monorepo path | A path in the monorepo |

When a conception spans several repos, each decision names its repo, and loom asks when it cannot tell.

A parent stays out of the archive while it has open children. Revising an archived conception creates a new one with a `supersedes` relation.

## Relation graph

Links live in the files. `index.db` derives the graph, so it can always be rebuilt.

- Links run from claims to sources, and between tasks, repos, tickets, meetings, people and pages.
- People are links inside their context. They are never promoted, and names are flagged at promotion.
- The app shows a list of relations first. Traversal is limited to the task's context plus shared knowledge.

## Sharing and promotion

- Each context is separate. Articles and distilled knowledge can be shared across contexts.
- Removing a context removes its store. Retention policy is out of scope.
- Promotion needs my approval. It shows the full text as it will read, and each term flagged by the per-context list needs a decision. It is refused on a possible secret.
- Client meeting content reaches a shared page only through promotion.
- A secret scan runs before anything leaves a context.

## Keeping knowledge useful

Accumulated noise costs every agent that loads it.

- **Text loaded into agents** must name the plausible mistake it prevents, one the compiler, the tests or a reading of the code would not catch. Otherwise it is removed. This covers specialist prompts and packs, not knowledge pages, which also serve me as references.
- **Writers split by verb:** authors add and amend. A separate pruning agent only proposes deletions and tightening, because an author overrates what was just written. I approve its proposals. It never adds or changes a decision, and sends wrong text back to its author's flow.
- **Per-context glossary:** terms with the names to avoid, which also feeds the promotion term list.
- **Out-of-scope records:** see [Closing a conception](#closing-a-conception).

## Search

Search covers tasks and knowledge across contexts. It ignores the page filter.

## Phone queue

An optional Cloudflare Worker that each user deploys. It serves an installable web app and an API, D1 holds the queue, and `loomd` polls it. Public links only by default. Each item is deleted once pulled.

Android starts with capture only. Reading and triage may come later. The Android share target and the Worker authentication need a spike first.
