# How loom search ranks tasks and repositories

> This page tells what `loom search` looks at. It tells why a task that you work on comes before a repository with the same name. It also tells why a search right after `loomd` starts can take a moment. Command: [`loom search`](../reference/cli/loom.md#loom-search). API and result format: [`GET /v1/search`](../reference/api.md#search), [`SearchResults`](../reference/api.md#searchresults). Storage: [how loom stores tasks](task-store.md).

## One query, two indexes

`index.db` holds two full-text tables (SQLite FTS5). One table indexes the repositories (name, slug, path, remote URLs, context, branch). The other table indexes the tasks (title, notes, link keys).

The two tables use the same tokenizer. The tokenizer cuts a field into words at each character that is not a letter or a digit. It also folds case and diacritics. The search cuts a query at whitespace. Each word of the query must be the start of a word in the result.

Thus, a search for `CORE-1042` looks for `core` followed by `1042`. A search for `!88` looks for `88`. A task with one of these link keys matches. A task whose notes say "see CORE-1042" also matches. For this reason, a task whose link key is exactly the query (case ignored) ranks first among the tasks of its group.

The search reads the two tables in one transaction. Thus, a search never mixes the repositories of one moment with the tasks of another moment.

## Groups before relevance

Full-text relevance alone tells which text holds these words best. It does not tell what you look for. The query `checkout` matches the repository `checkout`, the task that you fix in it, and the tasks that you closed there last year. The search puts first the results that you are most likely to act on:

1. tasks `active`, `ready` and `waiting`: the work in progress or next.
2. repositories: the places you work in.
3. tasks `inbox`, and tasks that loom cannot fully read (a status from a newer loom, an invalid `task.md`): still to look at.
4. tasks `done` and `cancelled`: history.

Archived tasks do not exist yet. When they exist, they will rank after all the other groups.

In a group, each kind keeps its own order. For tasks, the order is link key, then title, then the rest. At each of these levels, the most recently changed task comes first. For repositories, the order is exact name, name prefix, slug, then the rest. At each of these levels, the most recently used repository comes first. FTS5 relevance (`bm25`) only breaks ties, and the ID or the slug makes the order total: the same index always gives the same order.

`--type task` or `--type repo` keeps one kind. `-c` keeps one context.

## Right after loomd starts

`loomd` serves the API as soon as it listens. At the same time, it reconciles the task files with the index in the background. On an index that exists, this changes little: the rows of the previous run are already there. On a new or deleted index, the rows arrive one task at a time, and a search can miss most of them. Thus, a search that covers tasks waits for the end of that first reconciliation (at most 10 s), then it answers from what is indexed. When `loom search` starts `loomd`, it also waits for the first repository scan.

## Cost

The search ranks each match, then it cuts the list to `--limit`. If a word is in every task, the search reads every task row. It does not read the notes, only which column matched. On 1,000 repositories and 5,000 tasks, `loom search` answers in about 15 ms (debug build, median). This time includes the start of the `loom` process.
