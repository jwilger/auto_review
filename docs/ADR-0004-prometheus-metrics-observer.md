# ADR-0004: Prometheus Metrics and Observer Boundary

## Status

Accepted

## Date

2026-05-01

## Provenance

Reconstructed from metrics introduction and follow-up hardening in commits
`06bad45`, `c5d1fd3`, `7cd89fb`, `b4636b5`, and `f25f47f`; deployment/dashboard
follow-ups `2096cd1`, `c92c013`, `3461817`, and `4515922`; and documentation
commit `02b5e64`.

## Context

`auto_review` needs operator-visible metrics without coupling orchestration logic
to the gateway's HTTP and Prometheus implementation details. Earlier
observability material lived with ADR-0003, but the metrics contract became large
enough to warrant its own decision record.

## Decision

Expose gateway metrics in Prometheus text format and keep the collection boundary
explicit through a `ReviewObserver` interface between `ar-orchestrator` and
`ar-gateway`.

The orchestrator reports review lifecycle events through the observer
abstraction. The gateway owns metric recording, Prometheus exposition, and HTTP
serving. This keeps the review state machine testable and reusable while
allowing deployment-specific metrics to evolve in the gateway.

The stable metric surface includes:

- a review-duration histogram for completed review work;
- poller counters for chat polling activity and outcomes;
- gateway and review counters that preserve low cardinality suitable for
  Prometheus alerting and dashboards.

Metric names, label sets, dashboard panels, and alerting rules are treated as a
coupled operator contract. Changes to stable metrics must update the matching
Prometheus rules, Grafana dashboard, tests, and operator documentation in the
same change.

## Consequences

- Prometheus remains the supported metrics integration point, using plain text
  exposition rather than a vendor-specific metrics backend.
- The observer boundary prevents `ar-orchestrator` from depending on Prometheus
  crates or gateway runtime concerns.
- Metrics are intentionally conservative: low cardinality, stable names, and
  deployment artifacts that change together with the emitted metric surface.
- Operational follow-ups should preserve this coupling so CI can catch drift
  between emitted metrics, dashboard queries, and alert rules.
