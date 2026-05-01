# Prometheus rules for `auto_review`

Drop-in rules for Prometheus operators. The rules file
[`auto_review.rules.yaml`](./auto_review.rules.yaml) contains:

- **Recording rules** — pre-computed ratios (success rate, p95
  latency, combined chat command rate) so dashboards don't have
  to re-derive them on every refresh.
- **Alerting rules** — six conservative defaults covering the
  signals operators most need to know about: signature failures,
  payload-decode failures, success rate below SLO, poller stalled,
  review latency, and per-class failure spikes (Forgejo / LLM).

## Install

```yaml
# /etc/prometheus/prometheus.yml
rule_files:
  - /etc/prometheus/auto_review.rules.yaml

scrape_configs:
  - job_name: auto_review
    metrics_path: /metrics
    static_configs:
      - targets:
          - reviewer.example.com:8080
```

Reload Prometheus (`SIGHUP` or `/-/reload`) after copying the rules
file. The web UI's **Status → Rules** page will list each rule and
its current evaluation state.

## Tuning

The defaults are tuned for a deployment seeing a few PRs an hour.
Two knobs you'll likely want to adjust:

- **`for:` durations.** A busy site can shorten the 10m / 15m
  windows; a quieter one should keep or lengthen them so a
  one-off bad request doesn't page.
- **Threshold rates.** `0.05/s = 3/min` and `0.1/s = 6/min` are
  the two thresholds in the file. If your normal request volume
  is far below those, even a single failure could fire — drop
  them. If volume is far above, raise them.

Recording rules don't need tuning; they just expose Prometheus
queries you'd write anyway.

## Alertmanager routing

These rules carry `service: auto_review` and `severity:
warning|critical` labels. Route in your `alertmanager.yml` like:

```yaml
route:
  routes:
    - match:
        service: auto_review
        severity: critical
      receiver: oncall-paging
    - match:
        service: auto_review
        severity: warning
      receiver: oncall-slack
```

## See also

- [`/metrics`](../../docs/OPERATIONS.md#1-daily--weekly-checks) —
  the raw counters these rules build on.
- [OPERATIONS.md](../../docs/OPERATIONS.md) — operator runbook with
  a symptom-to-diagnosis quick-reference table that mirrors these
  alerts.
