# CLI reference

`auto-review` is the single public command operators install. It includes the
long-running gateway entrypoint and grouped subcommands for bootstrap,
operations, one-shot debugging, benchmarks, and maintenance.

## Top-level groups

| Command | Purpose |
|---|---|
| `gateway` | Starts the long-running gateway service. |
| `auth` | Bot user authentication and token bootstrap. |
| `webhook` | Forgejo webhook registration, auditing, deletion, and smoke tests. |
| `config` | Repository `.auto_review.yaml` validation. |
| `review` | One-off review execution. |
| `bench` | Fixture benchmark commands. |
| `ops` | Operational diagnostics and status. |
| `history` | Review-history maintenance. |
| `learnings` | Learning-store audit and deletion. |

## Bootstrap commands

| Command | Purpose |
|---|---|
| `auth init` | Mints the bot user's PAT via Basic auth and prints the one-time secret. |
| `webhook register` | Registers `pull_request` and `issue_comment` webhooks for one repo. |
| `webhook list` | Lists webhooks installed on one repo. |
| `webhook unregister` | Deletes a webhook by id or URL substring. |

## Deployment validation

| Command | Purpose |
|---|---|
| `ops doctor` | Probes Forgejo PAT validity, LLM reachability/model availability, git availability, secret strength, and runtime isolation posture. |
| `webhook test` | Sends an HMAC-signed test event to a running gateway. |
| `ops status` | Reads `/version`, `/info`, and `/metrics` for a one-screen live snapshot. |

Typical post-deploy triad:

```sh
auto-review ops doctor
auto-review webhook test --gateway-url https://reviewer.example.com --webhook-secret "$WEBHOOK_SECRET"
auto-review ops status --gateway-url https://reviewer.example.com
```

## Operations and maintenance

| Command | Purpose |
|---|---|
| `config validate` | Parses `.auto_review.yaml`; add `--strict` to reject unknown top-level keys. |
| `learnings list` | Lists persistent learnings, optionally as JSON. |
| `learnings forget` | Deletes one learning by id. |
| `history reset-pr` | Clears the last-reviewed SHA for one PR so the next review is fresh. |
| `history purge` | Deletes review-history rows older than N days. |

## Debugging and benchmarks

| Command | Purpose |
|---|---|
| `review once` | Runs a one-shot reasoning-path review against one PR without the gateway. It does not wire every gateway runtime store or optional LLM tier. `--dry-run` prints the base rendered LLM prompt and exits without clone/RAG/repo-config loading/posting. |
| `bench run` | Replays PR fixtures through the LLM review path. `--baseline FILE` compares against an earlier JSON run; `--fail-on-regression` exits non-zero on configured precision/recall/latency regressions. |

Benchmark fixture details live in [Benchmarks](./BENCHMARKS.md).

## Gateway isolation flags

`auto-review gateway` uses the packaged embedded OCI launcher by default on
supported direct-binary Linux installs. Use `--bare` or `AR_GATEWAY_BARE=true`
only when you intentionally run without that launcher, for example under an
operator-owned container, VM, or direct systemd boundary. Operator-owned images
can set `AR_GATEWAY_EXTERNAL_ISOLATION=container` to surface that external
runtime posture.
