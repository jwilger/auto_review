# Operations Runbook

Day-2 operations for `auto_review` after the [QUICKSTART](../QUICKSTART.md)
walks you through the first deploy. Audience: the on-call engineer or
operator running the bot.

For *what* the bot defends against, see
[THREAT-MODEL.md](./THREAT-MODEL.md). This document covers *how* you
keep it healthy.

---

## Quick reference

| Symptom                                                      | First action                                           | Section |
|--------------------------------------------------------------|--------------------------------------------------------|---------|
| `webhook_signature_failures_total` increasing                | Suspect secret drift or active probing                 | §2.1    |
| `webhook_payload_failures_total` increasing                  | Forgejo upgraded; payload schema may have shifted      | §2.2    |
| Reviews failing with `LLM` errors                            | Provider down or model unloaded                        | §3.1    |
| Reviews failing with `Workspace` errors                      | Disk full / network egress blocked / token revoked     | §3.2    |
| Bot replies to itself in a chat thread                       | `AR_BOT_LOGIN` mismatch                                | §4.1    |
| Bot ignores `@<name>` mentions                               | `AR_BOT_NAME` mismatch                                 | §4.1    |
| Reviewer host high CPU / memory                              | Sandbox limits not set; runaway linter                 | §5.1    |
| Same PR reviewed in a loop                                   | Bot self-detection broken; check `AR_BOT_LOGIN`        | §4.1    |

---

## 0a. Kubernetes probes

If you deploy via the bundled Helm chart, both probes are
already wired:

| Probe       | Path       | Semantics                                                |
|-------------|------------|----------------------------------------------------------|
| liveness    | `/healthz` | Process is up. Restart the pod when it fails.            |
| readiness   | `/readyz`  | Forgejo is reachable. Removes pod from service rotation. |

`/readyz` caches its check (default 10s) so probes don't hammer
Forgejo. If Forgejo is briefly unreachable, the pod is taken out of
the service mesh's rotation but **not** restarted — that's the
correct k8s semantics for transient downstream failures.

Tune the cache with `AR_READINESS_TTL_SECS` (set to 0 effectively
disables caching, but typical values are 10-30s).

## 0. Pre-deploy and post-deploy validation

Before exposing a freshly-deployed gateway to Forgejo:

```bash
# Confirms PAT validity, LLM reachability, and secret entropy
# all in one shot. Reads env vars so a configured deploy needs
# no args.
auto_review doctor

# Confirms the webhook intake path works end-to-end.
auto_review test-webhook \
    --gateway-url https://reviewer.example.com \
    --webhook-secret "$WEBHOOK_SECRET"

# Live operational snapshot: runtime config + key counters +
# success rate. Run from anywhere with HTTP access to the
# gateway; complements `doctor` (deps) and `test-webhook`
# (intake) with the live-state view.
auto_review status --gateway-url https://reviewer.example.com
```

Both commands are fast and idempotent — drop them into your
deploy script and your "did the upgrade break anything?"
runbook. `doctor` exits non-zero when any check fails;
`test-webhook` exits non-zero when the gateway returns non-2xx.

## 1. Daily / weekly checks

If you run Prometheus, drop in [`deploy/prometheus/auto_review.rules.yaml`](../deploy/prometheus/auto_review.rules.yaml)
for pre-baked recording + alerting rules covering signature
failures, payload-decode failures, success rate, poller stall,
review latency p95, and per-class failure spikes. See
[`deploy/prometheus/README.md`](../deploy/prometheus/README.md)
for tuning notes and Alertmanager routing.

If you run Grafana, import
[`deploy/grafana/auto_review.dashboard.json`](../deploy/grafana/auto_review.dashboard.json)
for a five-row dashboard covering the funnel, review outcomes,
skip paths, webhook intake, and chat surface. See
[`deploy/grafana/README.md`](../deploy/grafana/README.md) for
import steps.



**Scrape metrics** at `GET /metrics` from your Prometheus and dashboard:

*Webhook layer:*
- `auto_review_jobs_dispatched_total` — should track PR opens.
- `auto_review_webhook_signature_failures_total` — should be zero or
  near-zero.
- `auto_review_webhook_payload_failures_total` — should be zero.
- `auto_review_chat_commands_received_total` — non-zero only if your
  team uses `@<bot> remember/forget/re-review/...`.

*Review pipeline:*
- `auto_review_reviews_started_total` — fired once each review
  begins post-dedup. Compare against `jobs_dispatched_total`; the
  gap is reviews short-circuited by the `same_sha`/`trivial`/
  `disabled` skip paths (see `*_skipped_*_total` counters below).
