---
description: Run a fine-grained outside-in RGR workflow with specialist agents.
agent: build
---

Run the specialist outside-in RGR workflow for: $ARGUMENTS

Use the `outside-in-rgr-microcycle` skill and keep a visible RGR ledger. The primary implementer orchestrates; the RED/GREEN/review agents own their steps. Do not skip from RED to broad implementation, and do not accept multi-failure RED output.

Workflow:

1. Start a cycle with `rgr_start`, naming the behavior and focused command.
2. Dispatch `rgr-test-author` to write or activate the next smallest failing test and capture exactly one RED failure.
3. Dispatch `rgr-test-reviewer`, record RED, and call `rgr_approve_red` before any production edit.
4. If the reviewer vetoes, return to `rgr-test-author` with the mandatory notes.
5. Dispatch `rgr-diagnostic-implementer` with the current diagnostic and allowed immediate change.
6. If the diagnostic is ambiguous, require a lower-level unit test and route it through test review.
7. After one behavioral production edit, rerun the focused command. When the failure changes, use `rgr_record_changed_diagnostic` and `rgr_approve_changed_diagnostic` before the next GREEN turn.
8. When the focused test passes, record proof-of-work verification, call `rgr_mark_green`, then dispatch `rgr-implementation-reviewer` for the production diff.
9. If the reviewer vetoes, return to `rgr-diagnostic-implementer` with the mandatory notes or use `rgr_recover_implementation_review_veto` when recovering a guarded state.
10. Continue one diagnostic at a time until all current cycle tests pass.
11. Commit the approved GREEN/refactor checkpoint before the next RED, then run focused verification before handoff and state any skipped broader gate.

Do not commit unrelated work; approved GREEN/refactor checkpoints should be committed before starting the next RED unless the user explicitly says not to commit.
