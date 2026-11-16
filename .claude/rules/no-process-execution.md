---
paths:
  - "crates/ar-review/**"
  - "crates/ar-gateway/**"
---

The review runtime never executes repository-controlled tools, scripts or shell commands (ADR-0010, threat T1 in `docs/THREAT-MODEL.md`). Do not add `std::process::Command` or `tokio::process` to the review path. Deterministic linters, tests and builds run in the repository's CI before semantic review, not inside auto_review.