- `auto_review_reviews_succeeded_total` and the four
  `auto_review_reviews_failed_<class>_total` counters
  (`forgejo`, `workspace`, `llm`, `unhealable`). Track success
  rate as
  `succeeded / (succeeded + failed_forgejo + failed_workspace + failed_llm + failed_unhealable)`.
  A spike in a single class points at one subsystem.
- `auto_review_review_duration_seconds` is a proper Prometheus
  histogram with buckets at 1s, 5s, 15s, 30s, 60s, 120s, 300s,
  600s, and `+Inf`. Use
  `histogram_quantile(0.95,
   sum(rate(auto_review_review_duration_seconds_bucket[5m])) by (le))`
  for p95. The legacy `auto_review_review_duration_ms_sum` /
  `auto_review_reviews_completed_count` pair stays exposed for
  rolling-average dashboards
  (`rate(...sum[5m]) / rate(...count[5m])`).
- `auto_review_review_findings_sum` — total findings posted across
  successful reviews. Useful for charting bot output volume.
- `auto_review_reviews_skipped_<reason>_total` — `same_sha`
  (incremental dedup), `trivial_files` (lockfiles / vendored /
  generated), `disabled_by_config` (`enabled: false`). Operators
  shouldn't alert on these.

*Background poller:*
- `auto_review_poll_cycles_total` — should tick at
  `1 / AR_POLL_INTERVAL_SECS` per second when the poller is
  running. A flat counter means the poller is wedged or
  stopped; alert on `rate(poll_cycles_total[5m]) == 0`.
- `auto_review_poll_history_failures_total` and
  `auto_review_poll_pr_failures_total` — Forgejo-side or
  storage-side errors during polling. Spikes track Forgejo
  outages.
- `auto_review_poll_mentions_dispatched_total` — inline-thread
  mentions the poller picked up. Disjoint from
  `chat_commands_received_total` (webhook path).

**Tail logs** for anomalies:
```
journalctl -u auto-review -f --since "1 hour ago" | grep -E 'WARN|ERROR'
```
The orchestrator logs each review's repo, PR number, and final
finding count at INFO; warnings during the lint/RAG/verify phases are
non-fatal but worth scanning if findings drop noticeably.

---

## 2. Webhook anomalies

### 2.1 Signature failures

`auto_review_webhook_signature_failures_total` is the alerting signal
for either secret drift or active probing.

**Diagnosis:**
1. Compare the `WEBHOOK_SECRET` env var in your gateway against the
   value Forgejo has stored for the webhook
   (`GET /api/v1/repos/{owner}/{repo}/hooks` as admin).
2. Check `journalctl -u auto-review` for the `signature: ...`
   reject lines — `signature: mismatch` means wrong secret;
   `signature: not hex` means malformed sender; `signature: missing`
   means the header isn't being sent (proxy stripping it?).
3. Smoke-test the intake path with a fresh signature:
   ```bash
   auto_review test-webhook \
       --gateway-url https://reviewer.example.com \
       --webhook-secret "$WEBHOOK_SECRET"
   ```
   A 200 confirms the secret + headers + path work end-to-end.
   A 401 with the gateway's WEBHOOK_SECRET still proves the path
   itself is healthy and points at Forgejo's stored secret.

**Fix:** rotate (see §6.1).

### 2.2 Payload-decode failures

A spike here usually means Forgejo was upgraded and the JSON shape
shifted. We pin against the Gitea/Forgejo API contract; new fields
are tolerated, but renamed-or-removed fields break parsing.

**Diagnosis:** find a failing example in logs and compare against
[`crates/ar-forgejo/src/types.rs`](../crates/ar-forgejo/src/types.rs).
File an issue with the failing payload (redact the `secret` field
from the webhook envelope first).

**Workaround until fix:** revert Forgejo to the last working
version, or accept the missed reviews on PRs that webhook in this
window.

---

## 3. Review failures

The dispatcher posts a final commit-status to every PR. `error`
means transport / config; `failure` means the LLM produced
something the schema validator + self-heal couldn't repair.

### 3.1 LLM errors

`auto_review failed: llm: ...` on the commit-status.

**Cloud profile:** check the provider dashboard for 4xx (auth /
quota) or 5xx (provider outage). `LLM_API_KEY` rotation: see §6.2.

**Local profile (Ollama / vLLM):**
1. `curl -s ${LLM_BASE_URL}/v1/models | jq '.data[].id'` —
   confirm the configured `LLM_REASONING_MODEL` is loaded.
