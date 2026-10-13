-- Repositories of the index and their full-text search table (ADR-0012).

-- `id` is the rowid of the search table. An explicit INTEGER PRIMARY KEY keeps it stable
-- across VACUUM; `path` is the key every query uses.
CREATE TABLE repos (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    -- Context name, or `unassigned`.
    context TEXT NOT NULL,
    -- `ContextAssignment` as JSON: the rule that assigned the context.
    assignment TEXT NOT NULL CHECK (json_valid(assignment)),
    -- `Remote` list as JSON, in configuration order.
    remotes TEXT NOT NULL CHECK (json_valid(remotes) AND json_type(remotes) = 'array'),
    primary_remote TEXT,
    forge TEXT CHECK (forge IN ('github', 'gitlab')),
    -- Added by registration: scans keep it while it exists on disk.
    registered INTEGER NOT NULL DEFAULT 0 CHECK (registered IN (0, 1)),
    -- Unix seconds.
    first_seen INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_used INTEGER
) STRICT;

CREATE INDEX repos_context ON repos (context);

-- Searched columns. `remotes` holds the remote URLs separated by spaces; `branch` stays empty
-- until the repository state is indexed.
CREATE VIRTUAL TABLE repos_fts USING fts5 (name, slug, path, remotes, context, branch);

-- The search table follows `repos` through triggers, so that no write path can forget it.
CREATE TRIGGER repos_fts_insert AFTER INSERT ON repos BEGIN
    INSERT INTO repos_fts (rowid, name, slug, path, remotes, context, branch)
    VALUES (
        new.id, new.name, new.slug, new.path,
        (SELECT coalesce(group_concat(value ->> 'url', ' '), '') FROM json_each(new.remotes)),
        new.context, ''
    );
END;

CREATE TRIGGER repos_fts_delete AFTER DELETE ON repos BEGIN
    DELETE FROM repos_fts WHERE rowid = old.id;
END;

CREATE TRIGGER repos_fts_update AFTER UPDATE ON repos BEGIN
    DELETE FROM repos_fts WHERE rowid = old.id;
    INSERT INTO repos_fts (rowid, name, slug, path, remotes, context, branch)
    VALUES (
        new.id, new.name, new.slug, new.path,
        (SELECT coalesce(group_concat(value ->> 'url', ' '), '') FROM json_each(new.remotes)),
        new.context, ''
    );
END;
