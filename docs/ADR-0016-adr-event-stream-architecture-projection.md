# ADR-0016: ADR Event Stream and Architecture Projection

## Status

Accepted

## Date

2026-05-16

## Provenance

Created from the 2026-05-16 project decision to separate immutable ADR event
history from the current architecture projection.

## Context

The project uses Architecture Decision Records to capture significant technical
and process decisions. As the system evolves, previously accepted ADRs can become
partially outdated, superseded, or inconsistent with the current implementation.

Editing old ADRs to match the present architecture erases the rationale and
constraints that shaped earlier decisions. At the same time, operators and
contributors need a concise current architecture view that does not require
reconstructing the system from historical records.

Mechanical guardrails for enforcing this process will be updated separately, but
the process decision is made by this ADR.

## Decision

ADRs are immutable point-in-time decision events once they reach an accepted or
rejected state.

Proposed ADRs may continue to change while they are under discussion. After an
ADR is accepted or rejected, later decisions must be recorded in new ADRs rather
than by rewriting the old record.

Accepted or rejected ADRs may only receive status metadata updates that mark them
as superseded or partially superseded, plus a brief supersession note that points
to the later ADR. They must not be rewritten to reflect newer reasoning,
implementation details, or current architecture.

`docs/ARCHITECTURE.md` is the current architecture projection derived from the
ADR event stream. It should describe the system as it is intended to be
understood now, without preserving full rationale or historical decision context.
When useful, it may call out known legacy discrepancies between current
implementation and the accepted architecture direction.

## Consequences

- The ADR set becomes an append-only decision history after proposals are
  resolved, preserving the rationale and tradeoffs available at the time of each
  decision.
- Current architecture documentation has a separate home in
  `docs/ARCHITECTURE.md`, reducing pressure to rewrite historical ADRs for
  readability.
- Later changes that alter prior decisions require new ADRs, making the evolution
  of architecture explicit and reviewable.
- Existing tooling, review rules, and documentation checks may not yet enforce
  this distinction. Those mechanical guardrails will be updated separately.
