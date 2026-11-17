---
name: factory
description: This skill should be used when the user asks to "run the factory", "factory next", "factory status", "approve factory item", "halt the factory", or runs /factory. It applies whenever work items in this repository move through the auto_review delivery factory.
disable-model-invocation: true
argument-hint: "[next|ITEM_ID|status|approve ITEM_ID|rework ITEM_ID|upkeep|halt]"
---

# auto_review factory orchestrator

This skill is the only writer of work-item status. Stations return verdicts; only this skill turns them into tracker transitions, because a single writer keeps the board truthful. Read `factory/factory.yaml` and `factory/policy.md` at the start of every run.

## Before anything

Check the halt switch (`FACTORY_HALT=true` in the environment, the file `factory/HALT`, or any open issue labelled `factory:halt` (`gh issue list --label factory:halt --state open`)). When it is set, report and stop.

## Launching the workflow

Launch `.claude/workflows/factory-deliver.js` with these args, each from one source:

- `item`: `{id, title, body, images}` from the tracker's `10kr-work-tracking` skill. Download the item's image attachments into `/home/jwilger/src/auto_review/factory/evidence/<item>/handoff/` and pass their paths as `images`, because stations cannot open links.
- `evidenceDir`: `/home/jwilger/src/auto_review/factory/evidence`, where builders write visual evidence that reviewers and the approver can read.
- `check`: `gates.check` in `factory.yaml`. `visual`: `gates.visual`, when present. `rounds`: "Review rounds" in `policy.md`.
- `slice`: true when `factory.yaml` has a slice station. `lenses`: the review station's `lenses`.
- `models` and `efforts`: each station's `model` and `effort`, keyed by station id.
- `holdout`: `{enabled, threshold}` from `factory.yaml`, plus `score` when resuming at approve.
- `resume`, only when continuing an item: `{from, criteria, decisions, round, branch, findings, notes, paths, evidence, history}`.
- `inlineAgents`, only when a station's agent type is not yet registered (agents written in this session load in the next one): `{agentType: agent file body}` read from `.claude/agents/`.

Transition the item to `building` when the workflow starts and to `reviewing` when its result shows a review round ran.

Before every launch, run the slot check in `references/queueing.md`; requeue items by its "Requeue" steps.

Every build starts from current `origin/main`: the workflow tells the builder to `git fetch origin` and create a new item or slice branch from `origin/main`, or, when the branch exists (a resume or rework), to merge current `origin/main` into it before any other change. Slice branches are `factory-slice/<item>/<slice-id>`, outside the item branch's ref path, so `factory/<item>` and its slices coexist. The workflow treats a build that reports any `.claude/agent-memory/` path as gate-failed.

Every factory push runs in the project toolchain, `nix develop .#ci --command git push origin <branch>`, so the lefthook pre-push gate has its dependencies; never skip that hook and never force-push. The gate (`just ci`) includes `just hermetic-git`, which fails when test code under `crates/` spawns `git` without clearing `GIT_DIR`, `GIT_WORK_TREE` and `GIT_INDEX_FILE`, and `just conflict-markers`, which fails on conflict marker lines in tracked files.

## The approve rule

The approve station escalates (`needs-review`) on a change in class `factory`, on any spec decision with `needs_human: true`, on review that used every allowed round, and on the policy's other Escalate rules; it never escalates just because decisions exist. The spec writer marks a decision `needs_human` only for a product, policy or security choice with no clearly recommendable option. Every other decision is listed in the change request's `## Decisions` section and does not block auto-merge. `factory-deliver.js`, the spec-writer and approver agents and `factory/policy.md` state the same rule.

## Workflow results

Every result has `stage`, `outcome` and `history`. Apply the outcome:

`awaiting-human` is only for what a person alone can unblock: credentials, permissions, a policy or authority change, a halt for cause, or an action the factory cannot take. Everything else keeps moving without a factory command.

Post the item's resume state (`references/queueing.md`) whenever it moves to `awaiting-human` or `queued`.

