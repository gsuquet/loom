-- The search table folds diacritics fully (`remove_diacritics 2`, SQLite 3.27+): `é`, `ễ` and
-- `ç` match `e`, `e` and `c`. The default tokenizer of 0001 (`remove_diacritics 1`) leaves a
-- letter with several diacritics unfolded.
--
-- The tokenizer of an FTS5 table cannot change: the table is dropped, created again and
-- filled from `repos` and `repo_state`. The triggers of 0001 and 0002 name the table, not its
-- definition, and keep working.

DROP TABLE repos_fts;

CREATE VIRTUAL TABLE repos_fts USING fts5 (
    name, slug, path, remotes, context, branch,
    tokenize = 'unicode61 remove_diacritics 2'
);

INSERT INTO repos_fts (rowid, name, slug, path, remotes, context, branch)
SELECT
    r.id, r.name, r.slug, r.path,
    (SELECT coalesce(group_concat(value ->> 'url', ' '), '') FROM json_each(r.remotes)),
    r.context,
    coalesce((SELECT head ->> '$.name' FROM repo_state WHERE repo_id = r.id), '')
FROM repos r;
