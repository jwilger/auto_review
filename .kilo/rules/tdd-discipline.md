# TDD Discipline

Behavior production code requires an observed failing test first. Follow RED -> GREEN -> REFACTOR, record the failing output in the RGR ledger, and make the smallest production edit that changes the observed failure.

Exemptions are narrow: docs-only work, pure moves or renames, generated lockfile churn, and non-behavioral chores. If a test is hard to write, extract a testable seam instead of skipping RED.
