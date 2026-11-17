---
name: scratch-python-scripts
description: Multi-line python heredoc edits are refused by the worktree guard; scratch scripts must not shadow stdlib names and should run with python3 -I
metadata:
  type: feedback
---

Long `python3 - <<'EOF'` heredoc edits inside a Bash call are refused by the worktree guard as "too complex to verify". Write the edit script to the scratchpad and run it with `python3 -I <path>`. Never name a scratch script after a stdlib module (`types.py`, `re.py`, `json.py`): without `-I`, a later script in the same dir imports it and silently re-runs its edits.

**Why:** item 50 (2026-11-17): a scratch `types.py` was re-executed via `import re` -> `enum` -> `types`, duplicating a Rust enum block in the source.

**How to apply:** distinct script names (`edit_types.py`), always `python3 -I`. See also [[worktree-git-guard]].
