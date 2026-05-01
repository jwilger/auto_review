# ADR-0003: Observability and runtime introspection

**Status**: Accepted
**Date**: 2026-04-30

## Context

`auto_review` is a single-tenant self-hosted bot operators run
next to their Forgejo instance. It holds a write-scoped PAT,
shells out to ~44 linters, calls remote LLMs, and serves
webhooks from public-internet sources. When something goes
wrong — a Forgejo blip, an LLM timeout, a poisoned `.rubocop.yml`
hitting the sandbox, a misconfigured webhook firing in a tight
loop — operators need to figure out *what* and *why* without
SSH'ing into the box.

The default v1 trajectory was "log to stdout, let operators
grep". That works for a single-pod systemd deploy with two
PRs/day. It does not scale to:
- A k8s deployment that needs liveness vs readiness probes
  wired separately so transient downstream blips don't restart
  the pod.
- An on-call engineer who needs to know whether the bot is
  failing because Forgejo is down, the LLM is down, or the
  sandbox itself is misconfigured — *before* the first PR of
  the morning fires.
- An SRE wiring SLO alerts in Prometheus/Alertmanager.

This ADR documents the design choices made over Milestones 5+
to make the bot observable.

## Decisions

### 1. HTTP endpoints for runtime visibility

The gateway exposes five HTTP routes for ops use:

| Endpoint | Purpose | Example use |
|----------|---------|-------------|
| `GET /healthz` | Process liveness. Cheap; no I/O. | k8s `livenessProbe` |
| `GET /readyz` | Forgejo reachable through the bot's actual client. Async-Mutex-guarded TTL cache so probes don't hammer Forgejo. Returns 503 when downstream Forgejo is down. | k8s `readinessProbe` |
| `GET /version` | Static `{name, version}`. Useful for confirming a deploy. | Smoke test after `systemctl restart` |
| `GET /info` | Runtime-config snapshot — sandbox kind, learnings store kind, LLM tiers configured, poller status. Captured once at startup. | Issue-report attachment |
| `GET /metrics` | Prometheus text-format counters. | Scrape config |

Distinct routes (rather than one mega-endpoint) so each can be
bound to its k8s/probe semantics independently. `/healthz` and
`/readyz` are intentionally cheap; `/metrics` is the heavy one
and is hit on whatever Prometheus's scrape interval is.

### 2. ReviewObserver trait keeps orchestrator metrics-agnostic

The metrics format is a gateway concern, not an orchestrator
concern. We don't want `ar_orchestrator` to depend on a metrics
crate or on `ar_gateway`'s `Metrics` type. The dependency arrow
must stay `ar_gateway → ar_orchestrator`.

Solution: a small `ReviewObserver` trait + `ReviewObservation`
enum in `ar-orchestrator`. The orchestrator emits observations
at lifecycle transitions (Started / Succeeded / Failed /
Skipped); the gateway provides a `MetricsObserver` that
implements the trait by delegating to its `AtomicU64` counters.
Tests can swap a recording observer for assertion.

```
┌────────────────┐  observation  ┌─────────────────┐
│ ar-orchestrator│──────────────▶│  ReviewObserver │
│  (lifecycle)   │   trait call  │     (trait)     │
└────────────────┘               └────────┬────────┘
                                          │
                                          ▼
                            ┌─────────────────────────┐
                            │  ar-gateway::metrics    │
                            │   ::MetricsObserver     │
                            │  → AtomicU64 counters   │
                            └─────────────────────────┘
```

Same pattern for the chat poller's metrics — `ChatPoller`
takes an optional `Arc<Metrics>` and increments cycles /
mention-dispatches / failures from inside its loop.

### 3. AtomicU64 counters, no metrics crate dep

The metrics types are 30+ `AtomicU64` fields rendered to the
Prometheus text-exposition format directly. We considered
`metrics-rs` and `prometheus`-the-crate but rejected both:

- The metric set is small (~30 counters + one histogram). The
  abstractions of a generic registry library are overkill.
- Both crates pull in macro-heavy registration machinery that
  hurts compile time on a project where startup time matters
  (the gateway should be ready in <1s).
- The text-exposition format is trivial to render directly —
  one `# HELP` + `# TYPE` + sample line per counter, plus the
  histogram's `_bucket{le="X"}` / `_sum` / `_count` triple.

The cost: render-time string concatenation runs on every
scrape. At Prometheus's default 15s interval and ~30 counters
this is irrelevant.

### 4. Cumulative-bucket histogram for review duration

