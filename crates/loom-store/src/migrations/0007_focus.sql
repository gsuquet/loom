-- Machine-local focus per context (ADR-0013, decision 9): at most one active task per context.

CREATE TABLE focus (
    context TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    since   INTEGER NOT NULL
) STRICT;
