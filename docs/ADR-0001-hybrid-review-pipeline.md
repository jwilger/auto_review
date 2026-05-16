# ADR-0001: Hybrid Review Pipeline and Rust Workspace

## Status

Partially superseded

## Date

2026-04-30

## Provenance

Reconstructed from `docs/ADR-0001-architecture.md` as created in commit
`83e63d5` on 2026-04-30. Later edits in `b734b64` and `8dea09a` are
represented as supersession notes rather than folded back into the original
decision.

## Context

`auto_review` was intended to provide CodeRabbit-like review support for Forgejo
pull requests. Public material and third-party observations of comparable
systems pointed toward a hybrid pipeline plus agentic review shape rather than a
single open-ended ReAct loop.

The initial load-bearing assumptions were:

- Forgejo PR intake should feed a durable per-PR review workflow.
- Review should run as a multi-stage pipeline: triage, summarization, static
  analysis, context curation, review, and verification.
- Model routing should distinguish cheap work such as triage or summarization
  from reasoning-heavy review work.
- Repository-wide context should come from tree-sitter symbol extraction,
  LanceDB-backed vector embeddings, co-change data, and persistent learnings.
- Untrusted repository execution needed isolation because reviewer-host execution
  had known RCE failure modes in comparable systems.
- Review output needed schema validation and self-healing to make the workflow
  durable enough for PR automation.

## Decision

Adopt a Rust Cargo workspace made of single-purpose crates. Use Forgejo pull
requests as the primary intake surface and run a hybrid review pipeline for each
PR rather than relying on one monolithic LLM agent.

The accepted initial system shape was:

- A Rust workspace for orchestration, Forgejo integration, LLM routing, review
  logic, indexing, and CLI/operator entrypoints.
- A durable per-PR workflow persisted in Postgres through `sqlx`.
- A staged review pipeline covering triage, summarization, static analysis,
  repository context curation, review generation, and verification.
- Model tiers with cheaper models for triage/summarization and reasoning models
  for review.
- Repository context from tree-sitter symbols, LanceDB vector embeddings,
  co-change graph data, and persistent learnings stored in LanceDB.
- Sandboxed execution for linters and LLM-issued shell commands.
- An LLM provider abstraction oriented around local Ollama by default while
  allowing hosted/provider-backed models through the same trait.

Initial out-of-scope boundaries were multi-tenant SaaS identity, GitLab and
Bitbucket adapters, a web GUI/dashboard, and fine-tuned models.

## Consequences

- Rust increased the contributor learning curve but was accepted for memory
  safety, predictable concurrency, and lower orchestration/sandbox attack
  surface.
- The workspace boundary made the system explicit, but required discipline around
  crate responsibilities.
- The hybrid pipeline made cost and quality controls easier than a single agent
  loop, especially with cheap/reasoning model tiers.
- Durable workflow state and schema validation made PR automation more reliable,
  while adding persistence and recovery concerns.
- Repository context became a first-class review input, creating future pressure
  around vector-store choice and learnings persistence.
- Sandboxed execution added operational complexity, but was initially considered
  necessary for repo-controlled tool or command execution.

## Superseded / amended by

- ADR-0006 replaces the original LanceDB assumption with SQLite as the persistent
  vector-store default while preserving the `VectorStore` abstraction.
- ADR-0008 replaces the implied external persistence direction with embedded
  SQLite runtime state for the single-tenant deployment model.
- ADR-0010 retires bundled linter execution from the normal review runtime.
- ADR-0011 replaces broad sandbox assumptions for normal workspace access with
  hermetic Git and path-confined read-only tooling.
- ADR-0012 makes CI-gated dispatch the normal semantic review path.
