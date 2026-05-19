# Operations Runbook

Day-2 operations for `auto_review` after the [Quickstart](./QUICKSTART.md)
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
| Reviewer host high CPU / memory                              | Too many concurrent reviews / large workspaces          | §5.1    |
| Same PR reviewed in a loop                                   | Bot self-detection broken; check `AR_BOT_LOGIN`        | §4.1    |

---

## 0. Pre-deploy and post-deploy validation

Before exposing a freshly-deployed gateway to Forgejo:

```bash
# Confirms PAT validity, LLM reachability, git availability,
# secret strength, and runtime-isolation posture.
auto-review ops doctor

# Confirms the webhook intake path works end-to-end.
auto-review webhook test \
    --gateway-url https://reviewer.example.com \
    --webhook-secret "$WEBHOOK_SECRET"

# Live operational snapshot: /version + /info + /metrics.
auto-review ops status --gateway-url https://reviewer.example.com
```

These commands are fast and idempotent. `doctor` exits non-zero when any check
fails; `webhook test` exits non-zero when the gateway returns non-2xx.

## 0.1 Deployment posture

The recommended out-of-the-box Linux deployment is the signed `auto-review`
binary with embedded OCI isolation. On supported Linux hosts,
`auto-review gateway` defaults to the embedded OCI launcher. If embedded OCI
setup is unavailable, startup fails closed unless the operator explicitly opts
out with `auto-review gateway --bare` or `AR_GATEWAY_BARE=true`. That opt-out
leaves only application-level controls active and must not be treated as
container-equivalent isolation.

The project does not publish an official Docker/OCI image. Operators who want
Docker, Podman, Kubernetes, or Helm deployments may build and publish their own
image from the binary package.

Install details for binary, Nix/NixOS, systemd, custom container/Kubernetes/Helm,
Forgejo Actions, Prometheus, Grafana, and runner cache setup live in
[Deployment](./DEPLOYMENT.md).

## 0.2 Kubernetes probes

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

## 0.3 CI-triggered semantic reviews

The normal semantic-review trigger can be driven by Forgejo Actions after
your deterministic checks pass. Enable the gateway endpoint by setting a
strong `AR_CI_REVIEW_TOKEN` (generate it independently from
`WEBHOOK_SECRET`) and storing the same value as the Actions secret
`AR_CI_REVIEW_TOKEN`.

Projects choose their own prerequisites in workflow YAML. A review job should
depend on required checks and then use the project action wrapper, which calls
`POST /reviews/ci` without running linters or local review work in the runner:

```yaml
name: auto_review
on:
  pull_request:
    types: [opened, synchronize, reopened, ready_for_review]

jobs:
  fmt:
    runs-on: docker
    steps:
      - uses: https://code.forgejo.org/actions/checkout@v4
      - run: nix develop -c cargo fmt --all -- --check

  clippy:
    runs-on: docker
    steps:
      - uses: https://code.forgejo.org/actions/checkout@v4
      - run: nix develop -c cargo clippy --workspace --all-targets -- -D warnings

  test:
    runs-on: docker
    steps:
      - uses: https://code.forgejo.org/actions/checkout@v4
      - run: nix develop -c cargo nextest run --workspace --no-tests=pass

  semantic-review:
    runs-on: docker
    needs: [fmt, clippy, test]
    if: ${{ github.event_name == 'pull_request' }}
    steps:
      - uses: https://git.johnwilger.com/Slipstream/auto_review/deploy/forgejo-action@main
        with:
          gateway-url: https://reviewer.example.com
          action-token: ${{ secrets.AR_CI_REVIEW_TOKEN }}
          owner: ${{ github.repository_owner }}
          repo: ${{ github.event.repository.name }}
          pr-number: ${{ github.event.pull_request.number }}
          head-sha: ${{ github.event.pull_request.head.sha }}
```

Forgejo Actions intentionally exposes GitHub-compatible context and
environment names (`github.*`, `GITHUB_*`) for workflow compatibility; the
example above still runs on a Forgejo runner. Because Actions secrets are not
available to forked pull requests, this direct `pull_request` pattern is for
same-repository PRs. Do not switch to a privileged target-style workflow that
checks out or executes untrusted fork code with secrets.

