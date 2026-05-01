# Quickstart

Get `auto_review` reviewing PRs on a Forgejo instance you control.

## Prerequisites

- A running Forgejo instance you can administer.
- An LLM endpoint. Either:
  - **Local**: [Ollama](https://ollama.com/) (or vLLM) serving a coding model
    such as `qwen2.5-coder:32b`.
  - **Cloud**: an OpenAI-compatible API (OpenAI, OpenRouter,
    Together.ai, Groq, etc.) and an API key.
- A Rust toolchain (1.85+) **or** Docker.
- `git` on the host the gateway runs on (the orchestrator clones
  repositories at the PR's head SHA before running linters).
- The optional linters that match your codebases: `ruff`, `eslint`,
  `shellcheck`, `hadolint`, `markdownlint`. Missing binaries are
  silently skipped — install only what's relevant.

## 1. Create a bot user in Forgejo

In Forgejo's site administration, create a regular user account that
will own the bot's reviews — e.g. `auto-review`. Give it any password;
you'll use it once below to mint a token, then it stays unused.

Add this user as a collaborator (or member of an org-wide team) on every
repo it should review.

## 2. Build the gateway and CLI

```sh
git clone https://codeberg.org/jwilger/auto_review
cd auto_review
cargo build --release --workspace
```

This produces:
- `target/release/ar-gateway` — the long-running HTTP server.
- `target/release/auto_review` — the operator CLI.

## 3. Mint a personal access token for the bot

```sh
./target/release/auto_review init \
    --forgejo-url https://forgejo.example.com \
    --username auto-review
# Prompts for the bot's password.
```

The CLI prints a one-time PAT secret. Save it as `FORGEJO_TOKEN` in
your environment — Forgejo will not show it again.

## 4. Run the gateway

Pick a strong webhook secret (any random string) and export the
config:

```sh
export FORGEJO_BASE_URL=https://forgejo.example.com
export FORGEJO_TOKEN=<the PAT from step 3>
export WEBHOOK_SECRET=<a random string, e.g. openssl rand -hex 32>

# Local LLM via Ollama
export LLM_BASE_URL=http://localhost:11434
export LLM_REASONING_MODEL=qwen2.5-coder:32b

# OR cloud LLM
# export LLM_BASE_URL=https://api.openai.com
# export LLM_API_KEY=sk-...
# export LLM_REASONING_MODEL=gpt-4o-mini

./target/release/ar-gateway
```

The gateway listens on `0.0.0.0:8080` by default; override with
`AR_GATEWAY_BIND=0.0.0.0:9090`. It needs to be reachable from your
Forgejo instance — put it behind a reverse proxy (or expose it
directly inside your private network).

## 5. Register the webhook on a repo

```sh
./target/release/auto_review register-webhook \
    --owner alice --repo widgets \
    --gateway-url https://reviewer.example.com
```

This subscribes the bot to `pull_request` and `issue_comment` events,
secured with `WEBHOOK_SECRET`.

## 5b. (Optional) Smoke-test against a real PR without a webhook

Before flipping the gateway live, you can run the full pipeline against
one specific PR. No webhook required:

```sh
./target/release/auto_review review-once \
    --forgejo-url $FORGEJO_BASE_URL \
    --token $FORGEJO_TOKEN \
    --owner alice --repo widgets --pr 42 \
    --llm-base-url $LLM_BASE_URL \
    --llm-model $LLM_REASONING_MODEL
```

This clones the repo at the PR's head SHA, runs the linters, calls the
LLM, and posts the review — exactly what the webhook flow would do —
but synchronously, with all logs streaming to your terminal. Useful for
onboarding and for reproducing reported review issues.

## 6. Open a PR

The gateway will:

1. Verify the webhook's HMAC-SHA256 signature.
2. Skip the review if every changed file is trivial (lockfile bumps,
   vendored code, generated files); post a "skipped" commit status.
3. Otherwise: shallow-clone the repo at the head SHA, run
   language-appropriate linters (ruff for Python, eslint for JS/TS,
   shellcheck for bash, hadolint for Dockerfiles, markdownlint for
   markdown), feed the diff + linter findings to the configured LLM,
   self-heal any malformed JSON output, and post inline review
   comments + a top-level summary.

Latency is dominated by the LLM. Local 32B-class models on CPU can
take a couple of minutes per PR; small cloud models are much faster.

## Deployment options

### docker compose

```sh
cp .env.example .env  # set the variables above
docker compose -f deploy/docker-compose.yml up -d
```

### Forgejo Action

Not yet packaged. The gateway expects a long-running HTTP server
today; an Action-based mode is on the roadmap.

## Troubleshooting

- **Gateway returns 401 Unauthorized**: the webhook signature didn't
  verify. Check that the secret in Forgejo's webhook config matches
  `WEBHOOK_SECRET` byte-for-byte.
- **Reviews never appear**: check the gateway logs (`RUST_LOG=debug`).
  Common causes: bot user lacks repo access, LLM endpoint unreachable,
  invalid `FORGEJO_TOKEN`.
- **LLM returns malformed JSON repeatedly**: the self-heal loop
  bounded at 3 attempts. Smaller local models (≤7B) may struggle with
  strict JSON schemas; try a larger model or switch
  `LLM_REASONING_MODEL` to a cloud option.
- **Linter findings missing**: the linter's binary may not be on
  `$PATH`. Install it, or accept that the bot reviews without it
  (the LLM still works).

## Configuration reference

| Env var | Required | Default | Notes |
|---|---|---|---|
| `FORGEJO_BASE_URL` | yes | — | e.g. `https://forgejo.example.com` |
| `FORGEJO_TOKEN` | yes | — | bot user's PAT (`auto_review init`) |
| `WEBHOOK_SECRET` | yes | — | HMAC secret; matches Forgejo's webhook config |
| `LLM_BASE_URL` | yes | — | OpenAI-compatible endpoint root |
| `LLM_API_KEY` | no | — | omit for local Ollama |
| `LLM_REASONING_MODEL` | no | `qwen2.5-coder:32b` | model name passed to the LLM endpoint |
| `AR_GATEWAY_BIND` | no | `0.0.0.0:8080` | listen address |
| `RUST_LOG` | no | `info,ar_gateway=debug` | tracing-subscriber filter |
