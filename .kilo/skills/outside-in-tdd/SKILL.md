---
name: outside-in-tdd
description: RGR sequence, observed-failure evidence, drill-down unit tests, and non-behavioral exemptions for auto_review.
---

# Outside-In TDD

Use this skill for behavior changes and bug fixes.

## Rule

Never write production behavior without an observed failing test demanding it.

## Sequence

1. Name the behavior and the smallest externally visible test that should fail.
2. Write or activate that test.
3. Run the focused command and capture real failing output.
4. Record RED with the RGR ledger tool before editing production Rust.
5. Implement only the minimum code that changes the failure.
6. Run the focused test and record GREEN.
7. Refactor only while tests are green, then record REFACTOR.

## Drill-Down

When an integration or acceptance failure points at internal logic, add a focused unit test for that internal behavior, observe it fail, make it pass, then return to the outer test.

## Evidence

Observed failure output must be copied from an actual run, not paraphrased. Commit bodies should explain why and include the RED command/output for behavior commits when practical.

## Exemptions

RED is not required for docs-only changes, pure renames or moves where existing tests cover behavior, generated lockfile updates, and mechanical config chores. If a production Rust edit changes observable behavior, the exemption does not apply.
