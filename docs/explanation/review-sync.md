# How loomd syncs the MRs to review

> This page tells how `loomd` finds the merge requests (MRs) where you are a requested reviewer, and how it keeps one Review task for each. It tells when the task opens and closes, which fields the sync changes, and what a poll writes. Endpoint: [`POST /v1/reviews/sync`](../reference/api.md#reviews). Command: [`loom review sync`](../reference/cli/loom.md#loom-review-sync). Files: [MR mirror](../reference/task-file.md#mrs-mr-mirror). Keys: [forges](../reference/configuration.md#forges). Decision: [ADR-0016](../adr/0016-mr-review-mirror.md).

## The problem

Review requests come from many forges, many projects and many clients. Each forge shows its own list. A request that you do not see in time blocks a colleague. A review that you did is not the end: when the author pushes new commits, you must look again, and the forge does not always tell you.

`loomd` polls each forge. Each MR to review becomes a Review task in the priority list, in the context of its project. The task closes when your review is done, and opens again when the MR changes after it.

## Which forges, which MRs

`loomd` polls each context held on this machine, for each forge host that the context has a credential for (`token_cmd` or `token_env`). A context with `machines` is polled only by the first machine of its list, so that two machines do not both create a task for one MR.

Each poll reads two sets of MRs:

- **The MRs to review.** GitHub: the search `is:pr is:open archived:false review-requested:@me` (requests to you and to your teams), or `user-review-requested:@me` when `review_requests = "direct"`. GitLab: the open MRs with `scope=reviews_for_me`. From the second poll on, GitLab lists only the MRs updated since the start of the last poll that succeeded, minus one minute.
- **The MRs of the Review tasks**, one by one, except a `done` or `cancelled` task of a merged or closed MR. This set is necessary: GitHub removes you from the requested reviewers when you submit a review, and the GitLab list does not show a request that was removed. If `loomd` read only the search, a new head after your review would never open the task again.

## The context of an MR

One token can see the projects of several contexts. A context keeps only the MRs of its own projects. The first rule that matches gives the context of an MR:

1. the context of an indexed clone of the project (when several clones exist: the most recently used one, then the shortest path);
2. the `match.hosts` rules on `<host>/<project path>`;
3. the context whose token found the MR, when each other held context with a credential for the host is polled on this machine with the token of another account.

One token, or two tokens of one account, find the same MRs. Then rule 3 tells nothing, and the MR goes to no store: each poll reports it as failed, and asks for a `match.hosts` rule or a clone of the project. `loomd` compares the logins of the tokens. A login is read at the first poll of its context and host. Thus, until each context polled the host once, rule 3 does not apply, and the MR is reported as failed.

`default_context` never applies: an MR that a client token finds must not go to your personal store. An MR of another context is not written, and the answer counts it in `other_context`.

For the same reason, `loomd` refuses to move a Review task to another context. The context of the MR sets the context of the task, and the next poll would create a second task in that context.

## When a task opens and closes

| The poll sees | Task status before | Task status after |
| --- | --- | --- |
| An open MR where you are requested, and no task | (none) | `inbox`, new task. No task when your verdict is on the current head: there is nothing to review. |
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

- At creation: the title (`Review <key>: <MR title>`), the kind `review`, the status `inbox`, and the `origin` link (provider, key, URL).
- Then: the status, only on the changes of the table above, each one with a note.

The sync never changes the importance, the due date, the pin, the notes or the title after creation. These fields are yours.

The key of the MR is an alias of the task: `loom show acme/shop#412` shows the Review task of that MR. The command encodes the key in the URL path (`/v1/tasks/acme%2Fshop%23412`).

## What a poll writes

The MR mirror is in the task directory, in `mrs/<provider>-<key>/`. `loomd` owns these files.

A poll writes nothing when nothing changed:

- `loomd` reads each MR with the `ETag` of its last read. A `304 Not Modified` gives no new data. On GitHub, it also costs no request of the rate limit.
- When an MR changed, `loomd` reads its reviews (GitHub) or its versions and approvals (GitLab), and its threads.
- The threads and your verdict are also read again one hour after their last read, because a resolved thread or a review does not always change the MR.
- A `done` or `cancelled` task of a merged or closed MR is not read again. Only the archive moves it.
- `loomd` writes a file only when its bytes change, with an atomic rename.
- Each task that changed gets one commit of its store per poll. The message is the message of its events, otherwise `mr(<id>): <key> mirror updated`.

`loomd` keeps the `ETag`s in memory. After a restart, the first poll reads each MR in full. The files do not change if the MR did not change.

## When a forge fails

- A host that does not answer, or that refuses the token, stops its own poll only. The other hosts go on.
- The mirrors keep their last snapshot. The MRs that the stopped poll read before the failure are applied all the same. Thus, one MR that fails does not hold back the others. The answer of `loom review sync` and the log of `loomd` give the error. The answer also gives the time of the last poll that succeeded. `loomd` keeps this time in memory only: after a restart, it is empty until a poll succeeds.
- The next poll waits 2, then 4, then 8 times the interval, never more.
- A rate limit (`429`, or `403` with no remaining requests) stops the poll until the reset.
- When the token is refused (`401`), `loomd` gets the token again at the next poll: it can be a new one.
- A failure of one MR (for example, a `404`) stops only that MR. Its mirror keeps its snapshot, and the next poll reads it in full.
- When `loomd` stops, a poll that runs stops before its next forge call.

## Limits

- GitLab: only the approvals give a verdict. A comment without an approval is not a verdict, and a reset of the state of a reviewer is not a new request.
- GitHub: a request to one of your teams counts as a request to you, unless `review_requests = "direct"`.
- An archived Review task is not changed. After the archive, a new task comes only when you are requested: on a new head without your verdict, or by a new request, against what the last poll saw.
- An MR whose web URL is on another host than the forge host of `[forges]` gets no task: the poll reports it as failed.
- A review and a new request that both occur between two polls are not seen as a new request.
- GitLab, without a task: your approval is taken as a verdict on each new head. Thus, an MR that you approved before the first poll gets no task, even after a push, in a project that keeps the approvals.
- An MR that the forge no longer shows (deleted project, lost access) fails at each poll until you close its task.
- GitLab, in a project that keeps the approvals: after a push opens the task again, the approval that stays does not close it. Approve again after a reset, or close the task.
- If a task that `loomd` cannot read (a broken hand edit) holds the mirror of an MR, the poll creates no second task: it reports the MR as failed until you fix the file.
- When an MR without a task fails, GitLab lists the MRs from the same time again at the next poll.
- `loom review sync` exits with status 0 when a forge limits the rate: the answer gives the time of the reset.
