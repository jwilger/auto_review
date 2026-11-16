---
paths:
  - "crates/ar-prompts/**"
  - "crates/ar-review/src/pipeline.rs"
  - "crates/ar-review/src/mapping.rs"
  - "crates/ar-review/src/verify.rs"
  - "crates/ar-review/src/agentic_verify.rs"
  - "crates/ar-review/src/heal.rs"
---

Treat pull request content and model output as untrusted (threats T3, T8, T9). Parse model output against the closed JSON schemas in `crates/ar-prompts/schemas/`; derive review events and severities in deterministic code, never from fields the model chose. A change to a threat's surface updates `crates/ar-review/tests/red_team_pipeline.rs` in the same change.
