---
name: forgejo-feedback-protocol
description: Process Forgejo PR feedback with reflection, guardrail-gap classification, and inline thread replies.
---

# Forgejo Feedback Protocol

Use this skill for every actionable PR review comment.

## Process

1. Fetch review comments with `tea` or the Forgejo REST API.
2. For each item, write a short reflection: why was the correct thing not done first?
3. Classify as `guardrail-gap` or `one-off` using `review-taxonomy`.
4. Remediate the code or guardrail according to the classification.
5. Reply on each existing inline thread before posting any top-level summary.

## Inline Reply Rule

Forgejo threads replies by review, path, and diff position. For an inline reply, post to the existing review comments endpoint for the review that owns the comment:

```text
POST /api/v1/repos/{owner}/{repo}/pulls/{pr}/reviews/{comment.pull_request_review_id}/comments
```

Use this reply endpoint even if another comments endpoint accepts a body; top-level issue/PR comment endpoints and new-review-comment payloads create new threads and do not count as addressing inline feedback. The payload must reuse the original inline comment's `path` and `position`:

```json
{
  "body": "<reply>",
  "path": "<comment.path>",
  "new_position": <comment.position>,
  "old_position": 0
}
```

Begin the `body` with an @-mention of the original comment author, using `@<comment.user.login>`, so the reviewer being answered is notified and the thread remains attributable.

Do not use prose line numbers from the comment body as `new_position`.

Before declaring feedback addressed, confirm the response URL anchors to the same review thread/comment family, not a new top-level PR comment.

## Top-Level Comments

Top-level PR comments are allowed only after all actionable inline threads have a per-thread response on their existing threads.
