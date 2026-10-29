#!/usr/bin/env bash
# loom #183: check the GitHub GraphQL publish flow of ADR-0016 on a real repository.
#
# Needs: gh (logged in as the reviewer, scopes repo + workflow), git, jq, curl.
# Creates a PUBLIC throwaway repository <you>/<REPO> (default loom-graphql-spike).
# The pull request is opened by github-actions[bot], so the reviewer is not its author.
# Every response goes to ./out/NN-<step>.json (raw, with real logins: sanitize before commit).
# The repository stays; delete it after with: gh repo delete <you>/<REPO> --yes
set -euo pipefail

REPO_NAME="${REPO:-loom-graphql-spike}"
ME="$(gh api user --jq .login)"
R="$ME/$REPO_NAME"
OUT="$(pwd)/out"
mkdir -p "$OUT"
n=0
log() { printf '\n== %s\n' "$*"; }
save() { n=$((n + 1)); local f; f="$(printf '%s/%02d-%s.json' "$OUT" "$n" "$1")"; cat >"$f"; echo "-> $f"; cat "$f"; echo; }

# gql <step> <query> <variables-json>: the raw response, errors included.
gql() {
  jq -n --arg q "$2" --argjson v "$3" '{query: $q, variables: $v}' >"$OUT/req.json"
  gh api graphql --input "$OUT/req.json" 2>/dev/null | save "$1" || true
}
pull_rest() { gh api "repos/$R/pulls/$PR" --jq '{updated_at, head: .head.sha}'; }
# etag_check <step> <etag>: the status of GET pulls/<n> with If-None-Match.
etag_check() {
  gh api -i -H "If-None-Match: $2" "repos/$R/pulls/$PR" 2>/dev/null | head -1 \
    | jq -R --arg etag "$2" '{status_line: ., if_none_match: $etag}' | save "$1" || true
}
etag_of() { gh api -i "repos/$R/pulls/$PR" | tr -d '\r' | awk -F': ' 'tolower($1)=="etag"{print $2}'; }
anon() {
  jq -n --argjson reviews "$(curl -fsS "https://api.github.com/repos/$R/pulls/$PR/reviews")" \
        --argjson comments "$(curl -fsS "https://api.github.com/repos/$R/pulls/$PR/comments")" \
        '{reviews: [$reviews[] | {id, state, user: .user.login}], comments: [$comments[] | {id, body, user: .user.login, pull_request_review_id}]}' | save "$1"
}

Q_THREADS='query($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { id headRefOid updatedAt reviewThreads(first: 100) { nodes { id isResolved isOutdated path diffSide line startLine subjectType viewerCanReply viewerCanResolve comments(first: 50) { nodes { id author { login } body state replyTo { id } pullRequestReview { id state } } } } } } } }'
Q_PENDING='query($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { reviews(states: PENDING, first: 100) { nodes { id state author { login } createdAt commit { oid } comments(first: 100) { totalCount nodes { id body path line startLine replyTo { id } } } } } } } }'
M_ADD='mutation($input: AddPullRequestReviewInput!) { addPullRequestReview(input: $input) { pullRequestReview { id state } } }'
M_REPLY='mutation($input: AddPullRequestReviewThreadReplyInput!) { addPullRequestReviewThreadReply(input: $input) { comment { id state pullRequestReview { id state } } } }'
M_SUBMIT='mutation($input: SubmitPullRequestReviewInput!) { submitPullRequestReview(input: $input) { pullRequestReview { id state submittedAt url } } }'
M_RESOLVE='mutation($input: ResolveReviewThreadInput!) { resolveReviewThread(input: $input) { thread { id isResolved } } }'

log "1. Repository $R and a pull request opened by github-actions[bot]"
if ! gh repo view "$R" >/dev/null 2>&1; then
  gh repo create "$R" --public --add-readme --description "Throwaway: loom GraphQL publish spike (#183)"
fi
gh api -X PUT "repos/$R/actions/permissions/workflow" -f default_workflow_permissions=write -F can_approve_pull_request_reviews=true
WORK="$(mktemp -d)"
gh repo clone "$R" "$WORK/repo" -- --quiet
cd "$WORK/repo"
mkdir -p src .github/workflows
for i in $(seq 1 60); do printf 'pub const L%02d: u32 = %d;\n' "$i" "$i"; done >src/lib.rs
git add src/lib.rs && git commit -qm "base" && git push -q origin HEAD:main
git checkout -qb feature
for i in $(seq 20 24); do sed -i.bak "s/^pub const L$i: u32 = $i;/pub const L$i: u32 = ${i}0;/" src/lib.rs; done
rm -f src/lib.rs.bak
cat >.github/workflows/open-pr.yml <<'YML'
on:
  push:
    branches: [feature]
permissions:
  contents: read
  pull-requests: write
jobs:
  open:
    runs-on: ubuntu-latest
    steps:
      - run: gh pr create -R "$GITHUB_REPOSITORY" --base main --head feature --title "Spike pull request" --body "Opened by github-actions for the loom GraphQL spike."
        env:
          GH_TOKEN: ${{ github.token }}
YML
git add -A && git commit -qm "change lines 20 to 24" && git push -q origin feature
cd - >/dev/null
PR=""
for _ in $(seq 1 30); do
  PR="$(gh pr list -R "$R" --head feature --json number --jq '.[0].number // empty')"
  [ -n "$PR" ] && break
  sleep 10