2. If absent, `ollama pull <model>` or restart the inference server.
3. Watch `journalctl -u auto-review` after a fresh PR; the next
   review should succeed.

### 3.2 Workspace errors

`auto_review failed: workspace: clone failed: ...`

**Common causes:**
- The bot's PAT was revoked or expired → re-mint (§6.1).
- Disk pressure on the workspace tmpfs → bump the volume size or
  reduce `AR_WORKSPACE_MAX_BYTES` if set.
- Network egress to the Forgejo instance blocked from the gateway.

---

## 4. Bot identity

### 4.1 `AR_BOT_LOGIN` and `AR_BOT_NAME`

Two separate things:
- `AR_BOT_LOGIN` — the Forgejo username the bot authenticates as.
  Used for self-loop detection (don't act on the bot's own
  comments).
- `AR_BOT_NAME` — the mention handle (`@<name>`). Often the same
  as `AR_BOT_LOGIN`.

**Symptom: bot ignores all `@<name>` mentions.** `AR_BOT_NAME`
doesn't match what users are typing. Update the env var; restart.

**Symptom: bot replies to itself, looping.** `AR_BOT_LOGIN` doesn't
match the actual bot account. The webhook receives the bot's own
`issue_comment`, doesn't recognise the sender as itself, and acts.
Update the env var; restart. (As of v0.1.0, both the webhook
handler and the background poller honour these.)

---

## 5. Resource pressure

### 5.1 Sandbox limits

Without `AR_SANDBOX_IMAGE` set, linter binaries spawn directly on
the host. That's only safe for trusted-PR-source environments.
Production deployments should set:

```
AR_SANDBOX_IMAGE=ghcr.io/your-org/auto_review-sandbox:<tag>
AR_SANDBOX_MEMORY_MIB=512
AR_SANDBOX_CPUS=1.0
AR_SANDBOX_PIDS_LIMIT=128
AR_SANDBOX_TIMEOUT_SECS=60
```

Tune the limits per-linter empirically — `golangci-lint` on a large
Go monorepo will need more memory and a longer timeout than
`shellcheck`.

### 5.2 Long-running reviews

The orchestrator has no global per-PR timeout; each phase has its
own. If reviews start taking minutes, check:

1. LLM tier latency. `qwen2.5-coder:32b` on CPU can take 5-10× longer
   per token than on GPU.
2. Workspace clone size. Monorepos clone slowly. Consider
   `--depth=1` (already set) and shallow-fetch (set by the workspace
   builder).
3. Linter wall-clock. Increase `AR_SANDBOX_TIMEOUT_SECS` only if you
   have a known-slow linter; raising it for everything masks
   genuine fork-bomb attacks.

---

## 6. Rotation

### 6.1 Bot PAT (`FORGEJO_TOKEN`)

```bash
auto_review init \
    --forgejo-url $FORGEJO_BASE_URL \
    --username $AR_BOT_LOGIN \
    --token-name auto_review-$(date -I)
```

Save the new token, update the gateway env, restart, then revoke
the old token in Forgejo's user settings. Rotate at least every
180 days; sooner if you suspect compromise (cf. T5 in the
[threat model](./THREAT-MODEL.md)).

### 6.2 LLM API key (`LLM_API_KEY`)

Provider-specific. After rotation: update the env, restart the
gateway, run a smoke-test PR through `auto_review review-once
--dry-run` to confirm prompt rendering succeeds, then a real
`review-once` to confirm the new key works.

### 6.3 Webhook secret (`WEBHOOK_SECRET`)

Generate a new value (`openssl rand -hex 32`). Update both:
1. The gateway's env. Restart.
2. Every Forgejo webhook configured against this gateway. Audit
   them with:
   ```bash
   auto_review list-webhooks --owner <O> --repo <R>
   ```
   Then either patch each one in Forgejo's webhook UI, or remove
   the old hook and re-register cleanly:
   ```bash
   auto_review unregister-webhook --owner <O> --repo <R> \
       --match-url reviewer.example.com
   auto_review register-webhook --owner <O> --repo <R> \
       --gateway-url https://reviewer.example.com \
       --webhook-secret "$WEBHOOK_SECRET"
   ```

There's a brief window where webhooks signed with the old secret
will get rejected. Plan accordingly — schedule for a low-PR-traffic
window.

---

## 7. Repo-level operations

### 7.1 Disable the bot for one repo

Add to the repo root:
```yaml
# .auto_review.yaml
enabled: false
```

The bot still receives the webhook but skips the review and posts a
"disabled by repo config" success status.

### 7.2 Add custom guidelines

