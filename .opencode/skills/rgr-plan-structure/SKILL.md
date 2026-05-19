---
name: rgr-plan-structure
description: Write behavior checklists up front, then drive only the active behavior through one RED-GREEN-REFACTOR cycle.
---

# RGR Plan Structure

Use this skill before writing plans, todo lists, PR checklists, or session outlines for behavior work.

## Good Upfront Planning

Define the behavioral and functional requirements up front: user-visible
outcomes, risks, constraints, and examples that would make the work complete.
Keep this as a checklist of desired behavior, not a queue of pre-authored RGR
cycles.

For the active work, choose only the next smallest observable behavior and name
the focused RED target for that behavior. Future cycles emerge after the current
test is GREEN/refactored and the remaining behavior checklist is reconsidered.

## Bad RGR Planning

Do not pre-plan multiple RED-GREEN-REFACTOR cycles with expected tests,
diagnostics, or implementation edits. That predicts information RGR is meant to
discover.

## Bad Plans

Waterfall plans list components in construction order: models, events, handlers,
persistence, UI, then tests. Faux-RGR waterfalls list Cycle 1, Cycle 2, Cycle 3
with future tests or edits before the first cycle is GREEN. If a task cannot be
tied to the current failing test or the remaining behavior checklist, it is
speculative.

## Active Cycle Template

Cycle 1:

1. RED: add or activate `<test name>` for `<observable behavior>` and run `<command>`.
2. GREEN: make the minimum production edit to pass that failure.
3. REFACTOR: improve only after `<command>` is green.

After GREEN/refactor, return to the behavior checklist and choose the next
smallest observable behavior. Do not fill in future cycle details in advance.
