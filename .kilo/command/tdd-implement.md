---
description: Implement behavior through an explicit RED-GREEN-REFACTOR cycle.
agent: auto-review-rust-implementer
---

Use the `outside-in-tdd` and `rgr-plan-structure` skills for: $ARGUMENTS

For fine-grained specialist-agent orchestration, use `/outside-in-rgr`.

Workflow:

1. Identify the smallest failing test for the requested behavior.
2. Write or activate that test and run the focused command.
3. Record RED with the RGR ledger tool, including command and real output.
4. Make the minimum production edit.
5. Run the focused test and record GREEN.
6. Refactor only with tests green and record REFACTOR.
7. Run the strongest relevant verification gate feasible before handoff.
