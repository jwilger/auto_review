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

1. Multi-stage pipeline (triage → context curation → review → verify)
   rather than a single ReAct loop. Deterministic static analysis runs in CI
   before `auto_review` performs semantic review.
2. Two-tier model routing (cheap model for triage/summarize, reasoning
   model for review). ~50% cost win.
3. Repo-wide context via tree-sitter symbol extraction + vector embeddings +
   co-change graph + persistent "learnings" memory.
4. Avoid executing repo-controlled deterministic tooling in the reviewer
   runtime. Failure to isolate that class was exploitable: Kudelski achieved
   RCE via Rubocop running outside the jail.
5. Durable per-PR workflow with self-healing JSON-schema validation.

## Decision

Adopt the same hybrid pipeline shape, implemented in Rust as a Cargo
workspace of single-purpose crates (see `crates/`). Persist workflow
state via `sqlx`. Use the `VectorStore` abstraction for code embeddings and
persist learnings separately. CI owns deterministic linters/tests/builds;
`auto_review` clones workspaces for RAG and agentic verification, then runs the
semantic review pipeline after the CI trigger.
Provide an LLM provider abstraction that defaults to local Ollama and
currently ships an OpenAI-compatible provider for hosted OpenAI-compatible APIs,
Ollama, vLLM, OpenRouter, Together, Groq, and similar endpoints.

## Consequences

- Rust raises the bar for new contributors but pays off in the sandbox
  and orchestration layers, where memory safety and predictable
  concurrency directly reduce attack surface.
- SQLite is the persistent vector-store default today; ADR-0004 records the
  LanceDB-ready abstraction and the trigger for revisiting ANN storage.
- Removing bundled linter execution reduces reviewer-host attack surface and
  shifts deterministic tool hardening to CI; remaining sandbox needs are tracked
  separately in issue #46.
- The plug-in LLM provider trait means we can ship a "local-only"
  profile that works offline — a key differentiator for the Forgejo
  audience.

## Out of scope

- Multi-tenant SaaS (no Forgejo App identity).
- GitLab / Bitbucket adapters.
- Web GUI / dashboard.
- Fine-tuned models.
