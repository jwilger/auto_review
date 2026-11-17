# Following a factory change request to merge

The orchestrator follows every change request it opens, `approved` and `needs-review` alike, until it is merged or closed. It does this itself rather than through an ad-hoc helper, because it is the only writer of item status and the only one that can send review fixes back through the factory.

## Contents

- Watching
- Approval to merge
- Review findings and failed checks
- Reconcile records

## Watching

1. Read the change request's state with the installed `10kr-change-requests` skill's status read: required checks, people's reviews, and unresolved threads. Where `forge-github:10kr-babysit-pr` is installed, its loop may do the watching, but only the watching: never let it edit, fix or merge.
2. While a session runs the loop, wait with the harness's waiting tools, such as a background check watch or Monitor, rather than polling in a busy loop.
3. This project has no review bot and requires no approvals (`factory/policy.md`), so the factory's own review and approve stations are the review: there is no bot to re-trigger.
4. In `dark`, schedule auto-merge only for an `approved` item, after refreshing its base (merge current `origin/main` with a merge commit, `nix develop .#ci --command just ci`, `nix develop .#ci --command git push origin <branch>`), and bound to the new head with `gh pr merge <n> --auto --squash --match-head-commit <new head>`, so a head that moved afterwards fails to schedule. The only commits between the approved head and the new head are clean merges of main. A `needs-review` change request is never scheduled on the factory's own say-so; it waits for the maintainer's approval (below). The merge guard hook (`.claude/hooks/factory-merge-guard.sh`) refuses to merge or schedule any pull request that touches a `factory` path, except on the maintainer's recorded instruction for that pull request and head: the merge carries `--match-head-commit <sha>` for the current head, and a person under `## Roles` in `factory/policy.md` approved a commit on the branch with only merges of `main` after it, or applied the `factory:approved` label after the branch's last own commit. The hook verifies this itself through `gh api` and denies when it cannot read any of it; it is not a blanket bypass. Never add the `factory:approved` label yourself (the hook blocks it), never work around the hook, and never use its `FACTORY_MERGE_APPROVED` override, which is for a person's explicit instruction in the session only.

## Approval to merge

A `needs-review` change request is approved when a person under `## Roles` in `factory/policy.md` left an `APPROVED` review on a commit with only merges of main after it, or applied the `factory:approved` label after the branch's newest own commit. Reconcile then merges it by the approval merge sequence in `SKILL.md`: merge current `origin/main` into the branch (no force push), run `nix develop .#ci --command just ci`, push with `nix develop .#ci --command git push origin <branch>`, wait for `gh pr checks <n> --required --watch`, and run `gh pr merge <n> --squash --match-head-commit <that head>`. A conflict, a red gate or a failed required check aborts the merge, removes `factory:approved`, comments on the change request and requeues the item at build with the failure as a finding and the change request as `change`. Before any merge or schedule, `git diff --name-only origin/main...<branch>` must show no `.claude/agent-memory/` path; one that does is requeued with that finding instead.

## Review findings and failed checks

Review fixes always go through the factory, never as hand edits, because a hand edit skips the build gates, the review lenses and the approver. When a factory change request gets a request for changes, unresolved threads from the bot or a person, or a required check that fails because of the change:

1. Verify each finding with `engineering-core:review-verifier`, through `engineering-core:10kr-receiving-code-review`.
2. Answer each finding that does not hold up in its own thread, with the evidence.
3. For the findings that hold up, post the item's resume state (`references/queueing.md`) with them as `findings` and the change request's link as `change`, and requeue the item at station `build`, `queued`, unassigned. Leave the change request open.
4. When its slot comes up, fetch `factory/<item>` and launch the rework run on that branch. The builder merges current `origin/main` into it first, then adds commits on top; never rewrite history, because the change request is open and reviewers' approvals point at its commits. The run goes through review and approve again. On `approved` or `needs-review`, push the branch with `nix develop .#ci --command git push origin factory/<item>`, never with force, instead of opening a new change request. A rework push (any push other than a clean merge of main) voids an earlier approval, so remove the `factory:approved` label (`gh pr edit <n> --remove-label factory:approved`) when the change request carries it, then reply to and resolve each addressed thread, request review again, and keep following the change request.
5. A check that failed for a transient or infrastructure reason is rerun or reported as `engineering-core:10kr-ci-failure-follow-up` directs, not requeued, because the change did not cause it.
6. The no-progress check in `references/queueing.md` applies: findings that recur with nothing addressed, or a `needs_human` flag, send the item to `awaiting-human` with an options brief.
7. The approver's classification of the new head decides whether auto-merge is allowed, exactly as on the first run.

## Reconcile records

Append one follow-up line to `factory/metrics.jsonl` per forge event, never twice: `{item, event: "reviewed"|"merged"|"closed"|"reverted", ref, reviews: [{role, verdict}], outcome, at}`, where `ref` identifies the event (the review's id, or the merge or close time). Before appending, skip any `ref` already recorded for that item. Reverts come after an item is closed, so check them separately: for every item with a `merged` line and no `reverted` line in `factory/metrics.jsonl`, look on the default branch for a later commit that reverts its merge commit (git's own revert, which names the reverted commit), and record `reverted` when one exists.
