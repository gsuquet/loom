-- State of each repository (its card), and the search table's `branch` column filled from it.

-- One row per repository, written after each refresh. Each state field holds the JSON of its
-- known value (`null` included: no upstream, no operation…); SQL NULL means unknown.
CREATE TABLE repo_state (
    repo_id INTEGER PRIMARY KEY REFERENCES repos (id) ON DELETE CASCADE,
    -- Unix seconds.
    refreshed_at INTEGER NOT NULL,
    -- `RepoHead`: `{"kind": "branch" | "unborn", "name": …}` or `{"kind": "detached", "commit": …}`.
    head TEXT CHECK (head IS NULL OR json_valid(head)),
    -- Number of tracked paths with a change.
    dirty TEXT CHECK (dirty IS NULL OR json_valid(dirty)),
    -- `Untracked`: `{"count": …, "capped": …}`.
    untracked TEXT CHECK (untracked IS NULL OR json_valid(untracked)),
    -- `RepoOperation` or `null`.
    operation TEXT CHECK (operation IS NULL OR json_valid(operation)),
    -- `Upstream` or `null`.
    upstream TEXT CHECK (upstream IS NULL OR json_valid(upstream)),
    -- Branch name or `null`.
    default_branch TEXT CHECK (default_branch IS NULL OR json_valid(default_branch)),
    -- RFC 3339 timestamp or `null`.
    last_commit TEXT CHECK (last_commit IS NULL OR json_valid(last_commit)),
    last_fetch TEXT CHECK (last_fetch IS NULL OR json_valid(last_fetch)),
    -- Why some fields are unknown.
    error TEXT
) STRICT;

-- The branch a repository is on, for the search table: empty when detached or unknown.
-- `repos_fts` rows are written by the triggers on `repos` (0001) and on `repo_state` below.

DROP TRIGGER repos_fts_insert;
DROP TRIGGER repos_fts_update;

CREATE TRIGGER repos_fts_insert AFTER INSERT ON repos BEGIN
    INSERT INTO repos_fts (rowid, name, slug, path, remotes, context, branch)
    VALUES (
        new.id, new.name, new.slug, new.path,
        (SELECT coalesce(group_concat(value ->> 'url', ' '), '') FROM json_each(new.remotes)),
        new.context,
        coalesce((SELECT head ->> '$.name' FROM repo_state WHERE repo_id = new.id), '')
    );
END;

CREATE TRIGGER repos_fts_update AFTER UPDATE ON repos BEGIN
    DELETE FROM repos_fts WHERE rowid = old.id;
    INSERT INTO repos_fts (rowid, name, slug, path, remotes, context, branch)
    VALUES (
        new.id, new.name, new.slug, new.path,
        (SELECT coalesce(group_concat(value ->> 'url', ' '), '') FROM json_each(new.remotes)),
        new.context,
        coalesce((SELECT head ->> '$.name' FROM repo_state WHERE repo_id = new.id), '')
    );
END;

CREATE TRIGGER repo_state_fts_insert AFTER INSERT ON repo_state BEGIN
    UPDATE repos_fts SET branch = coalesce(new.head ->> '$.name', '') WHERE rowid = new.repo_id;
END;

CREATE TRIGGER repo_state_fts_update AFTER UPDATE OF head ON repo_state BEGIN
    UPDATE repos_fts SET branch = coalesce(new.head ->> '$.name', '') WHERE rowid = new.repo_id;
END;

-- Deleted with its repository (cascade), whose search row is already gone then.
CREATE TRIGGER repo_state_fts_delete AFTER DELETE ON repo_state BEGIN
    UPDATE repos_fts SET branch = '' WHERE rowid = old.repo_id;
END;
