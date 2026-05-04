# auto_review Forgejo Action

Requests a semantic review from a running `ar-gateway` after a Forgejo
Actions workflow's prerequisite jobs have passed. The action does not run the
review locally, build `ar-cli`, call LLM providers, or execute linters; it only
authenticates to the gateway's `POST /reviews/ci` endpoint.

## Usage

Configure `ar-gateway` with `AR_CI_REVIEW_TOKEN` and store the same value as an
Actions secret, for example `AUTO_REVIEW_ACTION_TOKEN`. Then add a gated review
job to `.forgejo/workflows/auto-review.yml`:

```yaml
name: auto-review
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
      - uses: https://git.johnwilger.com/jwilger/auto_review/deploy/forgejo-action@main
        with:
          gateway-url: https://reviewer.example.com
          action-token: ${{ secrets.AUTO_REVIEW_ACTION_TOKEN }}
          # Optional overrides; omitted values default from the PR context.
          owner: ${{ github.repository_owner }}
          repo: ${{ github.event.repository.name }}
          pr-number: ${{ github.event.pull_request.number }}
          head-sha: ${{ github.event.pull_request.head.sha }}
```

Forgejo Actions exposes GitHub-compatible `github.*` context values; the
workflow above still runs on a Forgejo runner. If `pr-number` is empty after
the input override and context default are evaluated, the action exits with a
clear pull request context error before contacting the gateway.

## Caveats

- **PRs from forks** do not receive repository secrets, so this direct
  `pull_request` workflow cannot call the gateway for them. Do not switch to a
  privileged target-style workflow that checks out or executes untrusted fork
  code with secrets; use a trusted maintainer-mediated path instead.
- **Gateway required**: this action is only a thin dispatcher. Run and expose
  `ar-gateway` before adding the workflow job.
- **Gateway rejection**: stale PR heads, draft PRs, closed PRs, or bad tokens
  cause the action to fail because `curl -f` treats non-2xx responses as
  workflow failures.
