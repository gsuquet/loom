-- Result of the last update of each repository (fetch, fast-forward), kept apart from its
-- state: a state refresh never erases it.

CREATE TABLE repo_update (
    repo_id INTEGER PRIMARY KEY REFERENCES repos (id) ON DELETE CASCADE,
    -- `LastUpdate.time`, Unix seconds.
    time INTEGER NOT NULL,
    -- `LastUpdate` as JSON: time, policy and outcomes.
    last_update TEXT NOT NULL CHECK (json_valid(last_update))
) STRICT;
