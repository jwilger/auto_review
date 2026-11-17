---
name: worktree-git-guard
description: Worktree isolation guard refuses git commit/checkout/status/log-with-pipes because rtk rewrites them; use the absolute git path with -C
metadata:
  type: feedback
---

In factory worktrees, `git commit`, `git checkout`, `git status` and multi-command git lines are refused by the worktree-isolation guard ("runs rtk with a git command among its operands"), because the rtk hook rewrites those subcommands. `git -C <worktree> switch/rm/grep` pass.

**Why:** the guard cannot see what rtk will run, so it refuses. Hit while building item 11 (2026-11-16).

**How to apply:** call `/home/jwilger/.nix-profile/bin/git -C <worktree> <subcommand>`, one git command per Bash call, commit messages via `-F <scratchpad file>`. Also the local `main` ref can be stale; diff against `origin/main` for `paths`.
