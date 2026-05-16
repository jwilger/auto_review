# ADR-0003: Gateway Runtime Visibility Endpoints

## Status

Accepted

## Date

2026-05-01

## Provenance

Reconstructed from runtime visibility endpoint work in commits `4b230c5`,
`0c13a82`, and `f670d96`. The material was previously part of
`docs/ADR-0003-observability.md`, which was created in commit `02b5e64` on
2026-05-01. This ADR splits the gateway runtime introspection endpoint decision
out from broader observability, metrics, and boundary-defense decisions.

## Context

`auto_review` runs as a self-hosted Forgejo review bot. Operators need to
understand whether the gateway process is alive, whether it is ready to serve
traffic, which version is deployed, how it was configured at startup, and
whether Prometheus can scrape operational counters.

The initial operational model relied heavily on process logs. Logs remain useful
for debugging individual failures, but they are not enough for deployment
automation, readiness probes, smoke tests, or routine operator checks. Runtime
visibility needs a small HTTP surface with stable semantics so deployment systems
and humans can ask narrow questions without conflating different operational
states.

## Decision

The gateway exposes five runtime visibility routes, each separated by
operational semantics:

| Endpoint | Purpose | Operational semantics |
| --- | --- | --- |
| `GET /healthz` | Process liveness | Cheap liveness check with no downstream I/O. |
| `GET /readyz` | Service readiness | Readiness check for required runtime dependencies through configured clients. |
| `GET /version` | Deployment identity | Static version response for smoke tests and deployment confirmation. |
| `GET /info` | Runtime configuration snapshot | Startup-time operational information for diagnosis and issue reports. |
| `GET /metrics` | Prometheus scrape target | Prometheus text exposition endpoint, not a liveness/readiness signal. |

The routes are intentionally separate rather than combined into a single status
endpoint. Each route has one operational contract and can be wired independently
by deployment tooling.

## Consequences

- Operators get a small, predictable runtime introspection surface.
- Deployment systems can distinguish liveness from readiness instead of
  restarting a healthy process during downstream outages.
- Smoke tests can verify deployed version identity without parsing logs.
- Runtime configuration information has a dedicated endpoint rather than being
  mixed into health checks or metrics.
- Prometheus scraping is isolated from liveness and readiness semantics.
- The gateway has more public routes to document and keep stable.

## Related notes

- ADR-0004 records detailed metrics design, Prometheus rule coupling, Grafana
  dashboard coupling, and review lifecycle instrumentation.
- ADR-0005 records gateway request-boundary defenses.
- ADR-0009 records the later CI-triggered semantic review intake route.
