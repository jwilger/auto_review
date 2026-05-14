---
description: Process Forgejo PR feedback with reflection, classification, remediation, and inline replies.
argument-hint: "<PR or feedback reference>"
---

Process Forgejo PR feedback: $ARGUMENTS

Use `forgejo-feedback-protocol` and `review-taxonomy`. Dispatch `forgejo-feedback-processor` via the Pi subagents `Agent` tool. Fetch comments, reflect on each actionable item, classify as `guardrail-gap` or `one-off`, remediate, and reply on each inline thread before any top-level summary.
