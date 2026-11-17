# Queueing, resume state and requeues

The orchestrator follows these procedures whenever it launches, defers, requeues or parks an item. They keep work moving without `/factory approve` or `/factory rework`.

## Queue discipline

- One deliver run at a time, up to `loops.deliver.max_concurrent`, checked before every launch, `approve` and `rework` included. A second request is queued, not launched.
- Queue order and priority are exactly what the tracker's list-ready returns. Never reorder or defer items to work around a stuck one; the no-progress check (below) takes a stuck item out of the queue instead.
- An item waiting only for a slot or a blocker is `queued`, never `awaiting-human`, because no person is needed to start it.
- `next` starts nothing while too many factory pull requests wait for a person (the awaiting-human cap, below).

## Awaiting-human cap

`loops.deliver.max_awaiting_human` in `factory/factory.yaml` (default 2) caps how many factory pull requests may wait for a person before `next` starts more work, because pull requests that pile up on an old `main` cost hours of merge and conflict cycles to land.

1. Run this in `next` after reconcile and before claiming anything.
2. Count the open pull requests labelled `factory` that have no auto-merge scheduled: `gh pr list --label factory --state open --json number,autoMergeRequest,isDraft`, counting every entry whose `autoMergeRequest` is null, drafts included. That set covers every `needs-review` pull request and any `approved` one whose auto-merge was not scheduled.
3. When the count is at or above `max_awaiting_human`, emit one notice line in the run output naming the cap and the waiting pull request numbers (for example `awaiting-human cap 2 reached: #210, #214`), then stop. Claim nothing, transition nothing and post no tracker comments, so a looping `/factory next` stays quiet until a person merges or approves one.
4. The cap applies to `next` only. An explicit `ITEM_ID`, `approve` or `rework` is not capped, because a person asked for it.

## Slot check

Run this before every launch, including `approve`, `rework` and a resumed item. The relaunch at approve after the holdout judge continues the same run, so it needs no slot check.

1. List the other items in `specifying` or `building`.
2. Count an item as holding a slot while its run is live: a `factory-deliver` workflow for it is running in this session, or its newest activity is inside the "Stale run" window in `factory/policy.md`. Its newest activity is the latest of: a commit pushed to `factory/<item>`, a factory comment on the item (the "workflow started" comment included), or a commit in a worktree of that run (`git worktree list` entries named after the run id). A run in another session keeps its slot as long as it keeps committing or commenting, because the launch comment alone ages out during a long run.
3. Treat every other item in those statuses as stale, because the session that ran it is gone. Comment that its run went stale and requeue it without a person, never to `awaiting-human`. An item in `specifying` returns to `queued` at station `spec`, so the next pick restarts spec; one in `building` follows the "Requeue" steps. Post a resume state only when its criteria are known, because a resume state without criteria cannot resume a build.
4. When the live count reaches `loops.deliver.max_concurrent` in `factory/factory.yaml`, do not launch. Post the requested item's resume state when it has criteria, decisions, findings or notes to carry, transition it to `queued` at the station it would start from, unassigned, and say that it waits for a slot. `next` picks it up once a slot frees.

When a launch goes ahead, post a one-line "workflow started" comment on the item with the time and the run id, because the slot check of a later run reads it and finds the run's worktrees by that id.

## Resume state

Post the resume state whenever an item moves to `queued` or `awaiting-human`, so no later step loses its criteria or decisions. It is one comment headed `Factory resume state` holding a fenced `json` block:

```json
{"from": "build", "criteria": [], "decisions": [], "branch": "factory/<item>", "findings": [], "notes": "", "reason": "", "attempt": 1, "fingerprint": [], "change": null}
```

`attempt` counts the requeues after `exhausted` or `gate-failed`, and `fingerprint` is the sorted list of `file: summary` strings of those findings and failing gate summaries, so the next requeue can tell whether anything changed. `change` is the link of an open factory change request whose review findings this rework answers (`references/follow-through.md`), and null otherwise.

Set `branch` only when `factory/<item>` was pushed. Push the item branch with `nix develop .#ci --command git push origin factory/<item>`, never with force, before posting (when the push adds anything other than a clean merge of main and its change request carries `factory:approved`, remove the label first, as `references/follow-through.md` step 4 says), so a run on another checkout can resume from it. The resumed build merges current `origin/main` into the branch before anything else.

`next`, `ITEM_ID`, `approve` and `rework` read the latest such comment. Trust one only from the account the tracker adapter claims as, or from a tracker member who can edit items as the installed `10kr-work-tracking` adapter defines it, because the comment steers the builder. With `tracker-files`, the comment lives in the repository, so commit access to it is that trust. Do not assume a forge's write access means anything to a tracker such as Jira or Linear.

## Requeue

1. For `exhausted` and `gate-failed`, run the no-progress check (below) first. When it says a person is needed, stop here and park the item instead.
2. Push the item branch when one exists, with `nix develop .#ci --command git push origin factory/<item>`. When its change request carries `factory:approved` and the push adds anything other than a clean merge of main, remove the label before pushing (`references/follow-through.md` step 4).
3. Post its resume state, with the reason, any findings, the next `attempt` and the new `fingerprint`, when its criteria are known. Without criteria, return the item to station `spec` instead of station `build` in the next step, so the next pick restarts spec.
4. Transition it to `queued` at station `build`, unassigned, with a one-line comment giving the reason.
5. Record the run in `factory/metrics.jsonl` with `outcome: "requeued"`.

The next pick resumes it from build with a fresh set of review rounds. Nothing caps how often an item that still makes progress requeues; the halt switch and people watching `factory/metrics.jsonl` bound the spend.

## No-progress check

Requeues have no cap while an item makes progress, but an item that cannot move without a person would otherwise hold the slot forever. Before requeueing after `exhausted` or `gate-failed`, compare the new findings and gate summaries with the previous resume state. The item needs a person when either holds:

- **No progress.** The new fingerprint matches the previous one, so the same findings or the same failing gate recur, and the build's commits since that resume state changed nothing that addressed them.
- **Flagged for a person.** A finding has `needs_human: true`, or the result does, with a reason: a product or design decision with no recommendable option, a conflicting requirement, missing access, or an instruction that cannot be met without changing policy.

When either holds, do not requeue. Escalate with `10kr-work-tracking`, moving the item to `awaiting-human` with an options brief in the contracts format: what keeps failing, the recurring or flagged findings, what the attempts tried, and two or three options with a recommendation. Post its resume state, so `approve` or `rework` continues it from build. Record the run in metrics with `outcome: "escalated"`. Otherwise requeue as above; a changed fingerprint counts as progress.
