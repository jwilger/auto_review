---
name: integrate-branch-ref-collision
description: Integrate step cannot create factory/<id> while slice refs factory/<id>/S* exist (git ref D/F conflict); slices may also drift from the criteria
metadata:
  type: project
---

Git cannot hold both `refs/heads/factory/37` and `refs/heads/factory/37/S1-...`, so the integrate step's target branch `factory/<id>` cannot be created while slice branches exist, and the slice branches are checked out in other worktrees so they cannot be deleted. Seen on item 37 (2026-11-17): I integrated on the worktree branch and reported BLOCKED/environment.

Also on item 37 the slice branches were linear (S1 = S2, S3 on top) and built to an older contract (different marker text and labels); the integrate step had to realign code, tests and docs to the given criteria.

**Why:** the orchestrator names the item branch as a prefix of the slice branches.

**How to apply:** at integrate, check `git for-each-ref refs/heads/factory/<id>` first; if only slice refs exist, do the work on the worktree branch and report the collision. Always check slice output against every criterion rather than trusting a green gate.
