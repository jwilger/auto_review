# ar-gateway

HTTP intake for `auto_review`. Accepts Forgejo webhooks, verifies
HMAC, dispatches review jobs, and exposes operational endpoints.

## Public surface

| Module | What's in it |
|--------|-------------|
| `lib::AppState`, `GatewayInfo` | Shared state passed to every route handler. Builder methods (`with_chat`, `with_metrics`, `with_readiness`, `with_info`, `with_webhook_rate_limit`, `with_webhook_dedup`, `with_bot_identity`) pin runtime config. |
| `webhook` | `POST /webhooks/forgejo` handler. HMAC verify → throttle → dedup → event dispatch. |
| `hmac` | Constant-time signature verification (T2 mitigation). |
| `metrics` | `Metrics` struct holding 30+ `AtomicU64` counters; `MetricsObserver` bridges the orchestrator's `ReviewObserver` trait to `/metrics`. |
| `ratelimit` | `TokenBucket` for the optional webhook throttle (T7 mitigation). |
| `dedup` | `RecentDeliveries` LRU on `X-Forgejo-Delivery` IDs to absorb Forgejo retries. |
| `poller` | `ChatPoller` background loop covering Forgejo's missing inline-review-thread webhook (gitea#26023). |

## HTTP routes

- `GET /healthz` — process liveness
- `GET /readyz` — Forgejo reachability with TTL cache
- `GET /version` — `{name, version}`
- `GET /info` — runtime-config snapshot
- `GET /metrics` — Prometheus text-format counters
- `POST /reviews/ci` — optional CI-triggered review dispatch. Enable with
  `AR_CI_REVIEW_TOKEN` generated independently from `WEBHOOK_SECRET` with at
  least 32 random bytes/chars; callers authenticate with `Authorization: Bearer
  ...` and provide owner, repo, PR number, head SHA, and trigger metadata. The
  gateway verifies the current Forgejo PR head still matches before dispatch.
- `POST /webhooks/forgejo` — webhook intake

See [`docs/ADR-0003-observability.md`](../../docs/ADR-0003-observability.md)
for the design rationale.

## Tests

Webhook intake is fully covered: HMAC verify, dedup retry, rate-limit
429, payload decode, review-request dispatch, chat-command dispatch. Run with
`cargo test -p ar-gateway`. See `webhook.rs` and the per-module
`tests` blocks (`metrics.rs`, `ratelimit.rs`, `dedup.rs`,
`poller.rs`).

## Dependencies

`axum` for the HTTP server, `hmac` + `sha2` for signature verify,
`reqwest` for the readiness probe, `tower` middleware. No metrics
crate — counters are hand-rolled.