- `escalated` (spec or slice only): the station could not recommend any option, so escalate with `10kr-work-tracking`, attaching `brief` for the roles in `owner`, and transition to `awaiting-human`. When `owner` includes `design-lead`, render the options before escalating: when `gates.visual` is set, run `factory-implementer` once per option on its own branch (`factory-option/<item>/option-<n>`) and open each as a draft change request with the installed `10kr-change-requests` skill, so the forge posts its before-and-after images; link them in the brief's **Rendered:** line. With no change-request adapter, or `forge: none`, render each option with the visual gate into `/home/jwilger/src/auto_review/factory/evidence/<item>/option-<n>/` instead and attach its images to the escalation through `10kr-work-tracking`, so the decider still sees them. When there is no visual gate, say in the brief that the options could not be rendered. Status shows the item as awaiting design.
- `blocked`: route on `blocker`, and comment the stage, the reason and the last gate summary in every case.
  - `item` (the script turns one without `blocked_by` into `environment`): record that the item is blocked by item `blocked_by` with the tracker's blocked-by link (the one list-ready reads), then requeue. List-ready skips it until the blocker closes.
  - `environment`: file the problem the builder described as a new item with `10kr-work-tracking` (create), not marked ready, so a human triages it; link it as this item's blocker, then requeue.
  - `human`, which a BLOCKED verdict naming no kind also becomes: transition to `awaiting-human`, because only a person can clear it.
- `gate-failed` (the builder said DONE but a gate stayed red or a UI change rendered no evidence) or `exhausted` (review rounds ran out): unless the no-progress check in `references/queueing.md` sends it to a person, requeue with the `findings` in the comment and the resume state, so the next pick reworks them.
- `agent-failed`: comment with the stage and the reason, and transition to `awaiting-human`, because a station that keeps failing would otherwise loop. Never retry silently.
- `reviewed` (holdout enabled): run the holdout judge on `branch` in a separate judge run whose scenarios the builder run cannot read, then relaunch with `resume: {from: 'approve', criteria, decisions, branch, round, paths, evidence, history}` and `holdout.score`, taking `decisions`, `paths`, `evidence` and `history` from the result, so decisions marked `needs_human` still keep the item from auto-merge (other decisions never do) and so the item's metrics keep its build and review rounds.
- `needs-review`: the approver's ESCALATE under the approve rule above (a `factory` change, a `needs_human` decision, exhausted review rounds or another Escalate rule), meaning a human must review the change. Open the change request with the installed `10kr-change-requests` skill, labelled `factory`, for review and never as a draft, in every mode. Put the item's intent, the approver's `brief`, a `Criteria` section listing every criterion, and a `## Decisions` section (below) in its body, so the reviewer can confirm or redirect each choice and a later `rework` can read them back, and link it with `link-change`. Request review from the roles in `owner`, resolved to people or teams through the policy's `## Roles`, and from the approver's `recommended` reviewers, leaving out the change request's author. Never schedule its merge on the factory's own say-so. Leave the item in `reviewing`; once a person approves it, reconciling merges it (below), and it closes when merged. With no change-request adapter, handle it as `approved` does.
- `approved`: open the change request with the installed `10kr-change-requests` skill, labelled `factory` so people can tell factory work from hand-made changes, with the item's intent and criteria, a `## Decisions` section (below), the approver's `reviewers` in its review section, and link it with `link-change`. When no change-request adapter is installed, or the forge is `none`, say so, transition the item to `awaiting-human` with a comment naming its branch, and stop. Required roles are requested by the forge's own rules; request nobody extra for them.
  - In `shadow`, open it as a draft, list the reviewers in its body only, and stop.
  - In `lit` and `dark`, the change request is open for review, and its author is whoever ran the factory: ask the `recommended` roles for review, leaving out that person, because an author never reviews their own change. An asked review holds the merge until the asked person approves the latest commit, by the contract's merge-conditions.
  - In `dark`, refresh the base (next bullet) and then schedule the merge (`gh pr merge <n> --auto --squash --match-head-commit <new head>`), but only when the approver's `classes` is not empty and every class in it is in the policy's "Auto-merge" list, the forge enforces every `required` routing rule for those classes, the account this run acts as cannot bypass those rules (checked through the installed `10kr-change-requests` skill; when it cannot be read, do not schedule the merge), nobody asked is still waiting, and nothing adds to an accessibility baseline or token allowlist. Otherwise comment who the item waits on and leave it in `reviewing`; reconciling closes it later.
  - Refreshing the base, before any auto-merge is scheduled: `git fetch origin`; run `git diff --name-only origin/main...<branch>` and, when any `.claude/agent-memory/` path appears, refuse to schedule and requeue the item with that finding; merge current `origin/main` into the item branch with a merge commit (no rebase, no force push); run `nix develop .#ci --command just ci`; push with `nix develop .#ci --command git push origin <branch>`. The only commits allowed between the approved head and the new head are clean merges of main, which carry no hand-resolved conflict or extra edit. When `origin/main` is already an ancestor of the branch head, skip the merge, gate and push. A conflict or a red gate is handled as in step 8 of the approval merge sequence (Reconcile): undo the local merge (`git merge --abort` for a conflict, `git reset --hard origin/<branch>` for a red gate), comment, and requeue at build; never hand-resolve.

