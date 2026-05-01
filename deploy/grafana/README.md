# Grafana dashboard for `auto_review`

[`auto_review.dashboard.json`](./auto_review.dashboard.json) is a
ready-to-import Grafana dashboard covering every counter and
recording rule that ships in this repo.

## Install

1. In Grafana: **Dashboards → New → Import**.
2. Upload `auto_review.dashboard.json` (or paste the JSON).
3. When prompted, select your Prometheus data source for the
   `DS_PROMETHEUS` variable.
4. Save.

The dashboard refreshes every 30s by default and shows the last
6 hours.

## Layout

| Row             | Panels |
|-----------------|--------|
| Pipeline funnel | Success rate (5m), p95 latency (5m), throughput, cumulative findings |
| Review outcomes | Stacked outcomes by class, percentile latency (p50/p95/p99) |
| Skipped reviews | Stacked skip-reason rates (no alerting; informational) |
| Webhook intake  | Events by type, signature/payload rejections |
| Chat surface    | Webhook vs poller intake, poller cycle health |

The pipeline funnel panels use the recording rules from
[`deploy/prometheus/auto_review.rules.yaml`](../prometheus/auto_review.rules.yaml).
Install both for best results — the dashboard works without the
recording rules but evaluates the equivalent expressions inline,
which is heavier on Prometheus.

## Customisation

The dashboard JSON is intentionally lightweight (no plugin deps,
no per-panel transforms, no Grafana 10-only features). Tweak in
the Grafana UI then **Save → Save as JSON file** to round-trip.

Tags: `auto_review`, `forgejo`, `code-review`.
