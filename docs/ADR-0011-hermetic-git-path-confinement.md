# ADR-0011: Normal Workspace Access Uses Hermetic Git and Path-Confinement

## Status

Accepted

## Date

2026-05-03

## Provenance

Reconstructed from the documentation mutation in `c13473f` on 2026-05-03, with
red-team test references from `crates/ar-review/src/workspace.rs` for path
confinement and symlink escape behavior.

## Context

`auto_review` handles untrusted repository content while serving several
different execution paths: gateway webhook intake, CI-triggered review dispatch,
orchestrator clone and context construction, chat, and agentic verification.
These paths do not need the same filesystem capabilities.

The safest default is to avoid exposing a checkout at all unless a component
explicitly needs repository content. Where repository content is needed, access
must be constrained so repo-controlled paths cannot escape the intended workspace
through absolute paths, `..` traversal, symlinks, or oversized reads.

## Decision

Normal workspace access is split by capability:

- Gateway and CI endpoints do not read the checkout. They handle webhook
  payloads, chat commands, dispatch decisions, and status transitions without
  opening repository files.
- Orchestrator clone and context construction use hermetic Git operations and
  read-only extraction. Repository content is materialized through controlled
  clone/fetch paths rather than ambient shell access to arbitrary host paths.
- Agentic verification exposes only path-confined `read_file` and `search`
  capabilities. Both reject path traversal and symlink escapes, enforce
  repository-root confinement after canonicalization, and apply caps to prevent
  unbounded reads or searches.
- Future repo-controlled execution must be introduced only behind a
  feature-specific sandbox. The sandbox must define its own allowed filesystem,
  network, process, time, and output capabilities, and must fail closed when
  those controls cannot be established.

## Consequences

- Components that do not require repository content remain cheaper and safer
  because they never receive checkout access.
- Review and verification code must request repository data through explicit,
  constrained interfaces instead of assuming ambient filesystem access.
- Any new feature that executes repository-controlled code or tools cannot reuse
  the normal read-only workspace path. It requires a dedicated sandbox design,
  threat-model review, and red-team tests before being enabled.
- The confinement contract is intentionally conservative: refused reads are
  preferable to accidental host-path disclosure or execution outside the intended
  workspace.
