# Quickstart

Get `auto_review` reviewing PRs on a Forgejo instance you control.

## Prerequisites

- A Forgejo instance you administer.
- A dedicated Forgejo bot user with access to each repo it reviews.
- An OpenAI-compatible LLM endpoint:
  - local: Ollama or vLLM, for example `qwen2.5-coder:32b`;
  - cloud: OpenAI, OpenRouter, Together, Groq, or another compatible API.
- Nix with flakes enabled if you build from source.
- A CI workflow that runs deterministic linters/tests/builds before asking
  `auto_review` for semantic review.

## 1. Install or build `auto-review`

Use a release asset when possible:

1. Download the latest Linux `x86_64` `auto-review` archive from the Forgejo
   release page.
2. Verify `SHA256SUMS` and its signature using the commands in the release notes.
3. Install the binary somewhere on `PATH`, for example `/usr/local/bin`.

Or build from source with the pinned Nix toolchain:

```sh
git clone https://git.johnwilger.com/jwilger/auto_review
cd auto_review
nix build .
export AUTO_REVIEW="$PWD/result/bin/auto-review"
```

The older `.#ar-cli` package name is still available, but `nix build .` is the
preferred installable package.

## 2. Create the bot PAT

Create a normal Forgejo user for the bot, add it to the repos it should review,
and mint its personal access token:

```sh
$AUTO_REVIEW auth init \
  --forgejo-url https://forgejo.example.com \
  --username auto-review
```

Save the printed one-time token as `AR_FORGEJO_TOKEN`. The gateway needs
`write:repository`, `write:issue`, and `read:user` scopes.

## 3. Configure the gateway

Create a secret env file for the gateway host or container:

```sh
FORGEJO_BASE_URL=https://forgejo.example.com
AR_FORGEJO_TOKEN=<bot PAT>
WEBHOOK_SECRET=<generate with: openssl rand -hex 32>
AR_CI_REVIEW_TOKEN=<generate with: openssl rand -hex 32>

# Local Ollama/vLLM example
LLM_BASE_URL=http://localhost:11434
LLM_REASONING_MODEL=qwen2.5-coder:32b

# Cloud example
# LLM_BASE_URL=https://api.openai.com
# LLM_API_KEY=<provider key>
# LLM_REASONING_MODEL=gpt-4o-mini
```

`AR_CI_REVIEW_TOKEN` enables `POST /reviews/ci`; store the same value as a Forgejo
Actions secret such as `AUTO_REVIEW_ACTION_TOKEN`.

## 4. Run the gateway

On supported Linux hosts, the packaged `auto-review` binary starts the gateway
through the embedded OCI launcher by default:

```sh
set -a
. ./auto-review.env
set +a
$AUTO_REVIEW gateway
```

Use `auto-review gateway --bare` only for local evaluation or custom host
boundaries where you intentionally opt out of embedded OCI isolation. Bare mode
keeps application-level controls only and is not container-equivalent isolation.

See [Deployment](./DEPLOYMENT.md) for binary, NixOS, systemd, custom
container/Kubernetes/Helm, and observability install details.

## 5. Register the webhook

```sh
$AUTO_REVIEW webhook register \
  --forgejo-url "$FORGEJO_BASE_URL" \
  --token "$AR_FORGEJO_TOKEN" \
  --owner alice --repo widgets \
  --gateway-url https://reviewer.example.com \
  --webhook-secret "$WEBHOOK_SECRET"
```

This subscribes the bot to `pull_request` and `issue_comment` events. Pull
request webhooks are low-cost intake and chat bookkeeping; normal semantic
review waits for the CI-triggered `/reviews/ci` request.

## 6. Add the CI review trigger

After your normal deterministic checks pass, call the gateway action:

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
        with:
          persist-credentials: false
      - run: just fmt

  clippy:
    runs-on: docker
    steps:
      - uses: https://code.forgejo.org/actions/checkout@v4
        with:
          persist-credentials: false
      - run: just clippy

  test:
    runs-on: docker
    steps:
      - uses: https://code.forgejo.org/actions/checkout@v4
        with:
          persist-credentials: false
      - run: just test

  deny:
    runs-on: docker
    steps:
      - uses: https://code.forgejo.org/actions/checkout@v4
        with:
          persist-credentials: false
      - run: just deny

  build:
    runs-on: docker
    steps:
      - uses: https://code.forgejo.org/actions/checkout@v4
        with:
          persist-credentials: false
      - run: just build

  semantic-review:
    runs-on: docker
    needs: [fmt, clippy, test, deny, build]
    if: ${{ github.event_name == 'pull_request' }}
    steps:
      - uses: https://git.johnwilger.com/jwilger/auto_review/deploy/forgejo-action@main
        with:
          gateway-url: https://reviewer.example.com
          action-token: ${{ secrets.AUTO_REVIEW_ACTION_TOKEN }}
          owner: ${{ github.repository_owner }}
          repo: ${{ github.event.repository.name }}
          pr-number: ${{ github.event.pull_request.number }}
          head-sha: ${{ github.event.pull_request.head.sha }}
