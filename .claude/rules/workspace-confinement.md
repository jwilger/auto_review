---
paths:
  - "crates/ar-review/src/workspace.rs"
  - "crates/ar-review/src/workspace_tools.rs"
  - "crates/ar-review/tests/red_team_workspace_tools.rs"
---

LLM workspace tools are read-only and confined to the clone root (ADR-0011, threat T4). Reject `..`, absolute paths and symlinks that resolve outside the clone; keep token redaction and hermetic git intact. A change here updates `crates/ar-review/tests/red_team_workspace_tools.rs` in the same change.
