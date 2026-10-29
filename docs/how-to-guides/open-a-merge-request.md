# Open a merge request

> Push the branch of a task and open a draft MR for it, linked to the task, with one command.

Before you start, make sure that:

- The context of the task has a forge credential for the host of the remote, with write access to pull requests or merge requests ([connect a forge](connect-a-forge.md)).
- The task is the focus of its context (`loom go <id>`), or you know its ID.
- You are in the repository, on the branch of the change, with at least one commit that the target branch does not have.

## Open the draft MR

1. Make sure that loom knows the default branch of the remote. If you cloned the repository, git knows it. Otherwise, run:

   ```sh
   git remote set-head origin --auto
   ```

2. Look at what `loom pr` will do. The command pushes nothing and sends nothing:

   ```sh
   loom pr -n
   ```

   The output shows the `git push` command, the search of the open MRs of the branch, and the request that opens the draft, with its title and description. The title is the task title. The description is the template of the task kind and the origin and related links of the task. The notes of the task are not sent.

3. Push the branch and open the draft MR:

   ```sh
   loom pr
   ```

   To link another task than the focus task, give its ID: `loom pr k3q`. To open the MR into another branch than the default branch, use `--base`: `loom pr --base release/2026.10`.

The output gives the key and the URL of the MR:

```text
pushed fix/login-timeout to origin
opened draft MR acme/shop#412 https://github.com/acme/shop/pull/412
task k3q7zm2a: delivery link acme/shop#412
```

The MR is a `delivery` link of the task (`loom show k3q --json`). The task log has an `mr` event (the Events tab of the TUI).

## If the branch has an MR already

`loom pr` opens no second MR. It links the open MR of the branch to the task: the output says `linked open MR`. Run `loom pr` again after more commits: it pushes the commits, and the task keeps one link to the MR.

## If the command fails

- `is the default branch` or `has no commit ahead`: create a branch and commit the change first.
- `git push … failed`: git refused the push, for example a hook of the remote or your git credentials. The command shows the git error and opens no MR. Fix the cause, then run `loom pr` again.
- `has no forge credential`: add a token for the host to the context ([connect a forge](connect-a-forge.md)), then restart `loomd`.
- `the branch is pushed`: the push succeeded, but the forge refused the MR. The message gives the reason of the forge. Fix the cause, then run `loom pr` again: it pushes nothing new and opens the MR.

The draft MR is ready for review only when you mark it ready on the forge. `loom pr` does not request reviewers or set labels.