```

Do not switch to a privileged target-style workflow that checks out or executes
untrusted fork code with secrets.

## 7. Verify

Run the diagnostic triad:

```sh
$AUTO_REVIEW ops doctor
$AUTO_REVIEW webhook test \
  --gateway-url https://reviewer.example.com \
  --webhook-secret "$WEBHOOK_SECRET"
$AUTO_REVIEW ops status --gateway-url https://reviewer.example.com
```

For a synchronous smoke test against one PR without webhooks:

```sh
$AUTO_REVIEW review once \
  --forgejo-url "$FORGEJO_BASE_URL" \
  --token "$AR_FORGEJO_TOKEN" \
  --owner alice --repo widgets --pr 42 \
  --llm-base-url "$LLM_BASE_URL" \
  --llm-model "$LLM_REASONING_MODEL"
```

## Configuration reference

Required gateway startup env vars:

| Env var | Notes |
|---|---|
| `FORGEJO_BASE_URL` | Forgejo base URL, e.g. `https://forgejo.example.com` |
| `AR_FORGEJO_TOKEN` | Bot user's PAT |
| `WEBHOOK_SECRET` | HMAC secret shared with Forgejo webhooks |
| `LLM_BASE_URL` | OpenAI-compatible endpoint root |

Required for CI-triggered semantic reviews:

| Env var | Notes |
|---|---|
| `AR_CI_REVIEW_TOKEN` | Bearer token accepted by `POST /reviews/ci`; if unset, that endpoint is disabled |

Common optional env vars:

| Env var | Default | Notes |
|---|---|---|
| `LLM_API_KEY` | — | Omit for local Ollama/vLLM when not required |
| `LLM_REASONING_MODEL` | `qwen2.5-coder:32b` | Review-generation model |
| `LLM_CHEAP_MODEL` | — | Verifier and chat-assist tier (`autofix`, `docstring`, `tests`, free-form Q&A) |
| `LLM_CHEAP_BASE_URL` / `LLM_CHEAP_API_KEY` | `=LLM_BASE_URL` / `=LLM_API_KEY` | Optional cheap-tier endpoint override |
| `LLM_EMBEDDING_MODEL` | — | Enables RAG context retrieval |
| `LLM_EMBEDDING_BASE_URL` / `LLM_EMBEDDING_API_KEY` | `=LLM_BASE_URL` / `=LLM_API_KEY` | Optional embedding endpoint override |
| `AR_GATEWAY_BIND` | `0.0.0.0:8080` | Listen address |
| `AR_BOT_LOGIN` | `auto-review` | Forgejo username for self-loop detection |
| `AR_BOT_NAME` | `=AR_BOT_LOGIN` | Mention handle |
| `AR_LEARNINGS_DB` | XDG state path | SQLite learnings path; use `:memory:` for volatile local evaluation |
| `AR_HISTORY_DB` | XDG state path | SQLite review-history path; use `:memory:` for volatile local evaluation |
| `AR_VECTOR_DB` | XDG state path | SQLite vector/RAG snippet store path; use `:memory:` to avoid persistence |
| `AR_DEDUP_DB` | XDG state path | SQLite webhook delivery dedup store path; use `:memory:` for volatile dedup |
| `AR_DEDUP_CAPACITY` | implementation default | Max delivery ids retained by the dedup store |
| `AR_EMBED_INPUT_CAP_BYTES` / `AR_EMBED_BATCH_SIZE` / `AR_EMBED_NUM_CTX` | implementation defaults | Embedding request shaping and local-model context controls |
| `AR_PRICE_TABLE_PATH` | — | JSON price override file for LLM cost estimates |
| `AR_REVIEW_COST_FOOTER` | `true` | Set `false` to persist cost without posting usage footer |
| `AR_POLL_INTERVAL_SECS` | `60` | Inline-thread mention poll cadence; `0` disables |
| `AR_SEVERITY_FLOOR` | `warning` | `note`, `warning`, or `error` |
| `AR_REVIEW_CONCURRENCY` | — | Cap concurrent reviews |
| `AR_GATEWAY_BARE` | `false` | Skip the embedded OCI launcher when intentionally using a different isolation boundary |
| `AR_GATEWAY_EXTERNAL_ISOLATION` | — | Set to `container` for operator-owned container deployments |
| `AR_WEBHOOK_RATE_PER_SEC` / `AR_WEBHOOK_BURST` | — | Optional webhook rate limiter |
| `AR_READINESS_TTL_SECS` | `10` | `/readyz` Forgejo probe cache TTL |
| `RUST_LOG` | `info,ar_gateway=debug` | Logging filter |

Full env-file examples and deployment-specific notes are in
[Deployment](./DEPLOYMENT.md).
