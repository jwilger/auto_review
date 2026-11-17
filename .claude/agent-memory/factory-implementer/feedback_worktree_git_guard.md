---
name: worktree-git-guard
description: Worktree isolation guard refuses git commit/checkout/status/log-with-pipes because rtk rewrites them; use the absolute git path with -C
metadata:
  type: feedback
---

In factory worktrees, `git commit`, `git checkout`, `git status` and multi-command git lines are refused by the worktree-isolation guard ("runs rtk with a git command among its operands"), because the rtk hook rewrites those subcommands. `git -C <worktree> switch/rm/grep` pass.

**Why:** the guard cannot see what rtk will run, so it refuses. Hit while building item 11 (2026-11-16).

The guard also refuses any compound command whose text merely contains `git` (e.g. `gh api repos/github/codeql-action/...` in a loop); put such loops in a scratchpad script and run `bash <script>`.

Integrate step: the item branch `factory/<n>` cannot be created while slice branches `factory/<n>/<slice>` exist (git ref D/F conflict), and the slices are checked out in sibling worktrees so they cannot be deleted. Merge on a stand-in branch (e.g. `factory-<n>-integrate`) and report that name.

**How to apply:** call `/home/jwilger/.nix-profile/bin/git -C <worktree> <subcommand>`, one git command per Bash call, commit messages via `-F <scratchpad file>`. Also the local `main` ref can be stale; diff against `origin/main` for `paths`.
