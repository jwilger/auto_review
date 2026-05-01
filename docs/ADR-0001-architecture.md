# ADR-0001: Overall architecture

**Status**: Accepted
**Date**: 2026-04-30

## Context

`auto_review` aims for parity with CodeRabbit on Forgejo. CodeRabbit's
public engineering writing (their blog, the LanceDB case study, the Google
Cloud Run case study, the Software Engineering Daily interview with Harjot
Gill, the Kudelski Security RCE post-mortem) plus reverse-engineering by
third parties indicate a **hybrid pipeline + agentic** design with these
load-bearing properties:

1. Multi-stage pipeline (triage → summarize → static analysis → context
   curation → review → verify) rather than a single ReAct loop.
2. Two-tier model routing (cheap model for triage/summarize, reasoning
   model for review). ~50% cost win.
3. Repo-wide context via tree-sitter symbol extraction + LanceDB vector
   embeddings + co-change graph + persistent "learnings" memory.
4. All untrusted execution in a sandbox. Failure to do so was
   exploitable: Kudelski achieved RCE via Rubocop running outside the jail.
5. Durable per-PR workflow with self-healing JSON-schema validation.

## Decision

Adopt the same hybrid pipeline shape, implemented in Rust as a Cargo
workspace of single-purpose crates (see `crates/`). Persist workflow
state in Postgres via `sqlx`. Use LanceDB embedded for both code
embeddings and learnings. Run all linters and LLM-issued shell commands
in an OCI sandbox via `youki` (with a `podman --runtime=crun` fallback).
Provide an LLM provider abstraction that defaults to local Ollama and
supports OpenAI / Anthropic / vLLM / OpenRouter via the same trait.

## Consequences

- Rust raises the bar for new contributors but pays off in the sandbox
  and orchestration layers, where memory safety and predictable
  concurrency directly reduce attack surface.
- LanceDB embedded means one fewer service to operate, at the cost of
  having to swap if we later need horizontally-scaled retrieval. The
  `VectorStore` abstraction in `ar-index` keeps this swap cheap.
- Sandboxing adds operational complexity (OCI tooling on the host) but
  is mandatory; this is documented in the threat model.
- The plug-in LLM provider trait means we can ship a "local-only"
  profile that works offline — a key differentiator for the Forgejo
  audience.

## Out of scope (per feasibility study §15)

- Multi-tenant SaaS (no Forgejo App identity).
- GitLab / Bitbucket adapters.
- Web GUI / dashboard.
- Fine-tuned models.
