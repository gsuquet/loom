-- Forge metadata of each repository (project ID, default branch, visibility…), read by the
-- forge poll, kept apart from its state: a state refresh or a scan never erases it.

CREATE TABLE repo_forge (
    repo_id INTEGER PRIMARY KEY REFERENCES repos (id) ON DELETE CASCADE,
    -- `ForgeMetadata.refreshed_at`, Unix seconds.
    refreshed_at INTEGER NOT NULL,
    -- `ForgeMetadata` as JSON: refreshed_at, project (or unknown) and error.
    metadata TEXT NOT NULL CHECK (json_valid(metadata))
) STRICT;
