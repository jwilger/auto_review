# Quickstart

Get `auto_review` reviewing PRs on a Forgejo instance you control with the
single `auto-review` command.

## Prerequisites

- A running Forgejo instance you can administer.
- An LLM endpoint. Either:
  - **Local**: [Ollama](https://ollama.com/) (or vLLM) serving a coding model
    such as `qwen2.5-coder:32b`.
  - **Cloud**: an OpenAI-compatible API (OpenAI, OpenRouter,
    Together.ai, Groq, etc.) and an API key.
- To build from source: [Nix](https://nixos.org/download.html)
  with flakes enabled (recommended — pins the toolchain).
- `git` on the host the gateway runs on (the orchestrator clones
  repositories at the PR's head SHA for RAG and agentic verification).
- CI that runs deterministic linters, tests, and builds before triggering
  `auto_review`'s semantic review.

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
nix build .#ar-cli

# Alternative: cargo from inside the dev shell.
nix develop --command cargo build --release --workspace
```

`nix build` produces:
- `result/bin/auto-review` — the unified operator CLI and gateway entrypoint.

The cargo path produces:
- `target/release/auto-review`

## 3. Mint a personal access token for the bot

```sh
./target/release/auto-review auth init \
    --forgejo-url https://forgejo.example.com \
    --username auto-review
# Prompts for the bot's password.
```

The CLI prints a one-time PAT secret. Save it as `AR_FORGEJO_TOKEN` in
the gateway environment — Forgejo will not show it again.
The operator CLI still accepts `--token` (or `FORGEJO_TOKEN`) for
one-off commands; prefer `--token "$AR_FORGEJO_TOKEN"` when your
developer shell reserves `FORGEJO_TOKEN` for your personal Forgejo identity.

## 4. Run the gateway

Pick a strong webhook secret (any random string) and export the
config:

```sh
export FORGEJO_BASE_URL=https://forgejo.example.com
export AR_FORGEJO_TOKEN=<the PAT from step 3>
export WEBHOOK_SECRET=<a random string, e.g. openssl rand -hex 32>
export AR_CI_REVIEW_TOKEN=<another random string, e.g. openssl rand -hex 32>

# Local LLM via Ollama
export LLM_BASE_URL=http://localhost:11434
export LLM_REASONING_MODEL=qwen2.5-coder:32b

# OR cloud LLM
# export LLM_BASE_URL=https://api.openai.com
# export LLM_API_KEY=sk-...
# export LLM_REASONING_MODEL=gpt-4o-mini

./target/release/auto-review gateway --bare
```

The gateway listens on `0.0.0.0:8080` by default; override with
`AR_GATEWAY_BIND=0.0.0.0:9090`. It needs to be reachable from your
Forgejo instance — put it behind a reverse proxy (or expose it
directly inside your private network).

During the single-binary OCI rollout, direct binary startup fails closed unless
the embedded launcher is available or you explicitly opt out with `--bare` (or
`AR_GATEWAY_BARE=true`). The bare command is intended for local evaluation and
for operators who build `auto_review` into their own custom VM images or
container images. Bare mode logs that only application-level controls are active
and is not container-equivalent isolation. The published container image marks
its external container boundary automatically.

Store the same `AR_CI_REVIEW_TOKEN` value as a CI secret (for example,
`AUTO_REVIEW_ACTION_TOKEN`) so your workflow can call `POST /reviews/ci` after
its prerequisite jobs pass.

## 5. Register the webhook on a repo

```sh
./target/release/auto-review webhook register \
    --forgejo-url $FORGEJO_BASE_URL \
    --token "$AR_FORGEJO_TOKEN" \
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
./target/release/auto-review ops doctor

# Inbound: gateway accepts a signed webhook end-to-end?
./target/release/auto-review webhook test \
    --gateway-url https://reviewer.example.com \
    --webhook-secret "$WEBHOOK_SECRET"

# Live snapshot: confirms /info, /metrics, runtime config
./target/release/auto-review ops status \
    --gateway-url https://reviewer.example.com
```

Each exits non-zero on a real problem so they fit cleanly in
shell pipes (`auto-review ops doctor && auto-review webhook test ...`).
See `docs/OPERATIONS.md` §0 for the full pre-deploy/post-deploy
checklist these implement.

## 5b. (Optional) Smoke-test against a real PR without a webhook

Before flipping the gateway live, you can run the full pipeline against
one specific PR. No webhook required:

```sh
./target/release/auto-review review once \
    --forgejo-url $FORGEJO_BASE_URL \
    --token $AR_FORGEJO_TOKEN \
    --owner alice --repo widgets --pr 42 \
    --llm-base-url $LLM_BASE_URL \
    --llm-model $LLM_REASONING_MODEL
```

This clones the repo at the PR's head SHA, prepares semantic context, calls the
LLM, and posts the review — the same review pipeline reached by CI-triggered
gateway dispatch —
but synchronously, with all logs streaming to your terminal. Useful for
onboarding and for reproducing reported review issues.

## 6. Open a PR

Opening or updating the PR sends a signed Forgejo webhook to the gateway. The
gateway verifies and accepts that low-cost intake, but the normal semantic review
waits for your workflow prerequisites. Configure Forgejo Actions (or another CI)
to run the deterministic checks you require, then call `POST /reviews/ci` with
the PR number and current head SHA.

When CI triggers the review, the gateway will:

1. Authenticate the CI request with `AR_CI_REVIEW_TOKEN` and verify that the
   supplied head SHA still matches the PR in Forgejo.
2. Skip the review if every changed file is trivial (lockfile bumps,
   vendored code, generated files); post a "skipped" commit status.
3. Otherwise: shallow-clone the repo at the head SHA, prepare RAG context,
   feed the diff to the configured LLM, self-heal any malformed JSON output,
   and post inline review comments + a top-level summary.

For a deliberate manual bypass, comment `@auto_review re-review` on the PR. The
bot queues a forced review and replies that the command intentionally bypasses CI
gating.

Latency is dominated by the LLM. Local 32B-class models on CPU can
take a couple of minutes per PR; small cloud models are much faster.

## Deployment options

### Nix / NixOS

The flake exposes the installable program, the gateway OCI image, and a NixOS
module:

```sh
# Install or run the CLI without enabling a service.
nix profile install git+https://git.johnwilger.com/jwilger/auto_review#ar-cli
nix shell git+https://git.johnwilger.com/jwilger/auto_review#ar-cli -c auto-review --help

# Build the recommended production image.
nix build git+https://git.johnwilger.com/jwilger/auto_review#ar-gateway-image
```

On NixOS, prefer running the OCI image behind your container runtime for
production because it supplies the external isolation boundary:

```nix
{
  virtualisation.oci-containers.containers.auto-review-gateway = {
    image = "git.johnwilger.com/jwilger/auto_review/ar-gateway:latest";
    ports = [ "127.0.0.1:8080:8080" ];
    volumes = [ "auto-review-state:/var/lib/auto_review" ];
    environmentFiles = [ "/run/secrets/auto-review-gateway.env" ];
  };
}
```

For operators who intentionally choose a direct systemd/bare-host boundary, import
the module and keep credentials in a root-owned runtime secret file rather than in
the Nix store:

```nix
{
  inputs.auto-review.url = "git+https://git.johnwilger.com/jwilger/auto_review";

  outputs = { nixpkgs, auto-review, ... }: {
    nixosConfigurations.reviewer = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        auto-review.nixosModules.default
        {
          services.auto-review.gateway = {
            enable = true;
            environmentFile = "/run/secrets/auto-review-gateway.env";
          };
        }
      ];
    };
  };
}
```

To install only the `auto-review` program on NixOS without running the gateway:

```nix
{
  imports = [ inputs.auto-review.nixosModules.default ];
  programs.auto-review.enable = true;
}
```

The environment file should contain the variables from §4 (`FORGEJO_BASE_URL`,
`AR_FORGEJO_TOKEN`, `WEBHOOK_SECRET`, `AR_CI_REVIEW_TOKEN`, and LLM settings) and
be provisioned by your secret manager, for example age/sops-nix, a tmpfiles rule
fed by an encrypted volume, or another out-of-store mechanism.

### docker compose

```sh
# Create a .env file with the variables from §4, including AR_CI_REVIEW_TOKEN.
docker compose -f deploy/docker-compose.yml up -d
```

### Forgejo Action

`deploy/forgejo-action/` ships a workflow template for users who
want Forgejo Actions to call the long-running gateway's CI review endpoint after
their prerequisite jobs pass. See that directory's README for the install steps.

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
  verify. `auto-review webhook test` confirms whether the gateway-
  side secret is correct; if it passes, check that Forgejo's
  webhook config has the same secret byte-for-byte.
- **Reviews never appear**: check the gateway logs (`RUST_LOG=debug`).
  Common causes: bot user lacks repo access (run
  `auto-review ops doctor`), LLM endpoint unreachable, invalid
  `AR_FORGEJO_TOKEN`, or a CI workflow that never calls `POST /reviews/ci` after
  required checks pass.
- **LLM returns malformed JSON repeatedly**: the self-heal loop is
  bounded at 3 attempts. Smaller local models (≤7B) may struggle
  with strict JSON schemas; try a larger model or switch
  `LLM_REASONING_MODEL` to a cloud option.
- **Deterministic checks missing**: configure Forgejo Actions (or another CI)
  to run linters/tests/builds and call `POST /reviews/ci` only after the checks
  you require have completed.

For Prometheus operators, drop in
`deploy/prometheus/auto_review.rules.yaml` and
`deploy/grafana/auto_review.dashboard.json` for ready-baked
alerting + dashboards.

## Configuration reference

Required:

| Env var | Notes |
|---|---|
| `FORGEJO_BASE_URL` | e.g. `https://forgejo.example.com` |
| `AR_FORGEJO_TOKEN` | gateway bot user's PAT (`auto-review auth init`) |
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
| `AR_CI_REVIEW_TOKEN` | — | enables `POST /reviews/ci` for Forgejo Actions jobs that trigger semantic review after required CI checks pass; generate independently from `WEBHOOK_SECRET` with at least 32 random bytes/chars |
| `AR_LEARNINGS_DB` | — | path → SQLite-backed learnings; unset → in-memory |
| `AR_HISTORY_DB` | — | path → SQLite-backed review history; unset → in-memory |
| `AR_POLL_INTERVAL_SECS` | `60` | inline-thread mention poll cadence; `0` disables |
| `AR_SEVERITY_FLOOR` | `warning` | drop findings below this severity (`note` to post everything, `error` to only post Error-severity) |
| `AR_WEBHOOK_RATE_PER_SEC` | — | enable webhook rate limiting (paired with `AR_WEBHOOK_BURST`) |
| `AR_DEDUP_CAPACITY` | `256` | LRU size for `X-Forgejo-Delivery` retry dedup; `0` disables |
| `AR_READINESS_TTL_SECS` | `10` | cache TTL for `/readyz` Forgejo probe |
| `RUST_LOG` | `info,ar_gateway=debug` | tracing-subscriber filter |

Deterministic linters, tests, and builds run in CI before the semantic review
trigger. The gateway clones and reads PR workspaces for review context, but it
does not run repo-controlled tools.

For local containerized gateway development, run:

```bash
nix run .#dev-gateway-container
```

The watcher rebuilds the Nix image, loads it into Podman or Docker, and
relaunches the gateway on `127.0.0.1:8090` after Rust/Nix source changes. It
passes common gateway/LLM environment variables through from the host and also
loads `.env` when present. Override the host dev port with
`AR_DEV_GATEWAY_PORT`.

Full reference (every env var with rationale): see
`deploy/systemd/auto_review.env.example`.