```yaml
# .auto_review.yaml
guidelines: |
  We never use raw SQL — every query goes through the QueryBuilder.
  Prefer immutable types; mutating helpers must be marked with
  `// MUTATES` for the reviewer.
```

These get injected into the LLM prompt's "repository conventions"
section. Validate locally:
```bash
auto_review validate-config .auto_review.yaml
```

### 7.1.5 Force a fresh full review on a specific PR

After a guideline / model change, or to recover from a botched
review, clear the orchestrator's "last reviewed SHA" record so
the next webhook triggers a full review (not an incremental
`compare` against a stale baseline):

```bash
auto_review reset-pr \
    --history-db /var/lib/auto_review/review_history.db \
    --owner $OWNER --repo $REPO --pr $PR
```

`--history-db` reads `AR_HISTORY_DB` by default; if both the
gateway and the operator's shell share that env var, the flag
is optional. Safe to run while the gateway is up — SQLite
handles concurrent access. The next webhook for that PR (a
push, an `@<bot> re-review`, etc.) will see no recorded SHA and
do a full review.

### 7.2.5 Tune signal-to-noise via `AR_SEVERITY_FLOOR`

Set the gateway env var to `warning` to suppress every Note-
severity finding before posting; `error` to only post
high-confidence problems. The bot still generates the dropped
findings (so the metric counters and duration histogram aren't
distorted), but the floor runs **before the verifier**, so
cheap-tier LLM calls are saved on every dropped finding.
Operators on busy monorepos use this both to keep PR pages
from drowning in stylistic nits AND to lower per-review token
spend.

```
AR_SEVERITY_FLOOR=warning   # most common: drop notes, keep warnings + errors
```

The bot validates this at startup; an unrecognised value
falls through to `note` (post everything) with a warning log,
so a typo doesn't accidentally suppress real findings.

### 7.3 Disable a noisy linter

```yaml
# .auto_review.yaml
disabled_tools:
  - markdownlint
  - phpstan
```

Names match `LinterRunner::name()`. Enumerate them with:

```bash
auto_review list-linters                  # full table
auto_review list-linters --language python # filter by language tag
auto_review list-linters --json | jq      # machine-readable
```

---

## 8. Learnings store

When `AR_LEARNINGS_DB` is set to a file path, learnings persist across
restarts.

**Backup:**
```bash
sqlite3 /var/lib/auto_review/learnings.db ".backup '/backup/learnings-$(date -I).db'"
```

**Inspect:**
```bash
auto_review list-learnings   # uses AR_LEARNINGS_DB by default
auto_review list-learnings --json | jq    # machine-readable
```

(For a custom inspection query, `sqlite3` against the file
works too — the schema is documented in
`crates/ar-index/src/sqlite_learnings.rs`.)

**Forget a single learning:**
```bash
auto_review forget-learning --id <ID>
```
Same effect as `@<bot> forget` from a PR thread but operates
directly on the SQLite store, so operators can script bulk
wipes without going through Forgejo.

**Restore:** stop the gateway, replace the file, restart.

**Wipe everything:** delete the file, restart.

---

## 9. Upgrade

Semver: pre-1.0, minor versions can break configuration. Always
read the [CHANGELOG](../CHANGELOG.md) before bumping.

```bash
# Build the new version
git -C /opt/auto_review pull
cargo build --release -p ar-gateway -p ar-cli

# Validate config still parses (some keys may have moved)
auto_review validate-config /etc/auto_review/

# Restart
sudo systemctl restart auto_review.service

# Smoke-test
curl -s http://localhost:8080/version | jq
auto_review doctor
```

The systemd unit ships under
[`deploy/systemd/`](../deploy/systemd/) — if you're on systemd and
haven't installed it, follow that README first.

If the new version fails to start, the old binary is still on disk
at `target/release/ar-gateway.bak` (manual; we do not auto-back-up).
Roll back, file an issue.

---

## 10. Filing an issue

Before filing, capture:
- `GET /version` JSON
- `GET /info` JSON (runtime configuration: sandbox kind,
  learnings store kind, LLM tiers, poller status)
- `GET /metrics` snapshot
- `journalctl -u auto-review --since "1h ago" --no-pager > logs.txt`
- The exact commit-status `description` text from the failing PR
- Sanitised `.auto_review.yaml` (strip any sensitive `guidelines`
  text)
- Forgejo version (`GET /api/v1/version`)

Attach those to the issue. Do **not** include `FORGEJO_TOKEN`,
`LLM_API_KEY`, or `WEBHOOK_SECRET` in any field.
