# auto_review

A self-hosted, AI-driven pull-request reviewer for [Forgejo](https://forgejo.org/).

`auto_review` aims for functional parity with closed-source AI reviewers
(CodeRabbit, Greptile, Cursor BugBot) while running entirely on infrastructure
you control, with optional support for fully local LLMs.

## Status

**Alpha.** End-to-end review pipeline works: webhook intake → LLM
triage (skip lockfile-only PRs, route trivial files away from the
reasoning model) → shallow-clone → 18 bundled linters fanned out
in parallel inside an optional sandbox → tree-sitter + embedding
RAG context + persistent learnings memory → reasoning-tier LLM
with strict-JSON-schema output and self-heal validation → cheap-
tier verifier drops unfounded findings → post inline review
comments + commit status. The `@auto_review` chat handler accepts
`help`, `remember <text>`, `forget <id>`, `re-review`, and free-
form questions answered by the cheap-tier model. The `bench`
CLI subcommand replays PR fixtures through the LLM-review path
for regression tracking and model comparison. CLI helpers mint
the bot's PAT and register the webhook on a repo.

To deploy: see [QUICKSTART.md](./QUICKSTART.md). For background,
the [feasibility study](./docs/FEASIBILITY.md) lays out the broader
plan; [ADR-0001](./docs/ADR-0001-architecture.md) captures the
architecture decision.

What's still on the roadmap: a LanceDB-backed vector store
(currently in-memory + SQLite-backed for learnings), the full
~45-linter set (18 bundled today), a youki-based sandbox in
addition to the podman path, and a labelled-corpus benchmark
(the bench harness handles regression tracking; precision/recall
needs ground-truth fixtures someone has to build). Real-world
verification on a production Forgejo instance is also pending.

### Production sandbox

For internet-facing deploys, set `AR_SANDBOX_IMAGE` to point at the
hardened linter image (`deploy/Dockerfile.sandbox`). Linter spawns
go through `podman run --network=none --read-only --cap-drop=ALL
--security-opt=no-new-privileges --memory=… --cpus=… --pids-limit=…
--user 65534:65534 -v <repo>:/work:ro`. Without this set, the
gateway still works but logs a `sandbox: direct (NO ISOLATION)`
warning — fine for a local LAN trial, **not** safe for any
internet-reachable deploy. (Background: an unjailed linter is the
exact path the [Kudelski writeup](https://research.kudelskisecurity.com/2024/05/01/a-trip-down-coderabbits-rabbit-hole/)
used to reach RCE on CodeRabbit.)

## Architecture (one-paragraph)

A Forgejo webhook lands at the **gateway**, which enqueues a job for the
**orchestrator**. The orchestrator runs a per-PR durable state machine:
clone → triage → summarize → static-analysis fan-out → context curation
(tree-sitter + LanceDB embeddings + learnings store) → review generation
→ verification (drop unfounded findings) → walkthrough → post review.
All untrusted execution (linters, LLM-issued shell tools) runs in an
OCI sandbox. LLM calls go through a pluggable provider abstraction
that supports OpenAI, Anthropic, Ollama, vLLM, and OpenRouter.

## Crates

| Crate | Purpose |
|---|---|
| `ar-gateway` | HTTP webhook intake; HMAC verification; job enqueue |
| `ar-orchestrator` | Per-PR state machine; activity dispatch |
| `ar-forgejo` | Forgejo REST client |
| `ar-llm` | LLM provider trait + implementations |
| `ar-index` | Tree-sitter parser + LanceDB embeddings + co-change graph |
| `ar-tools` | Static-analysis runners + result normalization |
| `ar-sandbox` | OCI sandbox launcher |
| `ar-prompts` | Prompt templates and JSON schemas |
| `ar-review` | Review pipeline activities |
| `ar-chat` | Agentic `@auto_review` chat handler |
| `ar-cli` | Operator CLI (`auto_review init`, `replay`, etc.) |

## License

AGPL-3.0-or-later. The intent is to keep this codebase open: anyone can
self-host, modify, or fork, but a hosted-service operator must publish their
modifications. See `LICENSE`.

## Acknowledgements

Architectural lineage from public CodeRabbit engineering writing and from
[Qodo PR-Agent](https://github.com/qodo-ai/pr-agent) (Apache-2.0). Specific
prompt patterns and the `__new hunk__` / `__old hunk__` diff format are
adapted from PR-Agent under attribution.