The action is a thin gateway client. It fails before making a request when PR
context is missing, and `curl -f` makes gateway rejections fail the workflow
instead of publishing a release-blocking false success.

The gateway fetches the PR from Forgejo and rejects stale requests with
`409 Conflict` if the supplied `head_sha` no longer matches the PR head.
Duplicate reviews for the same SHA still rely on the orchestrator's review
history unless the request explicitly sets `"force": true`.

`pull_request` webhooks are still accepted for low-cost intake, deduplication,
chat support, logging, and future bookkeeping, but they do not queue semantic
reviews by default. That includes ordinary lifecycle events such as `opened` and
`synchronize` as well as `review_requested`; use the CI workflow to decide which
deterministic checks must pass before calling `POST /reviews/ci`.

Explicit chat commands are the intentional bypass. `@auto-review re-review`
queues a forced review at the current PR head and replies that it intentionally
bypasses CI gating, so authors can distinguish it from the normal
waiting-for-CI/action-triggered lifecycle.

## 0.4 Project release automation credentials

This section is for maintainers of `Slipstream/auto_review`, not for normal gateway
operators. Keep release credentials out of the gateway systemd environment and
out of deployment env files.

Configure the release preparation credential as Forgejo Actions secret `RELEASE_PREPARE_TOKEN`. Its release preparation PAT blast radius is to prepare release PR branches and release PRs only in `Slipstream/auto_review` for trusted `main` push runs.

Create a dedicated release bot Forgejo user for release PR commits. Add its
public SSH signing key to that account, store the private key as Forgejo Actions secret `RELEASE_SIGNING_KEY`, and set repository variables `RELEASE_BOT_NAME` and `RELEASE_BOT_EMAIL` to the bot identity attached to the signing key. Release publish also uses that SSH signing key to sign `SHA256SUMS`.

Configure the release publishing credential as Forgejo Actions secret `RELEASE_PUBLISH_TOKEN`, owned by the same release bot named in `RELEASE_BOT_NAME`. Its release publishing PAT blast radius is to publish Linux binary artifacts, checksums, signatures, SBOM/provenance metadata, and Forgejo Releases only in `Slipstream/auto_review`; it also covers managed PR body/description edit for binary artifact links.

The PR publishing credential model keeps `RELEASE_PUBLISH_TOKEN` out of
checkout/build steps and exposes it only after artifacts exist. CI verifies PR
release artifacts, uploads release-binary links to release PR descriptions, and
keeps token-bearing publish steps scoped to token-bearing forge operations.

Final release publication verifies the reviewed Linux x86_64 binary archive,
attaches the final binary assets, and includes verification commands in the
release notes:

```bash
sha256sum -c SHA256SUMS
ssh-keygen -Y verify -f allowed-signers -I <release-bot-email> -n file -s SHA256SUMS.sig < SHA256SUMS
```

## 1. Daily / weekly checks

