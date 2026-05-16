# ADR-0006: SQLite Vector Store as Persistent Default

## Status

Accepted

## Date

2026-05-01

## Provenance

Reconstructed from former `docs/ADR-0004-vector-store.md`, created in commit
`30f9db0` on 2026-05-01. Commit `8dea09a` later performed wording cleanup and is
treated as non-decision provenance.

## Context

`auto_review` needs a persistent vector store default for local development,
operator deployments, and CI-adjacent workflows. ADR-0001 assumed LanceDB would
fill this role, but the current project shape benefits from a lower-operational
cost default that is easy to provision, back up, inspect, and test.

## Decision

Use `SqliteVectorStore` as the persistent default vector store for
`auto_review`.

Keep the `VectorStore` abstraction so storage implementations remain replaceable
behind the existing review and indexing boundaries. Keep `InMemoryVectorStore`
for tests and lightweight deterministic fixtures.

Defer LanceDB adoption until concrete scale, latency, or filtering requirements
justify the additional dependency and operational surface.

## Consequences

- SQLite becomes the default persistence layer for embeddings and related
  retrieval state, keeping the default deployment path simple.
- The abstraction boundary remains intact, so a later LanceDB implementation can
  be introduced without forcing review-pipeline callers to depend on
  LanceDB-specific APIs.
- Tests can continue using `InMemoryVectorStore`, avoiding unnecessary filesystem
  and database setup where persistence is not the behavior under test.

## Supersession

This ADR partially supersedes ADR-0001's LanceDB assumption. ADR-0001 remains
authoritative for the broader architecture, but its assumed default vector-store
backend is replaced by this decision.

## Deferred triggers

Reconsider LanceDB or another dedicated vector database if measured production
usage shows that SQLite no longer satisfies required scale, latency, or filtering
behavior.