done
[ -n "$PR" ] || { echo "no pull request after 5 minutes: check the Actions run of $R"; exit 1; }
VARS="$(jq -n --arg o "$ME" --arg r "$REPO_NAME" --argjson n "$PR" '{owner: $o, name: $r, number: $n}')"
gh api "repos/$R/pulls/$PR" --jq '{number, author: .user.login, head: .head.sha}' | save pull
PR_ID="$(gh api graphql -f query='query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r){pullRequest(number:$n){id}}}' -f o="$ME" -f r="$REPO_NAME" -F n="$PR" --jq .data.repository.pullRequest.id)"
HEAD="$(gh api "repos/$R/pulls/$PR" --jq .head.sha)"

log "2. Seed: a submitted thread on line 21 to reply to later"
gql seed-review "$M_ADD" "$(jq -n --arg p "$PR_ID" --arg c "$HEAD" '{input: {pullRequestId: $p, commitOID: $c, event: "COMMENT", body: "", threads: [{path: "src/lib.rs", line: 21, side: "RIGHT", body: "question: why times ten?"}]}}')"
gql threads-before "$Q_THREADS" "$VARS"
THREAD="$(jq -r '.data.repository.pullRequest.reviewThreads.nodes[0].id' "$(ls "$OUT"/*-threads-before.json)")"
pull_rest | save updated-at-0
ETAG0="$(etag_of)"

log "3. No pending review yet"
gql pending-empty "$Q_PENDING" "$VARS"

log "4. addPullRequestReview without event: pending, with two new threads"
gql add-review "$M_ADD" "$(jq -n --arg p "$PR_ID" --arg c "$HEAD" '{input: {pullRequestId: $p, commitOID: $c, threads: [
  {path: "src/lib.rs", line: 22, side: "RIGHT", body: "suggestion (non-blocking): a name.\n\n```suggestion\npub const L22: u32 = 220; // scaled\n```"},
  {path: "src/lib.rs", startLine: 20, startSide: "RIGHT", line: 23, side: "RIGHT", body: "praise: clear values."}]}}')"
REVIEW="$(jq -r '.data.addPullRequestReview.pullRequestReview.id' "$(ls "$OUT"/*-add-review.json)")"

log "5. addPullRequestReviewThreadReply into the pending review"
gql add-reply "$M_REPLY" "$(jq -n --arg r "$REVIEW" --arg t "$THREAD" '{input: {pullRequestReviewId: $r, pullRequestReviewThreadId: $t, body: "Thanks, makes sense."}}')"
gql pending-after-reply "$Q_PENDING" "$VARS"
gql threads-pending "$Q_THREADS" "$VARS"

log "6. What another account (anonymous) sees before submit"
anon anon-before-submit
pull_rest | save updated-at-1-pending
etag_check etag-after-pending "$ETAG0"

log "7. A second pending review: error shape"
gql error-second-pending "$M_ADD" "$(jq -n --arg p "$PR_ID" --arg c "$HEAD" '{input: {pullRequestId: $p, commitOID: $c, threads: [{path: "src/lib.rs", line: 22, side: "RIGHT", body: "again"}]}}')"

log "8. submitPullRequestReview with COMMENT"
gql submit-comment "$M_SUBMIT" "$(jq -n --arg r "$REVIEW" '{input: {pullRequestReviewId: $r, event: "COMMENT", body: "comment verdict"}}')"
anon anon-after-submit
pull_rest | save updated-at-2-submitted
ETAG1="$(etag_of)"

log "9. resolveReviewThread, then: does updated_at or the ETag change?"
sleep 5
gql resolve-thread "$M_RESOLVE" "$(jq -n --arg t "$THREAD" '{input: {threadId: $t}}')"
sleep 5
pull_rest | save updated-at-3-resolved
etag_check etag-after-resolve "$ETAG1"

log "10. A thread on a line outside the diff (line 50): error shape"
gql error-line-not-in-diff "$M_ADD" "$(jq -n --arg p "$PR_ID" --arg c "$HEAD" '{input: {pullRequestId: $p, commitOID: $c, threads: [{path: "src/lib.rs", line: 50, side: "RIGHT", body: "outside"}]}}')"
gql pending-after-error "$Q_PENDING" "$VARS"

log "11. The other verdicts: APPROVE, then REQUEST_CHANGES"
for verdict in APPROVE REQUEST_CHANGES; do
  # A pending review left by a failed step would block the next one.
  left="$(gh api graphql --input <(jq -n --arg q "$Q_PENDING" --argjson v "$VARS" '{query: $q, variables: $v}') --jq '.data.repository.pullRequest.reviews.nodes[0].id // empty')"
  if [ -z "$left" ]; then
    gql "pending-$verdict" "$M_ADD" "$(jq -n --arg p "$PR_ID" --arg c "$HEAD" '{input: {pullRequestId: $p, commitOID: $c, threads: []}}')"
    left="$(jq -r '.data.addPullRequestReview.pullRequestReview.id' "$(ls "$OUT"/*-pending-$verdict.json)")"
  fi
  gql "submit-$verdict" "$M_SUBMIT" "$(jq -n --arg r "$left" --arg e "$verdict" '{input: {pullRequestReviewId: $r, event: $e, body: ("verdict " + $e)}}')"
done
pull_rest | save updated-at-4-end
rm -f "$OUT/req.json"
log "Done. Outputs in $OUT. Delete the repository after with: gh repo delete $R --yes"
