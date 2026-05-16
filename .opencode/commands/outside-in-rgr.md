---
description: Run a fine-grained outside-in RGR workflow with specialist agents.
agent: build
---

Run the specialist outside-in RGR workflow for: $ARGUMENTS

Use the `outside-in-rgr-microcycle` skill and keep a visible RGR ledger. The primary implementer orchestrates; the RED/GREEN/review agents own their steps. Do not skip from RED to broad implementation, and do not accept multi-failure RED output.

Workflow:

1. Dispatch `rgr-test-author` to write or activate the next smallest failing test and capture exactly one RED failure.
2. Dispatch `rgr-test-reviewer`, record RED, and call `rgr_approve_red` before any production edit.
3. If the reviewer vetoes, return to `rgr-test-author` with the mandatory notes.
4. Dispatch `rgr-diagnostic-implementer` with the current diagnostic and allowed immediate change.
5. If the diagnostic is ambiguous, require a lower-level unit test and route it through test review.
6. After one behavioral production edit, rerun the focused command. When the failure changes, record the new RED and return control to the orchestrator.
7. When the focused test passes, dispatch `rgr-implementation-reviewer` for the production diff.
8. If the reviewer vetoes, return to `rgr-diagnostic-implementer` with the mandatory notes.
9. Continue one diagnostic at a time until all current cycle tests pass.
10. Commit the approved GREEN/refactor checkpoint before the next RED, then run focused verification before handoff and state any skipped broader gate.

Do not commit unrelated work; approved GREEN/refactor checkpoints should be committed before starting the next RED unless the user explicitly says not to commit.
