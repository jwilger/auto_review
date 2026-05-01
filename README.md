# auto_review

A self-hosted, AI-driven pull-request reviewer for [Forgejo](https://forgejo.org/).

`auto_review` aims for functional parity with closed-source AI reviewers
(CodeRabbit, Greptile, Cursor BugBot) while running entirely on infrastructure
you control, with optional support for fully local LLMs.

## Status

**Alpha.** End-to-end review pipeline works: webhook intake → triage
(skip lockfile-only PRs) → shallow-clone the repo → run language-
appropriate linters → call the LLM with a strict-JSON-schema prompt
and self-heal validation → post inline review comments and a
top-level summary. CLI helpers mint the bot's PAT and register the
webhook on a repo.

To deploy: see [QUICKSTART.md](./QUICKSTART.md). For background,
the [feasibility study](./docs/FEASIBILITY.md) lays out the broader
plan; [ADR-0001](./docs/ADR-0001-architecture.md) captures the
architecture decision.

What's still on the roadmap (per the feasibility study's later
milestones): RAG with tree-sitter + LanceDB, persistent learnings
memory, OCI sandbox for linter execution, agentic `@auto_review`
chat, and the full ~45-linter set CodeRabbit ships. Real-world
verification on a live Forgejo instance is also pending.

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