`auto_review_review_duration_seconds` is a Prometheus histogram
with eight cumulative buckets at 1s / 5s / 15s / 30s / 60s /
120s / 300s / 600s plus `+Inf`. Bounds are tuned for review
work: sub-second on cached incremental PRs, long-tail to
minutes on big diffs with cloud LLMs.

The legacy `review_duration_ms_sum + reviews_completed_count`
pair stays exposed alongside for dashboards already wired to
it. SREs use the histogram for `histogram_quantile(0.95, ...)`;
the sum/count pair is fine for rolling-average dashboards.

### 5. Stable JSON shapes are part of the public API

`/info` returns JSON; operators script against it. The shape
is documented as stable in `GatewayInfo`'s docstring: adding a
field is fine; renaming or removing one is a breaking change
that needs a major version bump.

`/metrics` is the same — once a counter name ships, it can't
be renamed without breaking operators' Prometheus rules and
Grafana dashboards. Both contract tests
(`shipped_prometheus_rules_reference_only_real_metrics`,
`shipped_grafana_dashboard_only_references_real_metrics`)
enforce this from CI.

### 6. Defensive depth at the gateway boundary

The webhook handler does the following work, in order, on every
request:

1. Token-bucket rate-limit check (T7 mitigation; opt-in via
   `AR_WEBHOOK_RATE_PER_SEC` + `AR_WEBHOOK_BURST`). Runs
   *before* HMAC so a flood of unsigned junk can't burn CPU.
2. HMAC verification (T2 mitigation). Constant-time compare,
   reject 401.
3. Event-type bucketing into per-event metric counters.
4. Payload-decode (T2 secondary): bad JSON → 400, increments
   payload-failure counter (= Forgejo schema drift signal).

Each rejected request increments a distinct counter so
operators can distinguish active probing from secret drift
from version mismatch.

## Consequences

**Positive**
- Five independent HTTP endpoints map cleanly to operator and
  k8s semantics.
- Metrics shipped from CI via the deploy/prometheus + deploy/grafana
  artefacts; contract tests pin the metric→artefact link, so
  rename-and-forget can't ship.
- Threat-model T2/T3/T4/T7/T8/T9 mitigations are now
  CI-enforced (see `crates/ar-review/tests/red_team_pipeline.rs`
  and `red_team_workspace_tools.rs`).
- The orchestrator stays metrics-agnostic; future swaps
  (OpenTelemetry, statsd) only touch the gateway side.

**Negative**
- The metric set is hand-maintained. Adding a counter means
  editing both `Metrics` (struct field, recorder, render entry)
  and `OPERATIONS.md` daily-checks. The contract tests
  catch the dashboard/rules drift but not the docs drift.
- Render-time string concatenation on every scrape — small but
  not zero. Acceptable at Prometheus's default cadence.
- `AtomicU64` only — no labels (Prometheus calls these
  "dimensions"). Per-event-type webhook counters are 4 distinct
  fields rather than one labelled counter. This is fine for the
  current cardinality but doesn't scale to high-cardinality
  metrics (e.g. per-repo). When that becomes necessary, swap
  in a real metrics crate.

## Alternatives considered

- **OpenTelemetry**: real distributed-trace support would be
  valuable for understanding multi-stage review failures, but
  it's heavy infrastructure for a single-tenant bot. Defer
  until somebody needs it.
- **Status page route**: a single `/status` returning
  everything (health + readiness + version + info + summary
  metrics). Rejected because it conflates probe semantics —
  k8s liveness vs readiness, Prometheus vs operator-eyeballing
  — into one endpoint. The split surface is more flexible.
- **JSON-format metrics**: rejected because Prometheus is the
  default ops platform in 2026 and JSON-metrics readers are
  obscure. Operators outside the Prometheus ecosystem can
  scrape the text format trivially.

## Related documents

- [`docs/THREAT-MODEL.md`](THREAT-MODEL.md) — what the bot defends
  against; metrics naming aligns with threat IDs (e.g. T7 →
  `webhook_rate_limited_total`).
- [`docs/OPERATIONS.md`](OPERATIONS.md) — how operators use the
  endpoints day-to-day.
- [`deploy/prometheus/auto_review.rules.yaml`](../deploy/prometheus/auto_review.rules.yaml)
  — recording rules + alerting rules grounded in these
  counters.
- [`deploy/grafana/auto_review.dashboard.json`](../deploy/grafana/auto_review.dashboard.json)
  — operator dashboard.
