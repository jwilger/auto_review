# ADR-0010: Retire Bundled Linter Execution from Normal Review Runtime

## Status

Accepted

## Date

2026-05-03

## Provenance

Reconstructed from commit `b734b64` on 2026-05-03 and the associated
documentation mutations to ADR-0001, ADR-0002, and the former observability ADR.

## Context

`auto_review` previously described a normal review runtime that could execute
bundled linters inside a linter sandbox and route those deterministic findings
into review prompts. That design coupled semantic review to deterministic lint,
test, and build execution, increasing runtime scope and operational surface area
for work that CI already handles more reliably.

## Decision

Normal semantic review no longer executes bundled linters and no longer routes
bundled-linter findings into LLM prompts.

Deterministic lint, test, and build verification belongs in CI. The review
runtime consumes the state implied by repository-selected prerequisites and CI
outcomes, but it does not run a bundled linter sandbox as part of normal review
generation.

The `AR_SANDBOX_IMAGE` configuration and the bundled linter sandbox are retired
for normal review runtime operation.

## Consequences

- The normal review runtime has a smaller execution surface and fewer sandboxing
  obligations.
- CI remains the authoritative place for deterministic lint, test, and build
  failures.
- Semantic review focuses on code review, context curation, LLM generation,
  self-heal validation, verification, severity filtering, and posting review
  results.
- Operational documentation and metrics should avoid implying that bundled linter
  execution is part of the normal runtime path.

## Supersession

This ADR supersedes ADR-0002 and ADR-0007 for normal runtime linter sandbox
execution.

This ADR partially supersedes ADR-0001 where ADR-0001 described bundled linter
execution or routing linter findings into prompts as part of the normal review
pipeline.
