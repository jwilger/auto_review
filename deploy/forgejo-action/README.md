# auto_review Forgejo Action

Runs `auto_review` as a Forgejo Action triggered by `pull_request`
events, instead of as a long-running webhook server.

## When to use this vs the gateway

| | Forgejo Action | Webhook gateway |
|---|---|---|
| Setup effort | One workflow file | Run + expose `ar-gateway` |
| Latency | Action runner cold-start (~10–30s) | None |
| Cost | Burns runner minutes per PR | Always-on container |
| Network | Outbound from runner only | Inbound HTTPS from Forgejo |
| Multi-repo | One workflow per repo | Single gateway, many repos |

The Action mode is the easier on-ramp for a single repo. The gateway
is the right shape once you're reviewing across multiple repos or
care about latency.

## Usage

Add `.forgejo/workflows/auto-review.yml` to the repo:

```yaml
name: auto-review
on:
  pull_request:
    types: [opened, synchronize, reopened, ready_for_review]

jobs:
  review:
    runs-on: docker
    steps:
      - uses: actions/checkout@v4
      - uses: https://codeberg.org/jwilger/auto_review/deploy/forgejo-action@main
        with:
          forgejo-token: ${{ secrets.GITHUB_TOKEN }}
          llm-base-url: ${{ vars.LLM_BASE_URL }}
          llm-api-key: ${{ secrets.LLM_API_KEY }}
          llm-reasoning-model: gpt-4o-mini
          # Optional: enable RAG by setting an embedding model.
          llm-embedding-model: text-embedding-3-small
          # Optional: enable LLM triage + verifier second-pass.
          llm-cheap-model: gpt-4o-mini
```

The action builds `auto_review` from source on first run. Subsequent
runs benefit from cargo's cache if the runner reuses the workspace.

## Caveats

- **PRs from forks** receive a token without write permission to the
  upstream repo, so the bot can't post reviews on them. Configure
  `pull_request_target` if you want fork PRs reviewed (and accept
  the additional security considerations).
- **Cargo build per run** is slow (~2 min on a cold runner). For
  high-throughput repos, prefer the gateway server which builds
  once.
- **Webhook mode is recommended for production**; Action mode is
  intended as a friction-free way to try the bot without standing
  up infrastructure.
