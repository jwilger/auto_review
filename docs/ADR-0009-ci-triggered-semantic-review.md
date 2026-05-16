# ADR-0009: CI-Triggered Semantic Review Intake

## Status

Accepted

## Date

2026-05-03

## Provenance

Reconstructed from a mutation in former `docs/ADR-0003-observability.md`
introduced by commit `83d3dc6` on 2026-05-03.

## Context

The semantic review pipeline needs a reliable intake path from CI after
deterministic checks have completed. Forgejo webhooks remain responsible for pull
request activity intake, but CI has additional context about whether prerequisite
checks have passed and which head SHA was verified.

Using the generic webhook intake for this signal would blur two different event
sources and could incorrectly classify valid CI requests as webhook anomalies.
The CI path also needs an explicit authentication boundary because it is a direct
request to enqueue semantic review work.

## Decision

Add an authenticated `POST /reviews/ci` endpoint for CI to request semantic
review after deterministic checks complete.

Requests to this endpoint are token-gated with `AR_CI_REVIEW_TOKEN`. The CI
caller sends the repository owner, repository name, pull request number, head
SHA, and trigger metadata needed to identify and audit the review request.

CI review intake is handled separately from Forgejo webhook anomaly accounting.
Requests to `POST /reviews/ci` are not counted as webhook anomaly metrics.

## Consequences

- CI can explicitly request semantic review only after deterministic
  prerequisites have run.
- The gateway retains a clear distinction between Forgejo webhook intake and
  CI-triggered intake.
- Operators must provision `AR_CI_REVIEW_TOKEN` for the gateway and configure CI
  to present the matching token.
- Review dispatch can use the supplied head SHA and trigger metadata to tie
  semantic review work to the exact CI-validated revision.

## Later development

ADR-0012 later makes CI gating the normal semantic review path.
