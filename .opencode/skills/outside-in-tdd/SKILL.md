---
name: outside-in-tdd
description: RGR sequence, observed-failure evidence, drill-down unit tests, and non-behavioral exemptions for auto_review.
---

# Outside-In TDD

Use this skill for behavior changes and bug fixes. This skill defines the discipline; the specialist RGR agents perform the writing and review handoffs. Prefer the full `outside-in-rgr-microcycle` workflow whenever code will be written.

## Rule

Never write production behavior without one observed failing test demanding it. Multi-failure output is not valid RED; narrow the command or split the behavior until exactly one test or one intentional compiler/API diagnostic is failing.

## Sequence

1. Name the behavior and the smallest externally visible test that should fail.
2. Dispatch `rgr-test-author` to write or activate that test, run the focused command, and capture real failing output.
3. Dispatch `rgr-test-reviewer` to approve the RED evidence and API pressure before production edits.
4. Record RED and RED approval with the RGR ledger tools before editing production behavior.
5. Dispatch `rgr-diagnostic-implementer` with the current diagnostic and allowed immediate change; it implements only the minimum code that changes that diagnostic.
6. Run the focused test after that edit. If the failure changes, record the new RED and repeat review/implementation for that diagnostic. If it passes, record GREEN.
7. Dispatch `rgr-implementation-reviewer` to approve the GREEN diff before refactor or broader verification.
8. Refactor only while tests are green and reviewer-approved, then record REFACTOR and commit the approved checkpoint before the next RED.

## Drill-Down

When an integration or acceptance failure points at internal logic, route the lower-level unit test through `rgr-test-author` and `rgr-test-reviewer`, observe it fail, use `rgr-diagnostic-implementer` for the minimum GREEN change, then return to the outer test.

## Evidence

Observed failure output must be copied from an actual run, not paraphrased. It must identify one current failing test or diagnostic. Commit bodies should explain why and include the RED command/output for behavior commits when practical.

## Exemptions

RED is not required for docs-only changes, pure renames or moves where existing tests cover behavior, generated lockfile updates, and mechanical config chores. Do not create deterministic tests that assert documentation wording for docs-only changes; use human/operator review for those changes unless the documentation is generated from or consumed by executable behavior. If a production Rust edit changes observable behavior, the exemption does not apply.
