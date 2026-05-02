# Quickstart

Get `auto_review` reviewing PRs on a Forgejo instance you control.

## Prerequisites

- A running Forgejo instance you can administer.
- An LLM endpoint. Either:
  - **Local**: [Ollama](https://ollama.com/) (or vLLM) serving a coding model
    such as `qwen2.5-coder:32b`.
  - **Cloud**: an OpenAI-compatible API (OpenAI, OpenRouter,
    Together.ai, Groq, etc.) and an API key.
- To build from source: [Nix](https://nixos.org/download.html)
  with flakes enabled (recommended — pins the toolchain) **or**
  Docker (for the pre-built sandbox image).
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
git clone https://git.johnwilger.com/jwilger/auto_review
cd auto_review

# Recommended: flake-pinned build. No system Rust install
# needed; reproducible across machines.
nix build .#ar-gateway .#ar-cli

# Alternative: cargo from inside the dev shell.
nix develop --command cargo build --release --workspace
```

`nix build` produces:
- `result/bin/ar-gateway` — the long-running HTTP server.
- `result-1/bin/auto_review` — the operator CLI.

The cargo path produces:
- `target/release/ar-gateway`
- `target/release/auto_review`

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

## 5a. Verify the deploy (recommended)

Three CLI commands cover deploy-time verification — drop them
into your deploy script:

```sh
# Outbound deps: Forgejo PAT valid? LLM endpoint reachable?
# Configured models actually loaded? Webhook secret strong?
./target/release/auto_review doctor

# Inbound: gateway accepts a signed webhook end-to-end?
./target/release/auto_review test-webhook \
    --gateway-url https://reviewer.example.com \
    --webhook-secret "$WEBHOOK_SECRET"

# Live snapshot: confirms /info, /metrics, runtime config
./target/release/auto_review status \
    --gateway-url https://reviewer.example.com
```

Each exits non-zero on a real problem so they fit cleanly in
shell pipes (`auto_review doctor && auto_review test-webhook ...`).
See `docs/OPERATIONS.md` §0 for the full pre-deploy/post-deploy
checklist these implement.

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

`deploy/forgejo-action/` ships a workflow template for users who
want to run the bot as a Forgejo Actions job (one shot per PR
event) instead of a long-running gateway. See that directory's
README for the install steps.

### systemd

Self-hosters running on bare metal use the unit at
`deploy/systemd/auto_review.service` plus the env-file template
at `deploy/systemd/auto_review.env.example`. See
`deploy/systemd/README.md` for the install walkthrough.

## Troubleshooting

First stop: run the diagnostic triad (see §5a). They cover most
of the common failure modes with explicit error messages.

If those pass and reviews still don't appear:
- **Gateway returns 401 Unauthorized**: the webhook signature didn't
  verify. `auto_review test-webhook` confirms whether the gateway-
  side secret is correct; if it passes, check that Forgejo's
  webhook config has the same secret byte-for-byte.
- **Reviews never appear**: check the gateway logs (`RUST_LOG=debug`).
  Common causes: bot user lacks repo access (run
  `auto_review doctor`), LLM endpoint unreachable, invalid
  `FORGEJO_TOKEN`.
- **LLM returns malformed JSON repeatedly**: the self-heal loop is
  bounded at 3 attempts. Smaller local models (≤7B) may struggle
  with strict JSON schemas; try a larger model or switch
  `LLM_REASONING_MODEL` to a cloud option.
- **Linter findings missing**: the linter's binary may not be on
  `$PATH`. Use `auto_review explain-routing --file <path>` to
  see which linters would run for a given file. Install the
  missing binary, or accept that the bot reviews without it (the
  LLM still works).

For Prometheus operators, drop in
`deploy/prometheus/auto_review.rules.yaml` and
`deploy/grafana/auto_review.dashboard.json` for ready-baked
alerting + dashboards.

## Configuration reference

Required:

| Env var | Notes |
|---|---|
| `FORGEJO_BASE_URL` | e.g. `https://forgejo.example.com` |
| `FORGEJO_TOKEN` | bot user's PAT (`auto_review init`) |
| `WEBHOOK_SECRET` | HMAC secret; matches Forgejo's webhook config |
| `LLM_BASE_URL` | OpenAI-compatible endpoint root |

Common optional env vars:

| Env var | Default | Notes |
|---|---|---|
| `LLM_API_KEY` | — | omit for local Ollama |
| `LLM_REASONING_MODEL` | `qwen2.5-coder:32b` | model name on the LLM endpoint |
| `LLM_CHEAP_MODEL` | — | optional triage / verifier tier (recommended) |
| `LLM_EMBEDDING_MODEL` | — | optional RAG retrieval (recommended) |
| `AR_EMBED_INPUT_CAP_BYTES` | `6144` | per-snippet byte cap before embedding; raise to ~24576 for hosted OpenAI embedders |
| `AR_EMBED_BATCH_SIZE` | `32` | inputs per `/v1/embeddings` POST |
| `AR_EMBED_NUM_CTX` | — | sends `options.num_ctx` on embed requests; set when pointing at Ollama so a raised input cap isn't silently truncated by the server's default 2048 |
| `AR_GATEWAY_BIND` | `0.0.0.0:8080` | listen address |
| `AR_BOT_LOGIN` | `auto_review` | bot's Forgejo username (self-loop detection) |
| `AR_BOT_NAME` | `=AR_BOT_LOGIN` | mention handle (`@<bot_name>`) |
| `AR_LEARNINGS_DB` | — | path → SQLite-backed learnings; unset → in-memory |
| `AR_HISTORY_DB` | — | path → SQLite-backed review history; unset → in-memory |
| `AR_POLL_INTERVAL_SECS` | `60` | inline-thread mention poll cadence; `0` disables |
| `AR_SANDBOX_IMAGE` | — | Required OCI image for the linter sandbox, e.g. `git.johnwilger.com/jwilger/auto_review/sandbox:<version>` built from `deploy/Dockerfile.sandbox` |
| `AR_SANDBOX_RUNTIME` | auto-detect `podman`, then `docker` | Optional OCI runtime override; startup fails if neither runtime is available |
| `AR_SEVERITY_FLOOR` | `warning` | drop findings below this severity (`note` to post everything, `error` to only post Error-severity) |
| `AR_WEBHOOK_RATE_PER_SEC` | — | enable webhook rate limiting (paired with `AR_WEBHOOK_BURST`) |
| `AR_DEDUP_CAPACITY` | `256` | LRU size for `X-Forgejo-Delivery` retry dedup; `0` disables |
| `AR_READINESS_TTL_SECS` | `10` | cache TTL for `/readyz` Forgejo probe |
| `RUST_LOG` | `info,ar_gateway=debug` | tracing-subscriber filter |

Full reference (every env var with rationale): see
`deploy/systemd/auto_review.env.example`.
