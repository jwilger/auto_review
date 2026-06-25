# CLI reference

`auto-review` is the single public command operators install. It includes the
long-running gateway entrypoint, AgentCore runtime entrypoint, and grouped
subcommands for bootstrap, operations, one-shot debugging, benchmarks, and
maintenance.

## Top-level groups

| Command | Purpose |
|---|---|
| `gateway` | Starts the long-running gateway service. |
| `agentcore` | Starts the AWS Bedrock AgentCore-compatible runtime service. |
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

## AgentCore Runtime

`auto-review agentcore serve` starts the AgentCore-compatible HTTP surface. It
binds to `0.0.0.0:9000` by default, accepts `--bind HOST:PORT`, and currently
exposes `/ping` for runtime health checks plus `/invocations` for
provider-neutral invocation payloads. Semantic-review invocations fetch the
current PR metadata, reject stale `head_sha` values, and run the review inline
before returning a structured outcome.
Chat-command invocations set `kind` to `chat_command` and include
`comment_body`; the runtime parses the same `@auto-review` commands used by the
gateway chat path and posts the reply through the selected repository host.

Configure exactly one repository provider for semantic-review invocations.
Forgejo mode uses the same runtime inputs as the gateway:
`--forgejo-url`/`FORGEJO_BASE_URL`, `--token`/`AR_FORGEJO_TOKEN`,
`--llm-base-url`/`LLM_BASE_URL`, optional `--llm-api-key`/`LLM_API_KEY`, and
`--llm-model`/`LLM_REASONING_MODEL`.

GitHub mode uses GitHub App credentials: `--github-api-url`/`GITHUB_API_URL`
(default `https://api.github.com`), `--github-app-id`/`GITHUB_APP_ID`,
`--github-app-private-key`/`GITHUB_APP_PRIVATE_KEY`, `--llm-base-url`/
`LLM_BASE_URL`, optional `--llm-api-key`/`LLM_API_KEY`, and `--llm-model`/
`LLM_REASONING_MODEL`. The private key may be provided as a PEM string or with
literal `\n` escapes in environment variables. GitHub semantic-review
invocations, including chat commands, must include `installation_id`; the
runtime mints a scoped installation token for the requested repository and uses
the GitHub `ReviewHost` adapter for PR reads, clone credentials, comments,
reviews, and statuses.

Invocation idempotency defaults to an in-memory store. For cold-start-safe
AgentCore deployments, set `--idempotency-dynamodb-table` or
`AGENTCORE_IDEMPOTENCY_DYNAMODB_TABLE`; the runtime loads normal AWS SDK
configuration and stores duplicate-suppression records in that table. Records
use an `expires_at` epoch-seconds TTL attribute, with
`--idempotency-ttl-secs`/`AGENTCORE_IDEMPOTENCY_TTL_SECS` defaulting to 86400
seconds.

Review history also defaults to the orchestrator's in-memory history. Set
`--history-dynamodb-table` or `AGENTCORE_HISTORY_DYNAMODB_TABLE` to persist
last-reviewed SHAs in DynamoDB so incremental review state survives AgentCore
cold starts.

Learnings default to no shared store in the AgentCore semantic-review path. Set
`--learnings-dynamodb-table` or `AGENTCORE_LEARNINGS_DYNAMODB_TABLE` to load and
store remembered guidance in DynamoDB for review context across cold starts.
