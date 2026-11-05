-- Machine-local clones of each MR of a Review task (ADR-0016, decision 3): the clone that holds its
-- refs, and the clones whose refs are not deleted yet. Paths of this machine, never in a store.

CREATE TABLE mr_clones (
    task_id TEXT NOT NULL,
    mr_key  TEXT NOT NULL,
    path    TEXT NOT NULL,
    remote  TEXT NOT NULL,
    retired INTEGER NOT NULL CHECK (retired IN (0, 1)),
    PRIMARY KEY (task_id, mr_key, path, remote)
) STRICT;
