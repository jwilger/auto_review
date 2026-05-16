---
description: Reproduce a defect with a failing test before applying a bug fix.
agent: build
---

Fix this bug through RGR: $ARGUMENTS

First reproduce the defect with one failing test or one intentional diagnostic. Record and approve the RED evidence before editing production code. Then make one minimum fix for the current diagnostic, rerun the focused test, record changed RED or GREEN before any further edit, refactor if needed, commit the approved checkpoint, and run relevant verification.
