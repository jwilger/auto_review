---
description: Prepare a scoped Forgejo PR with explicit-path staging and conventional commit checks.
agent: auto-review-rust-implementer
---

Prepare a Forgejo PR for: $ARGUMENTS

Workflow:

1. Audit scope with `git status` and diffs.
2. Stage only explicit paths; do not use `git add .`, `git add -A`, `git add -u`, or `git commit -a`.
3. Check commit titles follow conventional commits; the release PR generates changelog notes from conventional commits.
4. Verify relevant gates.
5. Use `tea pr create --repo jwilger/auto_review --head <branch> --base main --title "..." --description "..."`.

Do not use `gh` for this repo.
