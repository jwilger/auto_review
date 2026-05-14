---
description: Refactor with a green baseline and focused verification.
argument-hint: "<scope>"
---

Refactor safely: $ARGUMENTS

Workflow:

1. Identify and run the focused tests that cover the area before editing.
2. Confirm the baseline is green.
3. Make one small refactor that should not change behavior.
4. Rerun the same focused tests.
5. Stop if behavior changes or tests fail; diagnose before continuing.
