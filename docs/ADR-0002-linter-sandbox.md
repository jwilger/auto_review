# ADR-0002: Linter Sandbox for Repo-Controlled Tooling

## Status

Superseded

## Date

2026-04-30

## Provenance

`docs/ADR-0002-sandbox.md` was created in commit `ba042df` on 2026-04-30 as the
accepted ADR for sandboxing repo-controlled linter tooling. Commit `069b0ff`
corrected the stated linter count from approximately 13 to approximately 44; that
was non-decision cleanup. Later commits `b734b64` and `c13473f` superseded this
ADR through linter retirement and workspace rescope decisions rather than
mutating the original decision.

## Context

`auto_review` originally allowed repository-selected linters to run as part of
review. Because those linters and their configuration were controlled by the
reviewed repository, running them directly would let untrusted repository content
execute with reviewer privileges. The system needed a boundary that preserved
useful lint feedback while limiting filesystem, process, and network authority.

## Decision

Run bundled linters through `ar_sandbox`. Tests and local development use
`DirectSandbox` so behavior remains easy to exercise and debug. Production runs
use an OCI-backed sandbox to isolate repo-controlled tooling from the host and
from reviewer secrets.

## Consequences

The design made linter execution an explicit trust-boundary concern and
separated development ergonomics from production containment. It also added
operational and implementation complexity around sandbox setup, tool
availability, and environment parity between direct and OCI execution.

## Superseded by

ADR-0010 retires bundled linter execution from the normal semantic review
runtime. ADR-0011 replaces the remaining normal workspace access model with
hermetic Git and path-confined read-only access.
