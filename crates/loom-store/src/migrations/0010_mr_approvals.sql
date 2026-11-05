-- Machine-local: the head of the poll that first showed my approval of a GitLab MR that has no
-- Review task (GitLab does not tell on which head I approved). Rebuildable: a lost row dates the
-- approval to the head of the next poll.

CREATE TABLE mr_approvals (
    context    TEXT NOT NULL,
    host       TEXT NOT NULL,
    mr_key     TEXT NOT NULL,
    head_sha   TEXT NOT NULL,
    project_id INTEGER NOT NULL,
    PRIMARY KEY (context, host, mr_key)
) STRICT;