Every factory change request body, `approved` and `needs-review` alike, has a `## Decisions` section listing each entry in `decisions` with its question, chosen option, alternatives, why and `needs_human`, or `(none)` when there are none.

Follow every change request opened above until it is merged or closed, by `references/follow-through.md`. Review fixes go back through the factory as a rework run on the same branch, never as hand edits; when the resume state names a `change`, push to that change request (`nix develop .#ci --command git push origin <branch>`) instead of opening a new one.

## Commands

- `next`: reconcile first (below), then, before claiming anything, run the awaiting-human cap in `references/queueing.md`: count the open, `factory`-labelled pull requests with no auto-merge scheduled (`gh pr list --label factory --state open --json number,autoMergeRequest,isDraft`, drafts included), which covers every `needs-review` pull request and any `approved` one whose auto-merge was not scheduled. When the count is at or above `loops.deliver.max_awaiting_human` in `factory/factory.yaml`, emit one notice line in the run output naming the cap and the waiting pull request numbers, and stop: claim nothing, transition nothing and post no tracker comments. When that count cannot be read, fail closed the same way, with a notice that the cap could not be read. Otherwise list ready items with `10kr-work-tracking` (list-ready), take the first item in the order list-ready returns, and run it as `ITEM_ID` does. When every slot is taken (above), report that and stop without claiming anything. The cap applies to `next` only; `ITEM_ID`, `approve` and `rework` are not capped.
- `ITEM_ID`: claim the item, naming this run and the account the tracker adapter claims as in the claim comment (for GitHub Issues, `gh api --hostname "$(gh repo view --json url --jq .url | cut -d/ -f3)" user --jq .login`, which asks the tracker repository's own host rather than gh's default, so a GitHub Enterprise tracker names the right account; for other trackers, the connector login). When the item is at station `build` and has a trusted `Factory resume state` comment, resume it: transition it to `building` and launch with `resume: {from: 'build', criteria, decisions, branch, findings, notes}` from the latest one; the builder merges current `origin/main` into the branch before anything else. A pushed branch alone does not count, because it carries no criteria. Otherwise transition it to `specifying` and launch from the start. Apply the result.
- `status`: reconcile first (below), then summarise items per status, naming who each waits on (an engineer, the design lead, the product owner), and the last ten lines of `factory/metrics.jsonl`.

- `approve ITEM_ID`: record a person's answer on an awaiting-human item. For a spec escalation, relaunch with `resume: {from: 'build', criteria, decisions}`, taking the criteria from the answer and the decisions as below. For a shadow draft, ask whether it would have merged unchanged and record that as `human_edits`.
- `rework ITEM_ID`: relaunch with `resume: {from: 'build', criteria, decisions, branch, findings, notes}`, taking `notes` from the human. `rework` and `approve` take `criteria` and `decisions` from the latest resume-state comment or, for an item with an open factory change request and no resume state (as after `needs-review`), from that change request's `Criteria` and `Decisions` sections.
- `upkeep`: launch `.claude/workflows/factory-upkeep.js` with `{scans: loops.upkeep.scans, existing}` and file each returned item with `10kr-work-tracking` (create), not marked ready, so a human triages it.
- `halt`: set the halt switch and say how to clear it.

## Reconcile

People review, merge and close change requests after a run ends, so before `next` and `status`, read every item in `reviewing` that has a linked change request, with the installed `10kr-change-requests` skill's status:

