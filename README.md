# auto_review

A self-hosted, AI-driven pull-request reviewer for [Forgejo](https://forgejo.org/)
and GitHub.

`auto_review` gives Forgejo and GitHub operators a sovereignty-friendly alternative to
closed-source AI reviewers: it runs on infrastructure you control, supports local
or cloud OpenAI-compatible LLM endpoints, waits for your CI checks, reviews PRs
semantically, verifies findings before posting, and talks to authors through
`@auto-review` chat commands (`@auto_review` remains accepted as a
compatibility alias).

## TL;DR: install and run

1. Install or build the single public command:

   ```sh
   git clone https://github.com/jwilger/auto_review
   cd auto_review
   nix build .
   export AUTO_REVIEW="$PWD/result/bin/auto-review"
   ```

   Release downloads are also published as Linux `x86_64` archives with
   checksums, signatures, SBOM, and provenance metadata.

2. Create a Forgejo bot user, add it to the repos it reviews, and mint its PAT:

   ```sh
   $AUTO_REVIEW auth init \
     --forgejo-url https://forgejo.example.com \
     --username auto-review
   ```

3. Configure the gateway environment:

   ```sh
   FORGEJO_BASE_URL=https://forgejo.example.com
   AR_FORGEJO_TOKEN=<bot PAT>
   WEBHOOK_SECRET=<openssl rand -hex 32>
   LLM_BASE_URL=http://localhost:11434
   LLM_REASONING_MODEL=qwen2.5-coder:32b
   ```

4. Run the gateway. The supported out-of-the-box Linux service path is the
   `auto-review` binary with embedded OCI isolation:

   ```sh
   set -a
   . /etc/auto_review/auto_review.env
   set +a
   $AUTO_REVIEW gateway
   ```

   For local evaluation without embedded OCI isolation:

   ```sh
   $AUTO_REVIEW gateway --bare
   ```

5. Register a repo webhook and opt in to automatic webhook reviews as shown
   in [Quickstart](./docs/QUICKSTART.md).

Full setup: [Quickstart](./docs/QUICKSTART.md). Deployment options:
[Deployment](./docs/DEPLOYMENT.md). Day-2 operation:
[Operations](./docs/OPERATIONS.md). PR-author guide:
[User Guide](./docs/USER-GUIDE.md). Security posture:
[Threat Model](./docs/THREAT-MODEL.md).

## Current status

`auto_review` is released and versioned under semver — see
[CHANGELOG](./CHANGELOG.md) for the current release. The supported runtime
contract is the single-binary Forgejo reviewer path: a signed PR webhook or
CI callback triggers semantic review, the gateway isolates workspace handling, and the bot posts verified
review output back to Forgejo. The same gateway also serves GitHub repositories
through a GitHub App — automatic webhook-triggered reviews and chat (see
[GitHub repositories](./docs/GITHUB.md)). The reviewer can also run as an AWS
Bedrock AgentCore runtime instead of the long-lived gateway service.

```text
Forgejo webhook / CI trigger
  -> gateway HMAC + token validation
  -> shallow clone
  -> deterministic triage + RAG context + learnings
  -> reasoning-tier LLM strict JSON output
  -> self-heal
  -> pre-verifier severity floor
  -> cheap-tier verification
  -> post-verifier floor + path guard
  -> inline review comments + commit status
```

Webhook mode starts review for eligible non-draft PRs on either host without
waiting for CI unless the repository configures named gates. Existing Forgejo
repositories can keep calling `POST /reviews/ci` after their selected checks.
Explicit `@auto-review re-review` can force a review.

The chat handler supports `help`, `remember <text>`, `forget <id>`, `re-review`,
`autofix`, `tests`, and free-form questions. The `bench` command
replays labelled fixtures for regression tracking and model comparison.

What is not in the runtime: bundled linters, repo-controlled test/build
execution, or LLM-issued shell commands. Deterministic linters/tests/builds
belong in CI before the semantic-review trigger.

<!-- 10kr:begin how-work-flows -->
## How work flows

```text
GitHub issue ──(maintainer adds factory:ready)──▶ factory: spec → slice → build → review → approve ──▶ pull request ──▶ CI ──▶ merge
```

- **Requests** are GitHub issues. Anyone may open one; the maintainer triages it and, when it is ready for agents, labels it `factory:ready`. The factory only picks up issues the maintainer wrote, and treats everyone else's text as information, not instructions.
- **Building** happens in the agent delivery factory (`factory/`, `.claude/`): a spec writer turns the issue into acceptance criteria, a builder implements them test-first until `just ci` is green, and four independent reviewers (correctness, security, architecture, operability) check the change before an approver applies `factory/policy.md`.
- **Merging**: pull requests need the required CI checks listed in `.github/rulesets/require-pr.json` and no human approval. Factory pull requests merge automatically once approved, except those that change the factory or its gates (`.claude/`, `factory/`, `.github/`, `Justfile`, `Cargo.toml` and the toolchain files), which the maintainer reviews and merges.
- **Waiting on a person**: an issue labelled `status:awaiting-human` carries a short options brief; the maintainer answers it on the issue and runs `/factory approve <issue>` or `/factory rework <issue>`.
- **Stopping it**: open an issue labelled `factory:halt`.

Contributions by hand work as before: open a pull request against `main` (see `CONTRIBUTING.md`).
<!-- 10kr:end how-work-flows -->

## Architecture in one paragraph

A Forgejo webhook lands at the gateway, which HMAC-verifies PR intake and chat
commands. The optional CI endpoint verifies a bearer token and re-checks the PR
head SHA before dispatch. The orchestrator runs clone → deterministic triage →
context curation (diff, changed paths, repo guidelines, indexed symbols, and
available learnings/RAG context) → review generation → self-heal → pre-verifier
severity filtering → verifier → post-verifier floor/path guard → Forgejo
review/status posting. LLM
workspace tools are read-only and path-confined. LLM calls go through a tiered
OpenAI-compatible provider abstraction that works with hosted OpenAI-compatible
providers, Ollama, vLLM, OpenRouter, Together, Groq, and similar endpoints.

## Documentation map

- [Quickstart](./docs/QUICKSTART.md) — shortest install-and-run path.
- [Deployment](./docs/DEPLOYMENT.md) — binary, Nix/NixOS, systemd, custom
  container/Helm, Forgejo Actions, Prometheus, Grafana, and runner-cache notes.
- [Operations](./docs/OPERATIONS.md) — health checks, metrics, failures,
  rotation, history/learnings maintenance, upgrades.
- [User Guide](./docs/USER-GUIDE.md) — what PR authors see and how they talk to
  the bot.
- [CLI Reference](./docs/CLI.md) — grouped `auto-review` command surface.
- [Benchmarks](./docs/BENCHMARKS.md) — fixture replay and labelled corpus
  scoring.
- [Crate Map](./docs/CRATES.md) — workspace crate responsibilities.
- [Threat Model](./docs/THREAT-MODEL.md) and [ADRs](./docs/) — security and
  design rationale.

## Crates

| Crate | Purpose |
|---|---|
| `ar-gateway` | HTTP webhook intake, HMAC verification, CI/chat dispatch, ops endpoints |
| `ar-orchestrator` | Per-PR state machine, job dispatch, review history, lifecycle observations |
| `ar-forge` | Provider-neutral repository-host DTOs and the `ReviewHost` trait shared by the Forgejo and GitHub adapters |
| `ar-forgejo` | Forgejo REST client |
| `ar-github` | GitHub App REST client for review-host operations |
| `ar-agentcore` | AWS Bedrock AgentCore-compatible runtime HTTP surface |
| `ar-llm` | LLM provider trait and tier router |
| `ar-index` | Tree-sitter symbols, embeddings, vector stores, learnings store |
| `ar-prompts` | Prompt templates and JSON schemas |
| `ar-review` | Review pipeline activities |
| `ar-chat` | `@auto-review` chat handling |
| `ar-cli` | `auto-review` operator command |

## License

AGPL-3.0-or-later. See [LICENSE](./LICENSE).

## Acknowledgements

Architectural lineage from public CodeRabbit engineering writing and from
[Qodo PR-Agent](https://github.com/qodo-ai/pr-agent) (Apache-2.0). Specific
prompt patterns and the `__new hunk__` / `__old hunk__` diff format are adapted
from PR-Agent under attribution.
