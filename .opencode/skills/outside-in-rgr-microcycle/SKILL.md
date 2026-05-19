---
name: outside-in-rgr-microcycle
description: Fine-grained outside-in RED-GREEN-REFACTOR microcycles with specialist agents and single-diagnostic implementation.
---

# Outside-In RGR Microcycle

Use this skill when `/outside-in-rgr` is orchestrating one behavior through specialist-agent handoffs.

## Ledger

Keep a visible ledger for each active cycle:

```text
Goal:
Current test:
Focused command:
Observed failure:
Expected failure reason:
Reviewer decision:
Diagnostic under treatment:
Allowed immediate change:
Result:
Next control owner:
```

## RED

RED is valid when a focused command was run and produced an observed failure that is expected for the requested behavior. Compiler errors count as RED when the test intentionally pressures a missing API, missing type, or crate boundary.

RED must expose exactly one current failing test or one current diagnostic. If a command reports multiple failing tests, narrow the command or split the behavior before implementation.

Fix test misuse before production edits. Do not treat accidental misuse of existing code as implementation pressure.

## Test Review

Send every new or activated RED test to `rgr-test-reviewer` before production edits, then record approval with `rgr_approve_red`. A reviewer veto blocks implementation until the test author addresses the mandatory notes and records a new RED.

## Single Diagnostic

The implementer may treat exactly one current diagnostic at a time. The allowed production edit is the smallest concrete change that removes or changes that diagnostic. Do not predict later errors, prebuild adjacent behavior, refactor opportunistically, or batch fixes.

Each implementer handoff must name the current diagnostic and the allowed immediate change. After one behavioral production edit, stop and rerun the focused command; do not make a second behavioral edit until the orchestrator records the changed RED or GREEN.

## GREEN Diagnostic Loop

An approved RED test can require multiple GREEN implementation turns. The implementer is not required to make the whole test pass in one edit. It must make exactly one smallest edit that removes or changes the current diagnostic, then stop.

When the same focused command still fails with a new expected diagnostic from the same approved test, the orchestrator records that changed output with `rgr_record_red` using the same focused command and gets `rgr_approve_red` again. That refreshes the single-edit allowance for the next GREEN turn without asking `rgr-test-author` to write a new test.

Do not start a new outer RED cycle just because the approved test still fails differently. Start a new RED only when the next required behavior is not covered by the approved test, the failure is unrelated, or implementation review identifies a missing behavior outside the passing test.

If the same diagnostic remains after an attempted edit, stop for diagnosis instead of taking another edit token. If the next diagnostic requires an API boundary change across multiple files, ask the orchestrator to approve that explicit path set; do not use shell scripts or other tools to bypass the edit gate.

Delegated implementation leases are persisted in the project-local `.opencode/state/` store so implementer subagents can claim parent-approved work across plugin instances. Treat that store as guardrail-owned state: do not edit it directly, do not commit it, and use `rgr_claim_implementation_lease` to claim eligible delegated work.

## Ambiguous Failure Escape Hatch

If the current diagnostic does not identify one concrete code change, write or request a lower-level unit test that exposes the next decision point. That lower-level test must go through RED and test review before production edits.

## GREEN

GREEN means the focused command for the current test passes after the smallest demanded implementation change. When the observed failure changes but the test still fails, stop the implementer turn and return control to the orchestrator with the new diagnostic.

## Implementation Review

After GREEN, send the production diff to `rgr-implementation-reviewer`. A reviewer veto blocks refactor, broader verification, and handoff until the implementer addresses mandatory notes about minimality, type correctness, error handling, security boundaries, crate patterns, or style. If the reviewer finds a missing behavior not covered by the GREEN test, it returns to the orchestrator as a new RED instead of becoming an untested implementation request.

## REFACTOR

Refactor only after GREEN and implementation review approval. Refactors must preserve behavior and keep the focused command green. Avoid abstractions not demanded by the current behavior.

## Control Transfer

Return control to the orchestrator whenever a test is authored, a reviewer approves or vetoes, a diagnostic changes, a focused test passes, the same failure remains after an attempted edit, or a blocked state needs a decision.

## Stop Conditions

Stop the active microcycle when the current test passes, reviewer vetoes are resolved, focused verification passes, and the ledger identifies the next handoff. Commit each approved GREEN/refactor checkpoint before starting the next RED unless the user explicitly says not to commit.

## Blocked States

Report a blocked state when no focused command can be run, the failure output is unavailable, the diagnostic is ambiguous and no lower-level test seam is apparent, or required changes would touch unrelated user work.

## Verification

Run the narrow focused test first. Before handoff, run the strongest relevant gate feasible for the files changed and state any skipped gate with the reason.
