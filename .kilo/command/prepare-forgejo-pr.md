---
description: Prepare a scoped Forgejo PR with explicit-path staging and CHANGELOG checks.
agent: auto-review-rust-implementer
---

Prepare a Forgejo PR for: $ARGUMENTS

Workflow:

1. Audit scope with `git status` and diffs.
2. Stage only explicit paths; do not use `git add .`, `git add -A`, `git add -u`, or `git commit -a`.
3. Check whether `CHANGELOG.md` needs an `[Unreleased]` entry.
4. Verify relevant gates.
5. Use `tea pr create --repo jwilger/auto_review --head <branch> --base main --title "..." --description "..."`.

Do not use `gh` for this repo.
