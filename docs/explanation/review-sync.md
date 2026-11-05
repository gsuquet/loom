# How loomd syncs the MRs to review

> This page tells how `loomd` finds the merge requests (MRs) where you are a requested reviewer, how it keeps them in a queue until you start on one, and how it keeps one Review task for each MR that you started. It tells when an entry comes and goes, when a task opens and closes, which fields the sync changes, and what a poll writes. Endpoints: [reviews](../reference/api.md#reviews). Commands: [`loom review sync`](../reference/cli/loom.md#loom-review-sync), [`loom review queue`](../reference/cli/loom.md#loom-review-queue), [`loom review start`](../reference/cli/loom.md#loom-review-start). Files: [queue entry](../reference/task-file.md#queuemrs-queue-entry), [MR mirror](../reference/task-file.md#mrs-mr-mirror). Keys: [forges](../reference/configuration.md#forges). Refs and diffs of an MR: [MR refs and diffs](mr-refs.md). Decision: [ADR-0016](../adr/0016-mr-review-mirror.md).

## The problem

Review requests come from many forges, many projects and many clients. Each forge shows its own list. A request that you do not see in time blocks a colleague. A review that you did is not the end: when the author pushes new commits, you must look again, and the forge does not always tell you.

`loomd` polls each forge. Each MR to review becomes a queue entry, in the context of its project. An entry is a file, not a task: it does not go to the inbox, and you do not close it. It goes when the MR no longer waits on you. When you start on an MR, it becomes a Review task with a copy of the MR. The task closes when your review is done, and opens again when the MR changes after it.

## Which forges, which MRs

`loomd` polls each context held on this machine, for each forge host that the context has a credential for (`token_cmd` or `token_env`). A context with `machines` is polled only by the first machine of its list, so that two machines do not both write an entry for one MR. A context without `machines` is polled on each machine that holds it, and each `loomd` writes this warning at start, because a start on each of two machines that share the store creates two tasks for one MR:

```text
WARN MRs are polled on every machine holding the context: set `machines` so that only its first machine writes their queue entries context=company-a host=github.com
```

Each poll reads three sets of MRs:

- **The MRs to review.** GitHub: the search `is:pr is:open archived:false review-requested:@me` (requests to you and to your teams), or `user-review-requested:@me` when `review_requests = "direct"`. GitLab: the open MRs with `scope=reviews_for_me`. From the second poll on, GitLab lists only the MRs updated since the start of the last poll that succeeded, minus one minute.
- **The MRs of the queue**, one by one. This set is necessary: the search and the list do not show an MR whose request was removed, or that is merged or closed. Without this read, its entry would stay.
- **The MRs of the Review tasks**, one by one, except a `done` or `cancelled` task of a merged or closed MR. This set is necessary: GitHub removes you from the requested reviewers when you submit a review, and the GitLab list does not show a request that was removed. If `loomd` read only the search, a new head after your review would never open the task again.

## The context of an MR

One token can see the projects of several contexts. A context keeps only the MRs of its own projects. The first rule that matches gives the context of an MR:

1. the context of an indexed clone of the project (when several clones exist: the most recently used one, then the shortest path);
2. the `match.hosts` rules on `<host>/<project path>`;
3. the context whose token found the MR, when each other context with a credential for the host is polled on this machine with the token of another account. A context that this machine does not poll counts too: another machine can poll it with the same account.

One token, or two tokens of one account, find the same MRs. Then rule 3 tells nothing, and the MR goes to no store: each poll reports it as failed, and asks for a `match.hosts` rule or a clone of the project. `loomd` compares the logins of the tokens. A login is read at the first poll of its context and host that succeeds, and is kept after a refused token. Thus, until each context polled the host once with success, rule 3 does not apply, and the MR is reported as failed. A context whose token cannot be read (for example, a locked password manager) stops rule 3 for the other contexts of the host until its first poll succeeds.

`default_context` never applies: an MR that a client token finds must not go to your personal store. An MR of another context is not written, and the answer counts it in `other_context`.

For the same reason, `loomd` refuses to move a Review task to another context. A task stays in its context: a start and a poll find it in any context, so the MR gets no entry and no second task in the context that a rule now names.

## When an entry comes and goes

An MR has an entry when all these conditions are true: the MR is open, you are a requested reviewer, your verdict is not on its current head or a request is newer than your verdict, you are not its author, and it has no Review task in any context. `loomd` adds the entry at the first poll that sees this. `loomd` deletes the entry at the first poll that sees that one condition is false. The removal writes no event, because no task exists.

| The poll sees | Entry before | Entry after |
| --- | --- | --- |
| An open MR where you are requested, no task | none | added |
| The same MR, changed on the forge (title, head) | present | stays, its fields follow the MR |
| Your request is removed | present | deleted |
| Your verdict on the current head, and no newer request | present | deleted |
| You are requested again after your verdict, on the same head | none | added |
| A push on an MR that you approved, in a project that keeps the approvals | none | added |
| The MR is merged or closed | present | deleted |
| The MR has a Review task that is not archived, in any status, in any context | present | deleted |
| The MR is of another context now, that this machine polls with the same account | present | moved to the store of that context |
| The MR is of another context now, in other cases | present | no change, and the poll reports it as failed |
| The forge answers `404` for the MR, and the host answers | present | deleted, as for a closed MR |
| A list or another read fails (a `403`, a `500`, no answer) | any | no change |
| An incomplete GitHub search does not show the MR | present | no change |

An entry has one or more reasons. The only reason now is `review_requested`.

A request is newer than your verdict in these cases. On GitHub, you are in the requested reviewers: GitHub removes you when you submit a review. On GitLab, your reviewer state is `unreviewed` on an MR that you approved. `loomd` reads the state only for an approved MR without a task. The GitLab documentation does not tell what a new request does to the state: this behavior is not verified on a real GitLab.

GitLab does not tell on which head you approved. For an MR without a task, `loomd` keeps the head of the poll that first shows your approval, in `index.db` (machine-local, no commit). It reads such an MR one by one until it leaves, then deletes the row. A push after your approval, in a project that keeps the approvals, gives an entry. If you delete `index.db`, your approval counts for the head of the next poll.

Your own MR never has an entry for `review_requested`, even when GitLab lets you request your own review.

After an archived Review task, the mirror of that task tells what the last poll saw. A new entry needs a new head or a new request against that mirror. The archived task does not change. The start then creates a new task.

## When you start on an MR

`loom review start <key>` and [`POST /v1/reviews/queue/{key}/start`](../reference/api.md#reviews) read the MR on the forge, create the Review task in `active` with its mirror, and delete the entry. You do not wait for the next poll. The event of the creation has your actor, because the start is your action. The start does not change the focus of the context (`loom go`).

The poller thread does the start. A start waits for a poll that runs, and a poll waits for a start that runs. Thus, a poll cannot apply what it read before the start. The task thread stays the only writer of the store.

| Case | Result |
| --- | --- |
| The MR has a Review task that is not archived, in any context | That task, with `created` false. Nothing is written, and the forge is not called. |
| The forge does not answer, refuses the token or limits the rate | `502 forge`. The entry stays, no task is created. Do the start again. |
| The MR is merged or closed since the last poll | `409 conflict`. No task, and the entry is deleted. |
| The forge answers `404` for the MR | `404 not_found`. No task, and the entry is deleted. |
| No entry and no Review task | `404 not_found`. A start creates no task for an MR that is not in the queue. |
| Two contexts have this key | `400 invalid_request`. Name the context with `-c`. |
| This machine does not poll the context and host of the entry (another machine polls it, or the credential was removed) | `400 invalid_request`. Start on the first machine of the context, or add the credential. A task created on another machine is not known to the poll until a store sync, and a second task could come from it. |

If `loomd` stops between the creation of the task and the removal of the entry, the next start answers with the task, and the next poll deletes the entry.

## When a task opens and closes

The MR of a Review task that exists follows this table, also when the task was created before the queue existed. Such a task stays tracked, and it has no entry.

| The poll sees | Task status before | Task status after |
| --- | --- | --- |
| The MR is merged (open at the last poll) | open | `done` |
| The MR is closed (open at the last poll) | open | `cancelled` |
| Your verdict (approve, request changes, comment) on the current head | open | `done` |
| Your request is removed, after your verdict on the current head | open | `done` |
| Your request is removed, after your verdict on an older head | open | no change: the new head is still to review |
| Your request is removed, with no verdict | open | `cancelled` |
| A new head after the head of the last poll, without your verdict on it | `done` | `ready` |
| You are requested again (requested now, not at the last poll) | `done`, `cancelled` | `ready` |
| The MR is open again after a close, and you are requested | `cancelled` | `ready` |

An open status is `inbox`, `ready`, `active` or `waiting`. Each status change has a note before it, with the reason, for example `MR acme/shop#412: new head 84280c4 after 7ebf283, reopened`. The events have the actor `daemon`.

The sync compares two polls, not the state of one poll. A request that is removed is a change from "requested" to "not requested". Thus, if you close a task by hand, the next poll does not open it again because you are still requested. Only a new head or a new request opens it again.

GitHub gives the verdict from the reviews of the MR: your last review that approves, requests changes or comments, on a commit. A new review is a new verdict.

GitLab gives the verdict from the approvals. GitLab does not tell on which head you approved. Thus, `loomd` takes the head of the poll that first sees your approval. An approval that stays after a push is not a new verdict: some projects do not reset the approvals on a push, and you did not review the push. An approval that comes back after a reset is a new verdict.

`loomd` keeps the last verdict in `sync.json`. Thus, when GitLab resets the approvals after a push, the task still knows that a verdict was given.

When the search of GitHub is incomplete, an MR that is not in the results keeps the request state of the last poll. Thus, an incomplete search does not cancel a task.

## What the sync changes

The sync owns few fields of a Review task:

- At the start: the title (`Review <key>: <MR title>`), the kind `review`, the status `active`, and the `origin` link (provider, key, URL).
- Then: the status, only on the changes of the table above, each one with a note.

The sync never changes the importance, the due date, the pin, the notes or the title after creation. These fields are yours.

The key of the MR is an alias of the task: `loom show acme/shop#412` shows the Review task of that MR. The command encodes the key in the URL path (`/v1/tasks/acme%2Fshop%23412`).

## What a poll writes

The MR mirror is in the task directory, in `mrs/<provider>-<key>/`. `loomd` owns these files.

An entry is in `queue/mrs/<provider>-<key>.json` in the store. It is one file per MR, and `loomd` owns it. The MR mirror is written only for an MR with a Review task: a poll does not read the threads or the versions of an MR that has only an entry.

A poll writes nothing when nothing changed:

- `loomd` reads each MR with the `ETag` of its last read. A `304 Not Modified` gives no new data. On GitHub, it also costs no request of the rate limit.
- When an MR changed, `loomd` reads its reviews (GitHub) or its versions and approvals (GitLab), and its threads.
- The threads and your verdict are also read again one hour after their last read, because a resolved thread or a review does not always change the MR.
- A `done` or `cancelled` task of a merged or closed MR is not read again. Only the archive moves it.
- `loomd` writes a file only when its bytes change, with an atomic rename.
- The refs and the patches come after the poll or the start, in a separate step, and only for an MR with a Review task. A failure of that step does not change the result of the poll. See [MR refs and diffs](mr-refs.md).
- Each task that changed gets one commit of its store per poll. The message is the message of its events, otherwise `mr(<id>): <key> mirror updated`.
- The entries that changed get one commit of the store per poll. The message is `queue: <n> added, <n> updated, <n> removed`.

`loomd` keeps the `ETag`s in memory. After a restart, the first poll reads each MR in full. The files do not change if the MR did not change.

## When a forge fails

- A host that does not answer, or that refuses the token, stops its own poll only. The other hosts go on.
- The entries do not change, and the mirrors keep their last snapshot. The MRs that the stopped poll read before the failure are applied all the same. Thus, one MR that fails does not hold back the others. The answer of `loom review sync` and the log of `loomd` give the error. The answer also gives the time of the last poll that succeeded. `loomd` keeps this time in memory only: after a restart, it is empty until a poll succeeds.
- The next poll waits 2, then 4, then 8 times the interval, never more.
- A rate limit (`429`, or `403` with no remaining requests) stops the poll until the reset.
- When the token is refused (`401`), `loomd` gets the token again at the next poll: it can be a new one.
- A failure of one MR (for example, a `404` for an MR with a Review task, or a `403`) stops only that MR. Its entry or its mirror does not change, and the next poll reads it in full. A `404` for an MR with only an entry is not a failure: the entry is deleted. A lost access looks the same: the entry comes back when a list shows the MR again.
- When `loomd` stops, a poll that runs stops before its next forge call.

## Limits

- GitLab: only the approvals give a verdict. A comment without an approval is not a verdict, and a reset of the state of a reviewer is not a new request.
- GitHub: a request to one of your teams counts as a request to you, unless `review_requests = "direct"`.
- Rule 3 of the context: when the tokens of two accounts are both requested on one MR (for example, both accounts are in the requested team), each context has an entry for it. The first start gives the one task, and the entry of the other context goes at its next poll.
- An archived Review task is not changed. After the archive, a new entry comes only when you are requested: on a new head without your verdict, or by a new request, against what the last poll saw.
- GitHub: a request again through a team, on a head that has your verdict, gives no entry when the MR has no Review task. A team request does not put you in the requested reviewers, so `loomd` cannot see that it is after your verdict. A Review task that exists opens again in this case: it compares two polls.
- GitLab, without a task: an approval that stays after a push is not a verdict on the new head, and the entry stays even when you read the push. Approve again after a reset, or start on the MR.
- If you remove the credential of a context, its entries stay in the list and are not polled. A start answers `400 invalid_request`.
- An MR whose web URL is on another host than the forge host of `[forges]` gets no entry: the poll reports it as failed.
- Two forge hosts of one kind, in one context, with the same key: the first MR has the entry, and the poll reports the second as failed until the first entry goes.
- A review and a new request that both occur between two polls are not seen as a new request.
- An MR with a Review task that the forge no longer shows (deleted project, lost access) fails at each poll until you close its task.
- An entry file that `loomd` cannot read (a broken hand edit) is not in the list and is not polled. The next poll that finds the MR writes it again.
- GitLab, in a project that keeps the approvals: after a push opens the task again, the approval that stays does not close it. Approve again after a reset, or close the task.
- If a task that `loomd` cannot read (a broken hand edit) holds the mirror of an MR, the MR gets no entry and no second task: the poll reports it as failed until you fix the file.
- When an MR that the GitLab list found fails, and it has no task, GitLab lists the MRs from the same time again at the next poll.
- `loom review sync` exits with status 0 when a forge limits the rate: the answer gives the time of the reset.
