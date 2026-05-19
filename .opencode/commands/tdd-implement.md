---
description: Implement behavior through an explicit RED-GREEN-REFACTOR cycle.
agent: build
---

Use the `outside-in-tdd`, `outside-in-rgr-microcycle`, and `rgr-plan-structure` skills for: $ARGUMENTS

This command is a compatibility entry point for the specialist-agent RGR workflow. Do not perform code-writing steps directly when the RED/GREEN/review agents can own the step.

Workflow:

1. Start a cycle with `rgr_start`, naming the behavior and focused command before delegating test writing.
2. Identify the smallest failing test for the requested behavior and narrow it to exactly one RED failure.
3. Dispatch `rgr-test-author` to write or activate that test and run the focused command.
4. Dispatch `rgr-test-reviewer` to approve RED before any production edit.
5. Record RED with the RGR ledger tool, including command and real output, then call `rgr_approve_red`.
6. Dispatch `rgr-diagnostic-implementer` with the current diagnostic and allowed immediate change.
7. Run the focused test after one behavioral edit; use `rgr_record_changed_diagnostic`/`rgr_approve_changed_diagnostic` for an expected changed failure, or `rgr_record_proof_of_work_verification` and `rgr_mark_green` when the command passes.
8. Dispatch `rgr-implementation-reviewer` to approve the GREEN diff.
9. Refactor only with tests green and reviewer-approved, then `rgr_mark_refactor` and commit the approved checkpoint before the next RED.
10. Run the strongest relevant verification gate feasible before handoff.
