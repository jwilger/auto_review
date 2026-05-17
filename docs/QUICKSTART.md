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

Production deployment is container-first. The release image runs the same
`auto-review gateway` binary and marks the external container boundary with
`AR_GATEWAY_EXTERNAL_ISOLATION=container`.

For local evaluation or a custom host boundary, run the direct binary explicitly
in bare mode:

```sh
set -a
. ./auto-review.env
set +a
$AUTO_REVIEW gateway --bare
```

Bare mode is an explicit opt-out from the embedded OCI launcher. It keeps
application-level controls only and is not container-equivalent isolation.

See [Deployment](./DEPLOYMENT.md) for container image, NixOS, Kubernetes/Helm,
systemd, and observability install details.

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

Required gateway env vars:

| Env var | Notes |
|---|---|
| `FORGEJO_BASE_URL` | Forgejo base URL, e.g. `https://forgejo.example.com` |
| `AR_FORGEJO_TOKEN` | Bot user's PAT |
| `WEBHOOK_SECRET` | HMAC secret shared with Forgejo webhooks |
| `AR_CI_REVIEW_TOKEN` | Bearer token for CI-triggered review requests |
| `LLM_BASE_URL` | OpenAI-compatible endpoint root |

Common optional env vars:

| Env var | Default | Notes |
|---|---|---|
| `LLM_API_KEY` | — | Omit for local Ollama/vLLM when not required |
| `LLM_REASONING_MODEL` | `qwen2.5-coder:32b` | Review-generation model |
| `LLM_CHEAP_MODEL` | — | Triage / verifier tier |
| `LLM_EMBEDDING_MODEL` | — | Enables RAG context retrieval |
| `AR_GATEWAY_BIND` | `0.0.0.0:8080` | Listen address |
| `AR_BOT_LOGIN` | `auto-review` | Forgejo username for self-loop detection |
| `AR_BOT_NAME` | `=AR_BOT_LOGIN` | Mention handle |
| `AR_LEARNINGS_DB` | — | SQLite learnings path; unset is in-memory |
| `AR_HISTORY_DB` | — | SQLite review-history path; unset is in-memory |
| `AR_POLL_INTERVAL_SECS` | `60` | Inline-thread mention poll cadence; `0` disables |
| `AR_SEVERITY_FLOOR` | `warning` | `note`, `warning`, or `error` |
| `AR_REVIEW_CONCURRENCY` | — | Cap concurrent reviews |
| `AR_WEBHOOK_RATE_PER_SEC` / `AR_WEBHOOK_BURST` | — | Optional webhook rate limiter |
| `AR_READINESS_TTL_SECS` | `10` | `/readyz` Forgejo probe cache TTL |
| `RUST_LOG` | `info,ar_gateway=debug` | Logging filter |

Full env-file examples and deployment-specific notes are in
[Deployment](./DEPLOYMENT.md).
