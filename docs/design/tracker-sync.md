# Tracker sync

> Two-way sync and creation of Jira, GitLab and GitHub issues.

Edit issues locally, in the file or the TUI, and push them upstream.

### Mirror layout

```
tasks/<id>/issues/jira-CORE-1042/
  issue.md          # editable: frontmatter (title, status, labels, assignee, priority) + description in Markdown
  comments.md       # read-only rendered thread
  drafts/*.md       # new comments waiting to be pushed
  .base/issue.md    # last synced state, base of the 3-way merge
  raw.json          # last full remote payload
```

### Synced fields (v1)

Title, description, status, labels, assignee, priority, new comments. Not synced: editing or deleting existing comments, attachments, sprint, custom fields (read-only in `raw.json`).

### Sync rules

- **Pull** (poller machine): remote changes merge into `issue.md` with a 3-way merge against `.base`. Field-level for frontmatter, text merge for the description.
- **Push is explicit by default**: `loom push <task>` or the TUI shows the diff against the fresh remote, then pushes. Every edit notifies the issue's watchers, so batching edits matters. Auto-push with debounce is a per-context opt-in.
- Before each push: fetch remote, 3-way merge, push the merge, store the new base. A conflict blocks the push and creates an inbox item; nothing is overwritten silently.
- Only changed fields are sent. An unchanged description is never re-sent, so loom's Markdown conversion never reformats an issue nobody touched.
- Every push is an event with before/after values.
- Agents edit issue mirrors through proposed actions: same push pipeline, same approval.

### Provider specifics

- **GitHub / GitLab**: Markdown native. Status = open/closed, plus scoped labels for workflow states (`workflow::review`) mapped per context. Write needs the GitLab `api` scope (`read_api` is not enough) and a fine-grained GitHub token with issues write.
- **Jira**: the hard part.
  - Cloud REST v3 descriptions and comments are ADF (JSON), Data Center uses wiki markup. Conversion to Markdown is lossy (panels, mentions, media, merged table cells, status lozenges): unsupported nodes are kept as opaque placeholders in the Markdown so a round-trip preserves them.
  - Status is changed through workflow transitions, not set. Loom finds the transition to the target status; if it requires fields (resolution, …), the TUI asks for them; if no transition exists, the edit is rejected.
  - Status vocabulary mapped per context (workflow table in config).

### Creating issues

Creating a ticket = writing an issue mirror that has no remote key yet, then pushing it. Publishing is the first push: same diff preview, same approval, same event.

- **Where drafts live**:
  - attached to a task (`tasks/<id>/issues/draft-<n>/`) when the ticket comes from my work (an Investigation spawning a Bug for another team, a Story I will implement);
  - standalone (`contexts/<ctx>/drafts/<id>/`) when I write a ticket for the backlog.
- **After publish**: the draft takes its remote key. If the issue is assigned to me, ingestion finds it by its key and attaches it to the existing task instead of creating a duplicate. If not, it is an event in the journal; I can keep a `waiting { on }` task to follow it, closed automatically with the issue.
- **Templates**: the kind templates are the ticket templates, overridable per context to match each team's conventions (story format, labels, components).
- **Ticket lint before publish**: loom fetches and caches the required fields per project and issue type (Jira create metadata, GitLab/GitHub labels and milestones) and validates the draft locally. A ticket written through loom also passes the readiness gate, so it is executable by the factory.
- **Manually**: `loom issue new --kind bug --to jira:CORE` opens `$EDITOR` on the template; TUI `n`.
- **With an agent**: `loom issue draft --from <file|url|->` or TUI `N`. A ticket-writer template (direct API, no code tools):
  - input: rough notes, a Slack thread, investigation findings, meeting notes, an epic description;
  - output: one or several typed drafts (title, description in the kind template, acceptance criteria, labels, parent and links, suggested estimate);
  - checks duplicates through loom search over indexed issues and flags `possible_duplicate_of`;
  - creating drafts is internal (automatic); publishing follows the autonomy policy of the context.
- **Batch**: an epic breakdown produces N linked drafts, reviewed and edited together in the TUI, published in one go (parent created first, then children with their links).
- Commands: `loom issue new | draft | edit | push | publish`.
