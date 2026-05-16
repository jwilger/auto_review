# ADR-0012: Gate Semantic Review Behind CI

## Status

Accepted

## Date

2026-05-04

## Provenance

Reconstructed from implementation and documentation commit `d3ee9d4` on
2026-05-04. Related Forgejo action and compare fallback work appears in commits
`c42d522` and `f5eb3ed`.

## Context

`auto_review` receives regular Forgejo webhooks for pull request activity,
comments, and chat commands. Earlier versions could dispatch semantic review
directly from this webhook path, which made expensive LLM review work depend
primarily on webhook timing rather than on project-specific readiness criteria.

Projects already express many readiness rules through CI: formatting, tests,
linters, build checks, repository policy, or other prerequisites selected by the
repository. Running semantic review before those checks pass can waste model
budget on changes that are not yet reviewable and can produce noisy feedback for
issues deterministic tooling will catch first.

## Decision

Make CI-gated dispatch the normal semantic review path.

Regular Forgejo webhooks perform low-cost intake only. They verify and record
pull request state, update visible status, and handle `@auto_review` chat
commands, but they do not normally start semantic review work.

Semantic review dispatch occurs from the Forgejo Actions/CI path after the
repository-selected prerequisite checks have passed. This makes deterministic
project gates the readiness signal for LLM review and keeps the expensive review
pipeline behind checks the project owner controls.

Explicit chat commands, such as `@auto_review re-review`, may force a semantic
review even when the normal CI-gated path would not dispatch. This preserves
operator control for retries, manual investigation, and exceptional workflows.

## Consequences

- Semantic review normally runs only after deterministic project gates indicate
  the pull request is ready, reducing duplicate or low-value model work.
- Webhook handling stays cheap and responsive because it focuses on validation,
  bookkeeping, status updates, and chat command routing.
- Repositories can choose their own prerequisite checks without changing the
  semantic review pipeline itself.
- Maintainers retain an escape hatch through explicit re-review commands for
  retries and exceptional workflows.
- CI/action dispatch, webhook intake, and chat-forced review paths must remain
  clearly separated so future changes do not accidentally reintroduce
  unconditional semantic review from regular webhooks.
