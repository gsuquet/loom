# How loomd keeps the refs and the diffs of an MR

> This page tells how `loomd` fetches the head of a merge request (MR) into your clone, how it keeps a diff for each version, and how it removes the refs. Files: [MR mirror](../reference/task-file.md#mrs-mr-mirror). Poll: [review sync](review-sync.md). Decision: [ADR-0016](../adr/0016-mr-review-mirror.md).

## The problem

A forge keeps the head of each MR under a ref, but only the last head. After a force push, the old head has no ref, and `git gc` can remove it. You must still compare the new version with the version that you reviewed.

A comparison of the two heads does not give the correct result after a rebase. The command `git diff <old head> <new head>` also shows each change that came from the base branch.

## Which clone

`loomd` looks in the repository index for a repository with a remote that names the host and the path of the target project. The name of that remote does not matter. For a fork as `origin` and the project as `upstream`, `loomd` uses `upstream`. If two remotes name the project, the primary remote is the choice.

If several clones or worktrees have such a remote, `loomd` uses this order:

1. The clone with the most recent use. The use of a linked worktree counts as a use of its clone.
2. The main worktree of that clone.
3. The shortest path.

`loomd` keeps the choice in the `clone` field of `mr.json`. The choice changes only when that clone is gone from the disk or from the index, or no longer has a remote of the project.

## When loomd fetches

A separate thread does this work after each poll of a context and a forge host. It reads the Review tasks of that poll, and looks at the current head of each open MR. If that head has no recorded patch, the thread fetches the MR and writes the patch.

This step has its own failures:

- A git failure does not fail, delay or change the mirror sync. `loomd` writes the failure to its log.
- A lock of another git process is not a failure. `loomd` does not remove the lock.
- In both cases, the next poll does the work again, because the patch is not recorded.
- After a git failure, the other MRs of the host wait for the next poll. Each of them could wait for the full timeout.

A poll that finds no new head starts no fetch. `loomd` fetches only for tasks that exist. The merged and closed MRs get no new diff.

## What the fetch writes in your clone

The fetch writes only these refs. `<n>` is the number of the MR.

| Ref | Content |
| --- | --- |
| `refs/loom/mr/<remote>/<n>/head` | The head of the MR. Each fetch replaces it. |
| `refs/loom/mr/<remote>/<n>/base` | The tip of the target branch. It brings the objects. No diff reads it. |
| `refs/loom/mr/<remote>/<n>/reviewed` | The head of your last verdict. It holds the commit through force pushes and `git gc`. |

The command is:

```text
git fetch --no-tags --no-write-fetch-head --no-prune --no-prune-tags --no-recurse-submodules --refmap= -- <remote> +refs/pull/<n>/head:refs/loom/mr/<remote>/<n>/head +refs/heads/<base>:refs/loom/mr/<remote>/<n>/base
```

GitLab uses `refs/merge-requests/<n>/head` as the source of the head. The flags prevent changes outside `refs/loom/`:

- `--refmap=` stops the update of `refs/remotes/<remote>/<base>`, which git otherwise does when it fetches a branch by name.
- The other flags hold against your configuration, such as `fetch.prune` or `remote.<name>.tagOpt`.

The fetch does not touch your branches, your index or your working tree. Thus, a dirty working tree or an operation in progress does not stop it.

The fetch uses the same environment as [repository updates](repo-updates.md): `GIT_TERMINAL_PROMPT=0`, SSH `BatchMode` if you did not configure an SSH command, your credential helpers, and a timeout of 120 seconds. `loomd` does not give the forge token to git. The `update` policy of a repository does not apply, because this fetch moves no branch.

## The diff of a version

The diff of a version is `git diff -M <merge base> <head>`. GitLab gives the merge base. For GitHub, `loomd` runs `git merge-base <base.sha> <head>` with the base tip of that version, and writes the result to `versions.json`. A later tip of the target branch would move the hunks away from the hunks of the forge.

`loomd` sets the options of `git diff` itself, for example the prefixes and the number of context lines. Thus, your diff configuration does not change the patch. It writes the patch to `diffs/<sha>.patch` in the task directory, and sets `diff` to `local` in `versions.json`. The patch stays after the clone is gone. The store backup holds the patches, as it holds the other files of the task.

## Since the last review

The list of files to read again comes from the patches of the two versions, not from the two heads. For each file, `loomd` computes a hash of its normalized patch.

- The hash reads the `+`, `-` and context lines, and the `\ No newline at end of file` line.
- The hash does not read the `index` line, the line numbers or the section text of the `@@` lines. Thus, a rebase that only moves the lines of a file keeps its hash.
- For a file without hunks, the hash reads what the header tells: the mode, the rename source, or, for a binary file, the `index` line.

A file is in the list when its hash changed, when it is new in the diff, or when it left the diff. Example from the fixtures: between the two versions, `git diff` of the heads shows `README.md` and `src/auth/jwt.rs`, but only `src/auth/jwt.rs` is in the list.

## Without a clone

An MR without a clone still syncs. `loomd` reads the per-file patches of the forge, writes them to `diffs/<sha>.patch`, and sets `diff` to `reduced` in `versions.json`. A reduced patch has limits:

- It has no context expansion.
- It has only the current head.
- A binary or large file has no hunks. One line that starts with `# ` tells this.

If a clone comes later, the next poll replaces the reduced patch of the current head with the full one.

## Cleanup

After a task is archived, the next poll of its context and host deletes its refs with `git update-ref -d`. Only the refs of that MR go. The patches stay in the archived task.

The refs stay when an active Review task of any context has the same MR, because a clone has one set of refs for each MR. If a lock or a failure stops the deletion, the next poll does it again. After a restart, `loomd` looks at each archived task one more time.

## Limits

- A clone that this machine does not have is not a choice. If two machines poll one context, each one can write its own clone to `mr.json`. Set `machines` on the context, so that one machine polls it.
- Your last verdict gives the `reviewed` ref. If you never gave a verdict on a version, `git gc` can remove its commit after a force push. Its patch stays.
- A version whose head is not in the clone gets no patch. This occurs for an old GitLab version: a later force push replaced its head before the first fetch.
