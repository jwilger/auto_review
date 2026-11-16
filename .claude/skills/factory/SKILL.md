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

## Workflow results

Every result has `stage`, `outcome` and `history`. Apply the outcome:

`awaiting-human` is only for what a person alone can unblock: credentials, permissions, a policy or authority change, a halt for cause, or an action the factory cannot take. Everything else keeps moving without a factory command.

Post the item's resume state (`references/queueing.md`) whenever it moves to `awaiting-human` or `queued`.

- `escalated` (spec or slice only): the station could not recommend any option, so escalate with `10kr-work-tracking`, attaching `brief` for the roles in `owner`, and transition to `awaiting-human`. When `owner` includes `design-lead`, render the options before escalating: when `gates.visual` is set, run `factory-implementer` once per option on its own branch (`factory/<item>/option-<n>`) and open each as a draft change request with the installed `10kr-change-requests` skill, so the forge posts its before-and-after images; link them in the brief's **Rendered:** line. With no change-request adapter, or `forge: none`, render each option with the visual gate into `/home/jwilger/src/auto_review/factory/evidence/<item>/option-<n>/` instead and attach its images to the escalation through `10kr-work-tracking`, so the decider still sees them. When there is no visual gate, say in the brief that the options could not be rendered. Status shows the item as awaiting design.
- `blocked`: route on `blocker`, and comment the stage, the reason and the last gate summary in every case.
  - `item` (the script turns one without `blocked_by` into `environment`): record that the item is blocked by item `blocked_by` with the tracker's blocked-by link (the one list-ready reads), then requeue. List-ready skips it until the blocker closes.
  - `environment`: file the problem the builder described as a new item with `10kr-work-tracking` (create), not marked ready, so a human triages it; link it as this item's blocker, then requeue.
  - `human`, which a BLOCKED verdict naming no kind also becomes: transition to `awaiting-human`, because only a person can clear it.
- `gate-failed` (the builder said DONE but a gate stayed red or a UI change rendered no evidence) or `exhausted` (review rounds ran out): unless the no-progress check in `references/queueing.md` sends it to a person, requeue with the `findings` in the comment and the resume state, so the next pick reworks them.
- `agent-failed`: comment with the stage and the reason, and transition to `awaiting-human`, because a station that keeps failing would otherwise loop. Never retry silently.
- `reviewed` (holdout enabled): run the holdout judge on `branch` in a separate judge run whose scenarios the builder run cannot read, then relaunch with `resume: {from: 'approve', criteria, decisions, branch, round, paths, evidence, history}` and `holdout.score`, taking `decisions`, `paths`, `evidence` and `history` from the result, so open decisions still keep the item from auto-merge and so the item's metrics keep its build and review rounds.
- `needs-review`: the approver's ESCALATE, meaning a human must review and merge the change. Open the change request with the installed `10kr-change-requests` skill, labelled `factory`, for review and never as a draft, in every mode. Put the item's intent, the approver's `brief`, a `Criteria` section listing every criterion, and a `Decisions` section with every entry in `decisions` (question, chosen option, alternatives and why) in its body, so the reviewer can confirm or redirect each choice and a later `rework` can read them back, and link it with `link-change`. Request review from the roles in `owner`, resolved to people or teams through the policy's `## Roles`, and from the approver's `recommended` reviewers, leaving out the change request's author. Never schedule its merge. Leave the item in `reviewing`; reconciling closes it when a person merges. With no change-request adapter, handle it as `approved` does.
- `approved`: open the change request with the installed `10kr-change-requests` skill, labelled `factory` so people can tell factory work from hand-made changes, with the item's intent and criteria, the approver's `reviewers` in its review section, and link it with `link-change`. When no change-request adapter is installed, or the forge is `none`, say so, transition the item to `awaiting-human` with a comment naming its branch, and stop. Required roles are requested by the forge's own rules; request nobody extra for them.
  - In `shadow`, open it as a draft, list the reviewers in its body only, and stop.
  - In `lit` and `dark`, the change request is open for review, and its author is whoever ran the factory: ask the `recommended` roles for review, leaving out that person, because an author never reviews their own change. An asked review holds the merge until the asked person approves the latest commit, by the contract's merge-conditions.
  - In `dark`, schedule the merge (`gh pr merge <n> --auto --squash --match-head-commit <approved sha>`) only when the approver's `classes` is not empty and every class in it is in the policy's "Auto-merge" list, the forge enforces every `required` routing rule for those classes, the account this run acts as cannot bypass those rules (checked through the installed `10kr-change-requests` skill; when it cannot be read, do not schedule the merge), nobody asked is still waiting, and nothing adds to an accessibility baseline or token allowlist. Otherwise comment who the item waits on and leave it in `reviewing`; reconciling closes it later.

