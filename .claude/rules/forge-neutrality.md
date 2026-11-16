---
paths:
  - "crates/ar-forge/**"
  - "crates/ar-forgejo/**"
  - "crates/ar-github/**"
  - "crates/ar-orchestrator/**"
  - "crates/ar-review/**"
---

Forge-specific code stays behind the `ReviewHost` trait in `ar-forge` (ADR-0020). Pipeline and orchestrator code depends on `ar-forge` types only; Forgejo and GitHub REST details live in `ar-forgejo` and `ar-github`.
