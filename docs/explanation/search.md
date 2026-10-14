# How loom search ranks tasks and repositories

> What `loom search` looks at, why a task you are working on comes before a repository with the same name, and why a search right after `loomd` starts may take a moment. Command: [`loom search`](../reference/cli/loom.md#loom-search); API and result format: [`GET /v1/search`](../reference/api.md#search), [`SearchResults`](../reference/api.md#searchresults); storage: [how loom stores tasks](task-store.md).

## One query, two indexes

`index.db` holds two full-text tables (SQLite FTS5): one over the repositories (name, slug, path, remote URLs, context, branch), one over the tasks (title, notes, link keys). Both use the same tokenizer: a field is cut into words on every character that is not a letter or a digit, case and diacritics folded. A query is cut on whitespace, and every one of its words must start a word of the result.

So `CORE-1042` is searched as `core` followed by `1042`, and `!88` as `88`. A task finds them in its link keys; a task whose notes say "see CORE-1042" matches too. To tell them apart, a task whose link key is exactly the query (case ignored) ranks first among the tasks of its group.

Both tables are read in one transaction: a search never mixes the repositories of one moment with the tasks of another.

## Groups before relevance

Full-text relevance alone answers "which text holds these words best", not "what am I after". The query `checkout` matches the repository `checkout`, the task you are fixing in it, and the tasks you closed there last year. The ranking puts first what you are most likely to act on:

1. tasks `active`, `ready` and `waiting`: the work in progress or next;
2. repositories: the places you work in;
3. tasks `inbox`, and tasks loom cannot fully read (a status from a newer loom, an invalid `task.md`): still to look at;
4. tasks `done` and `cancelled`: history.

Archived tasks do not exist yet; when they do, they will rank after every other group.

Within a group, each kind keeps its own order. Tasks: link key, then title, then the rest; most recently changed first. Repositories: exact name, name prefix, slug, then the rest; most recently used first. FTS5 relevance (`bm25`) only breaks ties, and the ID or the slug makes the order total, so the same index always gives the same order.

`--type task` or `--type repo` keeps one kind; `-c` keeps one context.

## Right after loomd starts

`loomd` serves the API as soon as it listens, while it reconciles the task files with the index in the background. On an existing index this changes little: the rows of the previous run are already there. On a new or deleted index, the rows arrive one task at a time, and a search could miss most of them. A search that covers tasks therefore waits for the end of that first reconciliation, at most 10 s, then answers from what is indexed. `loom search` itself, when it starts `loomd`, also waits for the first repository scan.

## Cost

Every match is ranked, then the list is cut to `--limit`: a word found in every task reads every task row, but never its notes, only which column matched. On 1,000 repositories and 5,000 tasks, `loom search` answers in about 15 ms (debug build, median), including the start of the `loom` process.
