---
name: outside-in-tdd
description: RGR sequence, observed-failure evidence, drill-down unit tests, and non-behavioral exemptions for auto_review.
---

# Outside-In TDD

Use this skill for behavior changes and bug fixes. This skill defines the discipline; the specialist RGR agents perform the writing and review handoffs. Prefer the full `outside-in-rgr-microcycle` workflow whenever code will be written.

## Rule

Never write production behavior without an observed failing test demanding it. "Observed" means copied output from a command that actually ran in this session or an explicitly delegated subagent session. Inspection, reasoning, expected failure descriptions, and unavailable commands are not RED or GREEN evidence.

## BDD First

Start each behavior change with one externally visible contract: user-facing behavior, CLI/API behavior, generated artifact, security boundary, or tool contract. Do not begin by unit-testing helpers, schemas, or implementation modules unless an outer behavior is already RED and the next implementation decision is ambiguous.

## Microcycle Discipline

Each RED-GREEN-REFACTOR cycle may introduce one behavioral assertion or one compiler/API pressure point. If a test change asserts multiple independent behaviors, split it before production edits. GREEN work may address only the current diagnostic. When the diagnostic changes, stop and hand control back instead of continuing to fix predicted next errors.

## Sequence

1. Name the single external behavior and the smallest focused command that exercises it.
2. Dispatch `rgr-test-author` to write or activate exactly one failing behavioral assertion, run the focused command, and capture real failing output.
3. Dispatch `rgr-test-reviewer` to approve the RED evidence, behavior focus, and assertion size before production edits.
4. Record RED with the RGR ledger tool using copied command output only. If the command cannot run, do not record RED; report a blocked state and propose the missing semantic tool.
5. Dispatch `rgr-diagnostic-implementer` to implement only the minimum code that removes or changes one current diagnostic.
6. Run the focused test. If it passes, record GREEN. If the failure changes, stop and start/review the next microcycle; do not keep editing.
7. Dispatch `rgr-implementation-reviewer` to approve the GREEN diff before refactor or broader verification.
8. Refactor only while tests are green and reviewer-approved, then record REFACTOR.

## Drill-Down

When an integration or acceptance failure points at internal logic, route the lower-level unit test through `rgr-test-author` and `rgr-test-reviewer`, observe it fail, use `rgr-diagnostic-implementer` for the minimum GREEN change, then return to the outer test.

## Evidence

Observed failure output must be copied from an actual run, not paraphrased. Commit bodies should explain why and include the RED command/output for behavior commits when practical.

## Exemptions

RED is not required for docs-only changes, pure renames or moves where existing tests cover behavior, generated lockfile updates, and mechanical config chores. Do not create deterministic tests that assert documentation wording for docs-only changes; use human/operator review for those changes unless the documentation is generated from or consumed by executable behavior. If a production Rust edit changes observable behavior, the exemption does not apply.
