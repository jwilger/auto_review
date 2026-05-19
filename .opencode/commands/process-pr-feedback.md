---
description: Process Forgejo PR feedback with reflection, classification, remediation, and inline replies.
agent: forgejo-feedback-processor
---

Process Forgejo PR feedback: $ARGUMENTS

Use `forgejo-feedback-protocol` and `review-taxonomy`. Fetch comments, reflect on each actionable item, classify as `guardrail-gap` or `one-off`, remediate, and reply on each existing inline review thread before any top-level summary. Never create a new PR comment or new inline thread as the response to an existing inline thread.
