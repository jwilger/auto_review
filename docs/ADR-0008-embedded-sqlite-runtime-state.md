# ADR-0008: Embedded SQLite Runtime State for Single-Tenant Operation

## Status

Accepted

## Date

2026-05-02

## Provenance

Reconstructed from SQLite persistence work in commits `bdcb16e`, `1db7e61`, and
`ea2e364`.

## Context

`auto_review` is operated as a single-tenant service for one Forgejo
installation. Runtime state needs to survive process restarts and deployments
without requiring operators to provision a separate database service.

Earlier architecture notes assumed a Postgres-style persistence layer for durable
application state. Subsequent implementation work established local
SQLite-backed stores for runtime data that is owned by this service and does not
require multi-tenant database administration.

The durable state includes:

- learned repository and review memories;
- review history used to avoid duplicate or stale feedback;
- vector embeddings and related index metadata;
- webhook delivery deduplication state.

Tests and local development still benefit from in-memory stores where
persistence is not the behavior under test.

## Decision

Persist runtime state locally using embedded SQLite-backed stores rather than
Postgres or other external database services.

SQLite is the default durable storage mechanism for single-tenant operation.
In-memory stores remain acceptable for tests and development paths where the
relevant behavior does not depend on process-restart durability.

## Consequences

- Operators can deploy `auto_review` with fewer moving parts: the service
  requires a writable local data directory instead of a separately managed
  database server.
- Runtime state remains durable across restarts as long as the configured SQLite
  files are preserved by deployment and backup procedures.
- SQLite schema migrations and file lifecycle become part of the application's
  operational contract.
- If `auto_review` later becomes multi-tenant or requires horizontally scaled
  writers sharing the same state, the persistence decision will need to be
  revisited.

## Supersession

This ADR partially supersedes ADR-0001's persistence assumption where ADR-0001
implies an external database service for runtime state.