If you run Prometheus, drop in [`deploy/prometheus/auto_review.rules.yaml`](../deploy/prometheus/auto_review.rules.yaml)
for pre-baked recording + alerting rules covering signature
failures, payload-decode failures, success rate, poller stall,
review latency p95, and per-class failure spikes. Installation and tuning notes
are in [Deployment](./DEPLOYMENT.md#prometheus-and-grafana).

If you run Grafana, import
[`deploy/grafana/auto_review.dashboard.json`](../deploy/grafana/auto_review.dashboard.json)
for a five-row dashboard covering the funnel, review outcomes,
skip paths, webhook intake, and chat surface.



**Scrape metrics** at `GET /metrics` from your Prometheus and dashboard:

*Webhook layer:*
- `auto_review_jobs_dispatched_total` — should track CI-triggered reviews and
  explicit forced reviews, not ordinary PR webhook opens.
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
- `auto_review_reviews_succeeded_total` and the six
  `auto_review_reviews_failed_<class>_total` counters
  (`forgejo`, `workspace`, `llm`, `unhealable`, `panic`, `unknown`). Track success
  rate as
  `succeeded / (succeeded + failed_forgejo + failed_workspace + failed_llm + failed_unhealable + failed_panic + failed_unknown)`.
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
- `auto_review_verifier_findings_dropped_total` — findings the
  cheap-tier verifier corrected away. Sustained drop ratio
  (`rate(...dropped[5m]) / (rate(...sum[5m]) + rate(...dropped[5m]))`)
  above ~30% means the reasoning model is hallucinating heavily.
  Action: try a higher-quality reasoning model, or tighten the
  system prompt's anti-hallucination guidance.
- `auto_review_review_queue_waits_total` — only meaningful when
  `AR_REVIEW_CONCURRENCY` is set. Counts dispatches that had to
  wait on the semaphore before starting. If this is climbing
  faster than ~10% of `reviews_started_total`, the cap is too
  tight (or your traffic exceeds the deployment's capacity).
  Action: raise the cap, or scale horizontally with multiple
  gateway instances against a shared SQLite history.
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
finding count at INFO; warnings during the context, LLM, self-heal, or verify
phases are non-fatal but worth scanning if findings drop noticeably.

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
   auto-review webhook test \
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
version, or accept missed webhook intake/chat/bookkeeping in this window. Normal
semantic review requests sent to `/reviews/ci` use a separate payload contract.

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

### 5.1 Runtime review pressure

`auto_review` no longer runs bundled linters during review jobs. CI owns
deterministic linters, tests, and builds; the gateway handles clone/context
preparation, semantic LLM review, verification, and posting. Host CPU/memory
pressure now usually means too many concurrent reviews, large workspaces, or
slow LLM calls rather than runaway linter execution.

Run the gateway inside your deployment isolation boundary (for example the
packaged binary's embedded OCI launcher, an operator-owned container image, a VM,
or a deliberately hardened service-manager sandbox).

The default Nix package installs the `auto-review` binary. Use `nix build .`
when constructing custom VM images, custom container images, or direct systemd
hosts. A NixOS module is available for direct-host deployments. See
[Deployment](./DEPLOYMENT.md#nix-and-nixos).

### 5.1.5 Cap concurrent in-flight reviews

Without `AR_REVIEW_CONCURRENCY` set, a burst of N PRs spawns N
tmpdirs + N in-flight LLM calls. On a small bot reviewing
~tens of PRs/day this is fine. On high-traffic instances (a
shared org reviewer with hundreds of PRs/day) or expensive
cloud LLMs, the unbounded burst can blow through cost limits
or exhaust workspace disk.

```
AR_REVIEW_CONCURRENCY=4
```

Gateway-triggered review requests still ack quickly after validation. Excess
spawned review tasks wait on the semaphore — they don't get dropped, just queued.
Pick a value matching your worker
capacity (CPU cores × something; rule of thumb: start at the
number of CPU cores, raise if LLM I/O dominates).

### 5.2 Long-running reviews

The orchestrator has no global per-PR timeout; each phase has its
own. If reviews start taking minutes, check:

1. LLM tier latency. `qwen2.5-coder:32b` on CPU can take 5-10× longer
   per token than on GPU.
2. Workspace clone size. Monorepos clone slowly. Consider
   `--depth=1` (already set) and shallow-fetch (set by the workspace
   builder).
3. CI latency. If semantic reviews are CI-triggered, slow deterministic jobs
   delay the trigger before the gateway receives work.

---

## 6. Rotation

### 6.1 Gateway bot PAT (`AR_FORGEJO_TOKEN`)

```bash
auto-review auth init \
    --forgejo-url $FORGEJO_BASE_URL \
    --username $AR_BOT_LOGIN \
    --token-name auto_review-$(date -I)
```

Save the new token, update `AR_FORGEJO_TOKEN` in the gateway env, restart, then revoke
the old token in Forgejo's user settings. Rotate at least every
180 days; sooner if you suspect compromise (cf. T5 in the
[threat model](./THREAT-MODEL.md)).

### 6.2 LLM API key (`LLM_API_KEY`)

Provider-specific. After rotation: update the env, restart the
gateway, run a smoke-test PR through `auto-review review once
--dry-run` to confirm prompt rendering succeeds, then a real
`auto-review review once` to confirm the new key works.

### 6.3 Webhook secret (`WEBHOOK_SECRET`)

Generate a new value (`openssl rand -hex 32`). Update both:
1. The gateway's env. Restart.
2. Every Forgejo webhook configured against this gateway. Audit
   them with:
   ```bash
   auto-review webhook list --owner <O> --repo <R>
   ```
   Then either patch each one in Forgejo's webhook UI, or remove
   the old hook and re-register cleanly:
   ```bash
   auto-review webhook unregister --owner <O> --repo <R> \
       --match-url reviewer.example.com
   auto-review webhook register --owner <O> --repo <R> \
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

The bot still accepts PR webhooks, but repository config is enforced when a
CI-triggered or explicit forced review runs; disabled repositories get a
"disabled by repo config" success status instead of review findings.

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
auto-review config validate .auto_review.yaml
```

### 7.1.4 Purge old review-history rows

Long-running deployments accumulate one row per PR ever
reviewed; closed PRs from months ago don't need their
`last_reviewed_sha` kept forever. Wire a periodic cleanup:

```bash
# Run weekly via systemd timer or cron
auto-review history purge --older-than-days 90
```

`--history-db` reads `AR_HISTORY_DB` by default. Use
`--dry-run` to see the row count before deleting (the
deletion semantics are: rows whose `updated_at` is strictly
older than the cutoff are dropped; the indexed query is
fast enough that scheduling weekly is fine for any sane
table size).

Safe to run while the gateway is up — SQLite handles
concurrent access. A row dropped here just means the next
review on that PR (if it ever happens) will be a fresh full
review instead of an incremental compare-diff, which is the
right behaviour for a stale row anyway.

### 7.1.5 Force a fresh full review on a specific PR

After a guideline / model change, or to recover from a botched
review, clear the orchestrator's "last reviewed SHA" record so
the next CI-triggered or explicit forced review runs as a full review (not an
incremental `compare` against a stale baseline):

```bash
auto-review history reset-pr \
    --history-db /var/lib/auto_review/review_history.db \
    --owner $OWNER --repo $REPO --pr $PR
```

`--history-db` reads `AR_HISTORY_DB` by default; if both the
gateway and the operator's shell share that env var, the flag
is optional. Safe to run while the gateway is up — SQLite
handles concurrent access. The next CI-triggered review or explicit
`@<bot> re-review` for that PR will see no recorded SHA and do a full review.

### 7.1.6 Attribute per-review LLM cost

When SQLite review history is enabled with `AR_HISTORY_DB`, each successful
review records the estimated LLM cost for that review in
`per_review_cost_usd`. The estimate uses built-in OpenAI-compatible defaults
unless `AR_PRICE_TABLE_PATH` points at a JSON override file:

```json
{
  "gpt-4o-mini": { "input": 0.15, "output": 0.60, "embedding": 0.0 },
  "https://api.openai.com/v1|gpt-4o-mini": {
    "input": 0.15,
    "output": 0.60,
    "embedding": 0.0
  }
}
```

Prices are USD per million tokens. Provider-qualified keys
(`<base-url>|<model>`) override model-only keys when the same model name is
served by multiple providers.

By default, the bot also appends an `LLM usage and cost` footer to posted
reviews when token usage is available and the price table has matching entries
for the models used. Set `AR_REVIEW_COST_FOOTER=false` to keep recording
`per_review_cost_usd` without posting that footer.

### 7.2.5 Tune signal-to-noise via `AR_SEVERITY_FLOOR`

Default is `warning`: every Note-severity finding is dropped
before posting. Notes function as the LLM's reasoning
scratchpad — externalising observations about the diff makes
the review pass more thorough — but they're pure noise on the
PR page once the verifier has finished (e.g. "💡 Note:
switching from `find()` to `match_indices()` ensures all
occurrences are checked"). The bot still generates the dropped
findings (so the metric counters and duration histogram aren't
distorted), but the floor runs **before the verifier**, so
cheap-tier LLM calls are saved on every dropped finding.

```
AR_SEVERITY_FLOOR=note     # opt back in to posting notes
AR_SEVERITY_FLOOR=warning  # default: drop notes, keep warnings + errors
AR_SEVERITY_FLOOR=error    # only post Error-severity findings
```

The bot validates the value at startup; an unrecognised
spelling falls through to the default (`warning`) with a warn
log so a typo doesn't silently invert the operator's signal-
to-noise expectation.

## 8. Learnings store

By default, learnings persist in the gateway's SQLite state path. Set
`AR_LEARNINGS_DB` to a file path to choose the location explicitly, or to
`:memory:` for volatile local evaluation.

**Backup:**
```bash
sqlite3 /var/lib/auto_review/learnings.db ".backup '/backup/learnings-$(date -I).db'"
```

**Inspect:**
```bash
auto-review learnings list   # uses AR_LEARNINGS_DB by default
auto-review learnings list --json | jq    # machine-readable
```

(For a custom inspection query, `sqlite3` against the file
works too — the schema is documented in
`crates/ar-index/src/sqlite_learnings.rs`.)

**Forget a single learning:**
```bash
auto-review learnings forget --id <ID>
```
Same effect as `@<bot> forget` from a PR thread but operates
directly on the SQLite store, so operators can script bulk
wipes without going through Forgejo.

**Restore:** stop the gateway, replace the file, restart.

**Wipe everything:** delete the file, restart.

---

## 9. Upgrade

Semver: 1.x minor releases should preserve documented configuration and CLI
contracts. Breaking operator-facing changes require a major release or an
explicit migration note. Always read the [CHANGELOG](../CHANGELOG.md) before
bumping.

```bash
# Build the new version with the pinned Nix toolchain
git -C /opt/auto_review pull
nix build /opt/auto_review -o /tmp/auto-review-result
sudo install -m 0755 /tmp/auto-review-result/bin/auto-review /usr/local/bin/auto-review

# Restart
sudo systemctl restart auto_review.service

# Smoke-test
curl -s http://localhost:8080/version | jq
auto-review ops doctor
```

The systemd unit ships under
`deploy/systemd/`; the install walkthrough lives in
[Deployment](./DEPLOYMENT.md#systemd-direct-host-service).

If the new version fails to start, restore the previous binary from your release
artifact or host backup, restart, and file an issue.

## 9.1 Forgejo review-comment resolution gap

Forgejo 15.0.0 does not provide a token-authenticated REST API for
resolving inline review conversations. Gitea documents
`POST /repos/{owner}/{repo}/pulls/comments/{id}/resolve`, but on
Forgejo that route currently returns `405 Method Not Allowed` with
`Allow: GET`. The working resolver is Forgejo's web form endpoint,
`/{owner}/{repo}/issues/resolve_conversation`, which is protected by
CSRF and requires a browser session cookie.

Operationally, keep `auto_review` on PAT-based API auth only and treat
conversation resolution as a manual reviewer action in the Forgejo UI.
Do not plan automations that auto-resolve comments after a fix unless a
future Forgejo release exposes an API endpoint that works with the bot's
token. Re-check this during Forgejo upgrades by probing the REST route
against a test PR before depending on it.

---

## 10. Filing an issue

Before filing, capture:
- `GET /version` JSON
- `GET /info` JSON (runtime configuration: persistence backing,
  LLM tiers, poller status)
- `GET /metrics` snapshot
- `journalctl -u auto-review --since "1h ago" --no-pager > logs.txt`
- The exact commit-status `description` text from the failing PR
- Sanitised `.auto_review.yaml` (strip any sensitive `guidelines`
  text)
- Forgejo version (`GET /api/v1/version`)

Attach those to the issue. Do **not** include `AR_FORGEJO_TOKEN`,
`LLM_API_KEY`, or `WEBHOOK_SECRET` in any field.
