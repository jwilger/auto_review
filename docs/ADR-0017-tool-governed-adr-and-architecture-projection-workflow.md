# ADR-0017: Tool-Governed ADR and Architecture Projection Workflow

## Status

Accepted

## Date

2026-05-16

## Context

ADR-0016 established ADRs as immutable point-in-time decision events and made docs/ARCHITECTURE.md the current architecture projection. That process still needs mechanical enforcement so future work does not accidentally rewrite accepted history, skip projection updates, or create inconsistent cross-links.

The project already uses opencode plugins for local workflow guardrails, plus release-tooling tests and nix flake checks as CI backstops. ADR files have a regular structure and a small state machine, making them suitable for typed workflow tools rather than free-form direct edits.

## Decision

Manage ADR and architecture-projection changes through project-local opencode ADR workflow tools.

The exposed tools are:

- `adr_create`, which always creates a Proposed ADR, allocates the next ADR number, renders the ADR from required typed section fields, and stores the proposed docs/ARCHITECTURE.md projection patch without applying it.
- `adr_update`, which only updates Proposed ADRs, accepts the same typed section fields plus a list of sections to rewrite, and stores or rewrites the proposed projection patch without applying it.
- `adr_accept` and `adr_reject`, which are separate state-transition tools from Proposed to Accepted or Rejected. Accepting an ADR applies the stored architecture projection patch, removes that proposed-patch section from the ADR, and applies any recorded supersession metadata to prior accepted ADRs.
- `adr_delete_unmerged`, which deletes only ADR files absent from main and cleans derived architecture or supersession references.

Normal edit, write, and apply-patch paths are blocked for `docs/ADR-*.md` and `docs/ARCHITECTURE.md` so ADR/projection mutations go through the typed workflow. CI and local release-tooling checks remain the independent backstop because opencode plugin guards cannot prevent external editor or non-opencode changes.

## Consequences

The ADR workflow becomes more explicit and harder to bypass during opencode-assisted work. ADR IDs, Proposed-only creation, state transitions, supersession metadata, and architecture projection coupling are handled mechanically instead of by convention.

Proposed ADRs can carry proposed architecture projection changes without making those changes current architecture before acceptance. If the projection has drifted by the time an ADR is accepted, the stored patch may need to be resolved against the current document before the transition completes.

The plugin becomes part of the architecture-governance surface and must be kept covered by focused release-tooling tests. Contributors who edit ADRs outside opencode still rely on CI and code review to catch process violations. Because opencode loads plugins at startup, users must restart opencode after plugin changes before the new tools and guards are active.
