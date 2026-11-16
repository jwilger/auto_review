# Following a factory change request to merge

The orchestrator follows every change request it opens, `approved` and `needs-review` alike, until it is merged or closed. It does this itself rather than through an ad-hoc helper, because it is the only writer of item status and the only one that can send review fixes back through the factory.

## Contents

- Watching
- Review findings and failed checks
- Reconcile records

## Watching

1. Read the change request's state with the installed `10kr-change-requests` skill's status read: required checks, people's reviews, and unresolved threads. Where `forge-github:10kr-babysit-pr` is installed, its loop may do the watching, but only the watching: never let it edit, fix or merge.
2. While a session runs the loop, wait with the harness's waiting tools, such as a background check watch or Monitor, rather than polling in a busy loop.
3. This project has no review bot and requires no approvals (`factory/policy.md`), so the factory's own review and approve stations are the review: there is no bot to re-trigger.
4. In `dark`, schedule auto-merge only for an `approved` item, and only on the head the approve station approved, bound to it with `gh pr merge <n> --auto --squash --match-head-commit <sha>`, so a head that moved after approval fails to schedule. A `needs-review` change request is never scheduled; a person merges it. The merge guard hook (`.claude/hooks/factory-merge-guard.sh`) refuses to merge or schedule any pull request that touches a `factory` path; never work around it, and never use its `FACTORY_MERGE_APPROVED` override, which is for a person's explicit instruction in the session only.

## Review findings and failed checks

Review fixes always go through the factory, never as hand edits, because a hand edit skips the build gates, the review lenses and the approver. When a factory change request gets a request for changes, unresolved threads from the bot or a person, or a required check that fails because of the change:

1. Verify each finding with `engineering-core:review-verifier`, through `engineering-core:10kr-receiving-code-review`.
2. Answer each finding that does not hold up in its own thread, with the evidence.
3. For the findings that hold up, post the item's resume state (`references/queueing.md`) with them as `findings` and the change request's link as `change`, and requeue the item at station `build`, `queued`, unassigned. Leave the change request open.
4. When its slot comes up, fetch `factory/<item>` and launch the rework run on that branch. The builder adds commits on top; never rewrite history, because the change request is open and reviewers' approvals point at its commits. The run goes through review and approve again. On `approved` or `needs-review`, push the branch, never with force, instead of opening a new change request, reply to and resolve each addressed thread, request review again, and keep following the change request.
5. A check that failed for a transient or infrastructure reason is rerun or reported as `engineering-core:10kr-ci-failure-follow-up` directs, not requeued, because the change did not cause it.
6. The no-progress check in `references/queueing.md` applies: findings that recur with nothing addressed, or a `needs_human` flag, send the item to `awaiting-human` with an options brief.
7. The approver's classification of the new head decides whether auto-merge is allowed, exactly as on the first run.

## Reconcile records

Append one follow-up line to `factory/metrics.jsonl` per forge event, never twice: `{item, event: "reviewed"|"merged"|"closed"|"reverted", ref, reviews: [{role, verdict}], outcome, at}`, where `ref` identifies the event (the review's id, or the merge or close time). Before appending, skip any `ref` already recorded for that item. Reverts come after an item is closed, so check them separately: for every item with a `merged` line and no `reverted` line in `factory/metrics.jsonl`, look on the default branch for a later commit that reverts its merge commit (git's own revert, which names the reverted commit), and record `reverted` when one exists.
