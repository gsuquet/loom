# How loomd syncs the MRs to review

> This page tells how `loomd` finds the merge requests (MRs) where you are a requested reviewer, and how it keeps one Review task for each. It tells when the task opens and closes, which fields the sync changes, and what a poll writes. Endpoint: [`POST /v1/reviews/sync`](../reference/api.md#reviews). Command: [`loom review sync`](../reference/cli/loom.md#loom-review-sync). Files: [MR mirror](../reference/task-file.md#mrs-mr-mirror). Keys: [forges](../reference/configuration.md#forges). Decision: [ADR-0016](../adr/0016-mr-review-mirror.md).

## The problem

Review requests come from many forges, many projects and many clients. Each forge shows its own list. A request that you do not see in time blocks a colleague. A review that you did is not the end: when the author pushes new commits, you must look again, and the forge does not always tell you.

`loomd` polls each forge. Each MR to review becomes a Review task in the priority list, in the context of its project. The task closes when your review is done, and opens again when the MR changes after it.

## Which forges, which MRs

`loomd` polls each context held on this machine, for each forge host that the context has a credential for (`token_cmd` or `token_env`). A context with `machines` is polled only by the first machine of its list, so that two machines do not both create a task for one MR.

Each poll reads two sets of MRs:

- **The MRs to review.** GitHub: the search `is:pr is:open archived:false review-requested:@me` (requests to you and to your teams), or `user-review-requested:@me` when `review_requests = "direct"`. GitLab: the open MRs with `scope=reviews_for_me`. From the second poll on, GitLab lists only the MRs updated since the start of the last poll that succeeded, minus one minute.
- **The MRs of the open Review tasks**, one by one. This set is necessary: GitHub removes you from the requested reviewers when you submit a review, and the GitLab list does not show a request that was removed. If `loomd` read only the search, a new head after your review would never open the task again.

## The context of an MR

One token can see the projects of several contexts. A context keeps only the MRs of its own projects. The first rule that matches gives the context of an MR:

1. the context of an indexed clone of the project (when several clones exist: the most recently used one, then the shortest path);
2. the `match.hosts` rules on `<host>/<project path>`;
3. the context whose credential found the MR.

`default_context` never applies: an MR that a client token finds must not go to your personal store. An MR of another context is not written, and the answer counts it in `other_context`.

For the same reason, `loomd` refuses to move a Review task to another context. The context of the MR sets the context of the task, and the next poll would put it back.

## When a task opens and closes

| The poll sees | Task status before | Task status after |
| --- | --- | --- |
| An open MR where you are requested, and no task | (none) | `inbox`, new task |
| The MR is merged | open | `done` |
| The MR is closed | open | `cancelled` |
| Your verdict (approve, request changes, comment) on the current head | open | `done` |
| Your request is removed, after your verdict | open | `done` |
| Your request is removed, with no verdict | open | `cancelled` |
| A new head after the head of the last poll | `done` | `ready` |
| You are requested again (requested now, not at the last poll) | `done`, `cancelled` | `ready` |

An open status is `inbox`, `ready`, `active` or `waiting`. Each status change has a note before it, with the reason, for example `MR acme/shop#412: new head 84280c4 after 7ebf283, reopened`. The events have the actor `daemon`.

The sync compares two polls, not the state of one poll. A request that is removed is a change from "requested" to "not requested". Thus, if you close a task by hand, the next poll does not open it again because you are still requested. Only a new head or a new request opens it again.

GitHub gives the verdict from the reviews of the MR: your last review that approves, requests changes or comments, on a commit. GitLab gives the verdict from the approvals: your approval of the current head. `loomd` keeps the last verdict in `sync.json`. Thus, when GitLab resets the approvals after a push, the task still knows that a verdict was given.

## What the sync changes

The sync owns few fields of a Review task:

- At creation: the title (`Review <key>: <MR title>`), the kind `review`, the status `inbox`, and the `origin` link (provider, key, URL).
- Then: the status, only on the changes of the table above, each one with a note.

The sync never changes the importance, the due date, the pin, the notes or the title after creation. These fields are yours.

The key of the MR is an alias of the task: `loom show acme/shop#412` shows the Review task of that MR. The command encodes the key in the URL path (`/v1/tasks/acme%2Fshop%23412`).

## What a poll writes

The MR mirror is in the task directory, in `mrs/<provider>-<key>/`. `loomd` owns these files.

A poll writes nothing when nothing changed:

- `loomd` reads each MR with the `ETag` of its last read. A `304 Not Modified` costs no request of the rate limit and gives no new data.
- When an MR changed, `loomd` reads its reviews (GitHub) or its versions and approvals (GitLab), and its threads.
- The threads are also read again one hour after their last read, because a resolved thread does not always change the MR.
- `loomd` writes a file only when its bytes change, with an atomic rename.
- Each task that changed gets one commit of its store per poll. The message is the message of its events, otherwise `mr(<id>): <key> mirror updated`.

`loomd` keeps the `ETag`s in memory. After a restart, the first poll reads each MR in full. The files do not change if the MR did not change.

## When a forge fails

- A host that does not answer, or that refuses the token, stops its own poll only. The other hosts go on.
- The mirrors keep their last snapshot. The answer gives the error and the time of the last poll that succeeded.
- The next poll waits 2, then 4, then 8 times the interval, never more.
- A rate limit (`429`, or `403` with no remaining requests) stops the poll until the reset.
- When the token is refused (`401`), `loomd` gets the token again at the next poll: it can be a new one.
- A failure of one MR (for example, a `404`) stops only that MR. Its mirror keeps its snapshot, and the next poll reads it in full.

## Limits

- GitLab: only the approvals give a verdict. A comment without an approval is not a verdict, and a reset of the state of a reviewer is not a new request.
- GitHub: a request to one of your teams counts as a request to you, unless `review_requests = "direct"`.
- An archived Review task is not changed.
