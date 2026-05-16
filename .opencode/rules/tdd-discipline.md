# TDD Discipline

Behavior production code requires one observed failing test first. RED evidence must show exactly one failing test or one intentional compiler/API diagnostic for the current behavior; multi-failure output is not valid RED.

Follow RED -> REVIEW -> GREEN -> REVIEW -> REFACTOR with the specialist RGR agents. Record RED, get RED approval, then make only the smallest production edit that changes the current diagnostic. After one behavioral production edit, rerun the focused command and record the changed RED or GREEN before editing again.

Default code-writing handoff: use `rgr-test-author` to write or activate the next single RED, `rgr-test-reviewer` to approve RED before production edits, `rgr-diagnostic-implementer` to make one smallest GREEN edit for the named diagnostic and allowed immediate change, and `rgr-implementation-reviewer` to approve GREEN before refactor or broader verification. If GREEN review finds a missing behavior not covered by the passing test, return it to the orchestrator as the next RED instead of asking for untested implementation. The primary implementer orchestrates, keeps the ledger, and commits each approved GREEN/refactor checkpoint before starting the next RED.

Exemptions are narrow: docs-only work, pure moves or renames, generated lockfile churn, and non-behavioral chores. Do not add deterministic tests that assert documentation wording for docs-only changes; review the docs instead. If a test is hard to write for production behavior, extract a testable seam instead of skipping RED.
