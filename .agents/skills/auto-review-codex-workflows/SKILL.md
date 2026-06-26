---
name: auto-review-codex-workflows
description: Use for auto_review repo workflows formerly exposed as project commands: RGR implementation, bug fixes, safe refactors, verification, local review, Forgejo PR prep, and Forgejo feedback processing.
---

# auto_review Codex Workflows

Use these workflows as Codex-native replacements for project command entry
points. Do not recreate command files or custom prompts for shared workflows.

## RGR Implementation

Use for behavior changes and bug fixes.

1. Start an RGR cycle with `python3 scripts/codex/rgr.py start`, naming the observable behavior and focused command.
2. Use `outside-in-tdd`, `outside-in-rgr-microcycle`, and `rgr-plan-structure` for test shape and ledger discipline.
3. Dispatch `rgr-test-author` for the smallest next RED test and `rgr-test-reviewer` to review the RED evidence.
4. Record and approve RED before production edits.
5. Dispatch `rgr-diagnostic-implementer` with one current diagnostic and the allowed immediate change.
6. After one behavioral production edit, rerun the focused command. Record changed diagnostics before another edit, or record proof and mark GREEN when it passes.
7. Dispatch `rgr-implementation-reviewer` before refactor or broad verification.
8. Commit each approved GREEN/refactor checkpoint before starting the next RED unless the user explicitly says not to commit.

## Safe Refactor

Use only when behavior is intended to remain unchanged.

1. Identify and run the focused tests that cover the target area before editing.
2. Confirm the baseline is green.
3. Make one small refactor.
4. Rerun the same focused tests.
5. Stop and diagnose if behavior changes or tests fail.

## Verification

Prefer focused checks first, then broader gates as needed.

```sh
just fmt
just clippy
just test
just codex-test
just deny
just build
just ci
```

Use `just ci` for the aggregate routine gate. State any skipped gate and why.

## Local Review

For committed branch review, compare the current branch against its base branch.
For uncommitted review, inspect staged, unstaged, and untracked files.

Dispatch read-only review subagents when useful:

- `architecture-reviewer` for architecture, crate boundaries, docs, env parsing, errors, and observability.
- `test-coverage-reviewer` for RGR evidence and test coverage.
- `security-reviewer` for threat model, sandboxing, secrets, auth, and dependency risks.

Return findings first, ordered by severity, with file and line references where possible. Do not edit files during review.

## Forgejo PR Prep

1. Audit scope with `git status` and diffs.
2. Stage only explicit paths; never use `git add .`, `git add -A`, `git add -u`, or `git commit -a`.
3. Use conventional commit titles and include a short body explaining why the change is needed.
4. Verify relevant gates.
5. Prefer Forgejo MCP for PR creation. Use `tea pr create` only as a fallback.
6. Include a closure trailer in the PR description for issue-linked branches, such as `Closes #123`, `Fixes #123`, or `Resolves #123`.
7. Do not use `gh` for this repo.

## Forgejo Feedback Processing

Use `forgejo-feedback-protocol` and `review-taxonomy`.

1. Fetch comments and review threads.
2. Reflect on each actionable item before editing.
3. Classify each item as `guardrail-gap` or `one-off`.
4. Remediate the issue.
5. Reply on the existing inline review thread before any top-level summary.

Never create a new PR comment or new inline thread as the response to an existing inline thread.
