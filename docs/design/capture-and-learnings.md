# Capture and learnings

> What loom records about my work, and how learnings are produced and scoped.

## Capture

```json
{"ts":"…","machine":"…","context":"company-a","task":"k3q7zm2a","actor":"me|agent:<template>|daemon|rule:<id>","type":"status|note|commit|mr|review|agent_session|meeting|gate|tool_call","data":{}}
```

- **Automatic**: task lifecycle, my commits in indexed repos (linked by branch), forge activity, agent sessions (Claude Code hooks `SessionStart`/`SessionEnd`; transcript copied into `files/`, since Claude Code deletes transcripts after `cleanupPeriodDays`).
- **Semi-automatic**: meetings, imported as sources (see [Meetings](knowledge.md#meetings)).
- **Manual**: `loom log "…"`.
- **Excluded**: shell history, keystrokes, screen.
- Byproduct: daily timesheet suggestion per context (active intervals + commits + sessions + meetings).

otel is for loom's own diagnostics. The ledger is `events.jsonl`, never an observability backend.

## Learnings

Learnings are knowledge. See [Knowledge](knowledge.md). `loom done --learned "…"` stays optional and never blocks, and its text becomes a source for a page.
