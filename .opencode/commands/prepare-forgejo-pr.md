---
description: Prepare a scoped Forgejo PR with explicit-path staging and conventional commit checks.
agent: build
---

Prepare a Forgejo PR for: $ARGUMENTS

Workflow:

1. Audit scope with `git status` and diffs.
2. Stage only explicit paths; do not use `git add .`, `git add -A`, `git add -u`, or `git commit -a`.
3. Check commit titles follow conventional commits; the release PR generates changelog notes from conventional commits.
4. Ensure commit message body (or PR description body for this PR) includes why the change is needed.
   Prefer:

   ```text
   Why:
   - <reason / problem / risk addressed>

   What:
   - <specific change made>

   Validation:
   - <focused checks run>
   ```

5. Verify relevant gates.
6. Create the PR on Forgejo:
   - **Preferred:** use `forgejo_create_pull_request` via MCP (for example: `forgejo_create_pull_request --owner jwilger --repo auto_review --base main --head <branch> --title "..." --body "..."`).
   - **Fallback only:** `tea pr create --repo jwilger/auto_review --head <branch> --base main --title "..." --description "..."`.
7. Ensure PR description includes one closure trailer for issue-linked branches, such as:
    - `Closes #123`
    - `Fixes #123`
    - `Resolves #123`

Do not use `gh` for this repo.