- **Merged**: `close` the item.
- **Closed without merging**: transition the item to `awaiting-human` with a comment saying the change request was closed unmerged, so a person decides between `rework` and dropping it.
- **Approved by a person after `needs-review`** (the item's latest after-run line in `factory/metrics.jsonl` has outcome `needs-review`): the factory merges it by the approval merge sequence below once either signal holds, and otherwise never merges or schedules it.
  - (a) A non-author account named in `factory/policy.md` `## Roles` (as `main` has it, never the branch's own copy) left an `APPROVED` review on commit R, that review is the reviewer's latest decisive one (their newest `APPROVED` or `CHANGES_REQUESTED` review is that `APPROVED` one, so a later request for changes withdraws it), and every commit on the branch after R is a clean merge of main: `git rev-list --no-merges R..HEAD ^origin/main` is empty, and each merge's tree equals `git merge-tree --write-tree` of its parents (no hand-resolved conflict or extra edit).
  - (b) The pull request carries the label `factory:approved`, the latest `labeled` timeline event for it was made by a login in `## Roles` (as `main` has it), and that event came after the committer date of the newest non-merge commit on the branch that is not in `origin/main`. The factory itself never adds `factory:approved`.
- **Reviewed by a person**, for any other item: in `dark`, once every asked reviewer has approved, recheck the item's merge eligibility above and the contract's merge-conditions, refresh the base (the `approved` bullets above) and then schedule the merge on the new head when both pass; auto-merge stays off while any asked review is pending, because the forge does not hold a merge for an ordinary review request. An approval still counts when every commit after the approved one is a clean merge of main (`git rev-list --no-merges <approved>..HEAD ^origin/main` is empty, and each merge's tree equals `git merge-tree --write-tree` of its parents), so the refresh's own merge of main never re-requests review. When any other push has landed since an asked reviewer approved, cancel any scheduled merge and request that reviewer again, per the contract's Merge rules. Otherwise, and in `lit`, leave it in `reviewing`.

### Approval merge sequence

For a pull request approved by signal (a) or (b), in order:

1. `git fetch origin`, then `git diff --name-only origin/main...<branch>`; when any `.claude/agent-memory/` path appears, refuse to merge and requeue as in step 8 with that finding.
2. When `origin/main` is already an ancestor of the branch head, skip to step 5 with the current head, because there is nothing to refresh and no push to dismiss a review. Otherwise merge current `origin/main` into the item branch with a merge commit (no rebase, no force push).
3. Run `nix develop .#ci --command just ci`.
4. Push with `nix develop .#ci --command git push origin <branch>`.
5. Wait for the required checks on that head, bounded: first poll `gh pr checks <n> --required` (about every 30 seconds, for at most 10 minutes) until it reports checks for that head, then run `timeout 45m gh pr checks <n> --required --watch --fail-fast`. When no checks are reported in time or the watch times out, stop here without changing anything: the next reconcile picks the pull request up again. A required check that failed for a transient or infrastructure reason is rerun first, as `references/follow-through.md` step 5 directs; only a failure the change caused goes to step 8.
6. `gh pr merge <n> --squash --match-head-commit <that head sha>`, then `close` the item and record a `merged` metrics line. For a `factory`-class pull request the merge guard hook accepts the recorded approval as the maintainer's instruction for that head (`references/follow-through.md`); it is not a blanket bypass.
7. When the merge command in step 6 fails, change nothing and leave the item in `reviewing`, then act on the reason. The head moved (someone pushed): the next reconcile re-reads the approval signals for the new head. The merge guard denied it because the approval no longer covers the head (for example, GitHub dismissed the review when step 4 pushed, under the ruleset's `dismiss_stale_reviews_on_push`): comment on the pull request asking the maintainer to add `factory:approved`, which survives clean merges of main. A ruleset or branch protection blocked it: comment with the forge's message and transition the item to `awaiting-human`, because only a person can change that.
8. When merging main conflicts, the gate is red, or a required check fails because of the change, never hand-resolve or merge. Undo only what is still local: on a conflict, `git merge --abort`; on a red gate in step 3, the merge commit exists only locally, so `git reset --hard origin/<branch>` (safe, nothing was pushed) and do not push it; after a failed required check, the clean merge of main is already pushed and nothing needs undoing. Then remove `factory:approved` (`gh pr edit <n> --remove-label factory:approved`), comment on the pull request, and requeue the item at build with a resume state whose finding describes the conflict or failure and whose `change` is the pull request link. The rework then needs a fresh approval.

Record each forge event, and any revert, as a follow-up metrics line, as `references/follow-through.md` describes under "Reconcile records".

## After every item

Append one JSON line to `factory/metrics.jsonl` and never edit earlier lines. Fields: `item`, `actor` (the tracker account this run claimed as, plus the forge login when it differs), `loop`, `mode`, `classes` (from the approver), `stations` (ids run), `rounds`, `gate_failures` (`[{station, gate}]` from `history`, the visual gate included), `verdicts`, `lenses` (`[{round, lens, verdict, findings}]` from the review entries in `history`), `outcome` (merged, needs-review, awaiting-review, requeued, escalated, abandoned or drafted), `human_edits` (true, false or null) and `at` (ISO time). People's reviews and later merges arrive as follow-up lines from reconciling.

## Additional resources

- `references/queueing.md`: queue discipline, the slot check, the resume state and who may post it, requeues and the no-progress check. Read it before any launch and whenever an item is deferred, requeued or parked.
- `references/follow-through.md`: watching a factory change request to merge, review fixes through the factory, and reconcile metrics lines. Read it after opening a change request and while reconciling.