Follow every change request opened above until it is merged or closed, by `references/follow-through.md`. Review fixes go back through the factory as a rework run on the same branch, never as hand edits; when the resume state names a `change`, push to that change request instead of opening a new one.

## Commands

- `next`: reconcile first (below), then list ready items with `10kr-work-tracking` (list-ready), take the first item in the order list-ready returns, and run it as `ITEM_ID` does. When every slot is taken (above), report that and stop without claiming anything.
- `ITEM_ID`: claim the item, naming this run and the account the tracker adapter claims as in the claim comment (for GitHub Issues, `gh api --hostname "$(gh repo view --json url --jq .url | cut -d/ -f3)" user --jq .login`, which asks the tracker repository's own host rather than gh's default, so a GitHub Enterprise tracker names the right account; for other trackers, the connector login). When the item is at station `build` and has a trusted `Factory resume state` comment, resume it: transition it to `building` and launch with `resume: {from: 'build', criteria, decisions, branch, findings, notes}` from the latest one. A pushed branch alone does not count, because it carries no criteria. Otherwise transition it to `specifying` and launch from the start. Apply the result.
- `status`: reconcile first (below), then summarise items per status, naming who each waits on (an engineer, the design lead, the product owner), and the last ten lines of `factory/metrics.jsonl`.

- `approve ITEM_ID`: record a person's answer on an awaiting-human item. For a spec escalation, relaunch with `resume: {from: 'build', criteria, decisions}`, taking the criteria from the answer and the decisions as below. For a shadow draft, ask whether it would have merged unchanged and record that as `human_edits`.
- `rework ITEM_ID`: relaunch with `resume: {from: 'build', criteria, decisions, branch, findings, notes}`, taking `notes` from the human. `rework` and `approve` take `criteria` and `decisions` from the latest resume-state comment or, for an item with an open factory change request and no resume state (as after `needs-review`), from that change request's `Criteria` and `Decisions` sections.
- `upkeep`: launch `.claude/workflows/factory-upkeep.js` with `{scans: loops.upkeep.scans, existing}` and file each returned item with `10kr-work-tracking` (create), not marked ready, so a human triages it.
- `halt`: set the halt switch and say how to clear it.

## Reconcile

People review, merge and close change requests after a run ends, so before `next` and `status`, read every item in `reviewing` that has a linked change request, with the installed `10kr-change-requests` skill's status:

- **Merged**: `close` the item.
- **Closed without merging**: transition the item to `awaiting-human` with a comment saying the change request was closed unmerged, so a person decides between `rework` and dropping it.
- **Reviewed by a person**: never schedule the merge of an item whose run ended `needs-review` (the item's latest after-run line in `factory/metrics.jsonl`), because a person merges those. Otherwise, in `dark`, once every asked reviewer has approved the latest commit, recheck the item's merge eligibility above and the contract's merge-conditions, and schedule the merge when both pass; auto-merge stays off while any asked review is pending, because the forge does not hold a merge for an ordinary review request. When a push has landed since an asked reviewer approved, cancel any scheduled merge and request that reviewer again, per the contract's Merge rules. Otherwise, and in `lit`, leave it in `reviewing`.

Record each forge event, and any revert, as a follow-up metrics line, as `references/follow-through.md` describes under "Reconcile records".

## After every item

Append one JSON line to `factory/metrics.jsonl` and never edit earlier lines. Fields: `item`, `actor` (the tracker account this run claimed as, plus the forge login when it differs), `loop`, `mode`, `classes` (from the approver), `stations` (ids run), `rounds`, `gate_failures` (`[{station, gate}]` from `history`, the visual gate included), `verdicts`, `lenses` (`[{round, lens, verdict, findings}]` from the review entries in `history`), `outcome` (merged, needs-review, awaiting-review, requeued, escalated, abandoned or drafted), `human_edits` (true, false or null) and `at` (ISO time). People's reviews and later merges arrive as follow-up lines from reconciling.

## Additional resources

- `references/queueing.md`: queue discipline, the slot check, the resume state and who may post it, requeues and the no-progress check. Read it before any launch and whenever an item is deferred, requeued or parked.
- `references/follow-through.md`: watching a factory change request to merge, review fixes through the factory, and reconcile metrics lines. Read it after opening a change request and while reconciling.
