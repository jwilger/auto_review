# auto_review

A self-hosted, AI-driven pull-request reviewer for [Forgejo](https://forgejo.org/).

`auto_review` aims for functional parity with closed-source AI reviewers
(CodeRabbit, Greptile, Cursor BugBot) while running entirely on infrastructure
you control, with optional support for fully local LLMs.

## Status

**Alpha.** End-to-end review pipeline works: webhook intake → LLM
triage (skip lockfile-only PRs, route trivial files away from the
reasoning model) → shallow-clone → tree-sitter + embedding
RAG context + persistent learnings memory → reasoning-tier LLM
with strict-JSON-schema output and self-heal validation → cheap-
tier verifier drops unfounded findings → post inline review
comments + commit status. The `@auto_review` chat handler accepts
`help`, `remember <text>`, `forget <id>`, `re-review`,
`autofix`, `docstring`, `tests`, and free-form questions
answered by the cheap-tier model. The `bench`
CLI subcommand replays PR fixtures through the LLM-review path
for regression tracking and model comparison. CLI helpers mint
the bot's PAT and register the webhook on a repo.

Build, dev, and CI all run through one `flake.nix` so local
work and CI exercise identical derivations bit-for-bit
(see [CONTRIBUTING.md](./CONTRIBUTING.md) for the dev setup,
or `nix flake check` for the same gates CI runs).

To deploy: see [QUICKSTART.md](./QUICKSTART.md). To run on an
ongoing basis (rotation, upgrades, alerts, repo config),
see [docs/OPERATIONS.md](./docs/OPERATIONS.md). If you're a
PR author whose changes are reviewed by an `auto_review`
deployment and you want to know what the bot does and how to
talk to it, see [docs/USER-GUIDE.md](./docs/USER-GUIDE.md).
If you've found a security issue, see
[SECURITY.md](./SECURITY.md) for the disclosure process. For background,
the [feasibility study](./docs/FEASIBILITY.md) lays out the broader
plan; [ADR-0001](./docs/ADR-0001-architecture.md) captures the
architecture decision; the [threat model](./docs/THREAT-MODEL.md)
enumerates attacker profiles, trust boundaries, and per-class
mitigations (read this before exposing the bot to drive-by PRs).
[ADR-0002](./docs/ADR-0002-sandbox.md) records the superseded linter
sandbox decision and completed issue #46 rescope; [ADR-0003](./docs/ADR-0003-observability.md)
documents the metrics / readiness / runtime-introspection design;
[ADR-0004](./docs/ADR-0004-vector-store.md) explains why
embeddings persist via SQLite today rather than LanceDB.

What's still on the roadmap: real-world verification on a
production Forgejo instance with real PR traffic; a larger
labelled-corpus benchmark (5 fixtures ship today across SQLi /
command injection / hardcoded secrets / path traversal / XSS,
but a production-quality precision-recall sweep needs more); a
LanceDB-backed vector store as a drop-in for the SQLite path
(documented in ADR-0004) when a deployment outgrows
brute-force cosine. The runtime no longer bundles or runs linters;
deterministic linters/tests/builds belong in CI, which can trigger
semantic review after required checks pass.

## Architecture (one-paragraph)

A Forgejo webhook lands at the **gateway**, which enqueues a job for the
**orchestrator**. The orchestrator runs a per-PR review pipeline:
clone → triage → context curation
(tree-sitter symbols + in-memory cosine-similarity over the
learnings store) → review generation → verification (drop unfounded
findings) → severity-floor filter → post review.
LLM workspace tools are read-only and constrained to the clone root; CI owns
deterministic tool execution. LLM calls go through a pluggable provider abstraction
that today ships an OpenAI-compatible client (works against hosted
OpenAI, Ollama, vLLM, OpenRouter, Together, Groq, etc.).

## Crates

| Crate | Purpose |
|---|---|
| `ar-gateway` | HTTP webhook intake; HMAC verification; job enqueue |
| `ar-orchestrator` | Per-PR state machine; activity dispatch |
| `ar-forgejo` | Forgejo REST client |
| `ar-llm` | LLM provider trait + implementations |
| `ar-index` | Tree-sitter parsers + embeddings + co-change graph + learnings store |
| `ar-tools` | Legacy static-analysis runners retained outside normal review runtime |
| `ar-sandbox` | Sandbox abstraction retained for future execution features and tests |
| `ar-prompts` | Prompt templates and JSON schemas |
| `ar-review` | Review pipeline activities |
| `ar-chat` | Agentic `@auto_review` chat handler |
| `ar-cli` | Operator CLI; see `crates/ar-cli/README.md` |

## License

AGPL-3.0-or-later. The intent is to keep this codebase open: anyone can
self-host, modify, or fork, but a hosted-service operator must publish their
modifications. See `LICENSE`.

## Acknowledgements

Architectural lineage from public CodeRabbit engineering writing and from
[Qodo PR-Agent](https://github.com/qodo-ai/pr-agent) (Apache-2.0). Specific
prompt patterns and the `__new hunk__` / `__old hunk__` diff format are
adapted from PR-Agent under attribution.
