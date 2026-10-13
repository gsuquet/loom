-- Tasks of every context store, indexed from their files (ADR-0013): rebuilt by a reindex.

-- `key` is the rowid of the search table; `id` is the key every query uses.
CREATE TABLE tasks (
    key INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    context TEXT NOT NULL,
    -- SHA-256 of `task.md` as last read or written by loomd, lowercase hex.
    hash TEXT NOT NULL,
    -- `Task` as JSON, NULL when the file is invalid.
    task TEXT CHECK (task IS NULL OR json_valid(task)),
    -- Why the file is invalid, NULL when it is valid.
    error TEXT,
    -- Empty when the file is invalid.
    title TEXT NOT NULL,
    -- Status name as written (an unknown one included), importance (`p0`…`p3`), due date
    -- (`YYYY-MM-DD`), pinned, last change (Unix seconds); NULL when the file is invalid.
    status TEXT,
    importance TEXT,
    due TEXT,
    pinned INTEGER CHECK (pinned IN (0, 1)),
    updated INTEGER,
    -- The notes, or the whole file when it is invalid.
    body TEXT NOT NULL,
    -- Keys of the origin and related links, separated by spaces.
    link_keys TEXT NOT NULL,
    CHECK ((task IS NULL) <> (error IS NULL))
) STRICT;

CREATE INDEX tasks_context ON tasks (context);

-- Same tokenizer as `repos_fts` (0004): case and diacritics folded.
CREATE VIRTUAL TABLE tasks_fts USING fts5 (
    title, body, link_keys,
    tokenize = 'unicode61 remove_diacritics 2'
);

-- The search table follows `tasks` through triggers, so that no write path can forget it.
CREATE TRIGGER tasks_fts_insert AFTER INSERT ON tasks BEGIN
    INSERT INTO tasks_fts (rowid, title, body, link_keys)
    VALUES (new.key, new.title, new.body, new.link_keys);
END;

CREATE TRIGGER tasks_fts_delete AFTER DELETE ON tasks BEGIN
    DELETE FROM tasks_fts WHERE rowid = old.key;
END;

CREATE TRIGGER tasks_fts_update AFTER UPDATE ON tasks BEGIN
    DELETE FROM tasks_fts WHERE rowid = old.key;
    INSERT INTO tasks_fts (rowid, title, body, link_keys)
    VALUES (new.key, new.title, new.body, new.link_keys);
END;
