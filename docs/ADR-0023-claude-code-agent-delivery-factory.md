# ADR-0023: Claude Code Agent Delivery Factory

## Status

Partially superseded

## Superseded By

ADR-0028: the factory merges `factory` class changes itself once the maintainer has recorded approval on the pull request's head. The merge guard hook verifies that approval.

## Date

2026-11-16

## Context

ADR-0021 made Codex the maintained agent surface and named its parts:
`.codex/` (settings, hooks and subagents), `.agents/skills/`, the helper scripts
under `scripts/codex/`, and the `tests/codex/` suite run by `just codex-test`.
None of these exist in the repository any more, and `just codex-test` is no
longer a recipe. The `.forgejo/` directory, which held Forgejo Actions
workflows and the pull request template, has also been removed; CI runs on
GitHub Actions.

Contributor and agent documentation still pointed at the removed surface, so
instructions told readers to run tools and edit paths that cannot be found.
ADR-0021 also carried the ADR-0016/ADR-0017 governance requirement through
typed ADR workflow helpers (`scripts/codex/adr.py`), and those helpers are gone
with the rest of the Codex surface.

## Decision

The maintained agent surface for repository work is:

- `AGENTS.md` for always-loaded repository guidance.
- `.claude/` for the Claude Code agent delivery factory: specialist agents in
  `.claude/agents/`, path-scoped rules in `.claude/rules/`, the factory
  workflows in `.claude/workflows/`, hooks in `.claude/hooks/`, and project
  settings in `.claude/settings.json`.
- `factory/` for the factory's policy and loop definitions: `factory/policy.md`
  (authority, change classes and review routing), `factory/loop.md` and
  `factory/judge.md`.

`just ci` is the single verification gate, for people and agents alike. There
is no separate agent-policy test suite.

## Consequences

The ADR-0016 event-stream requirement still applies: ADRs are append-only, a
changed decision is a new ADR that supersedes the old one, the older record
gains only brief supersession metadata, and `docs/ARCHITECTURE.md` is updated
in the same change. With the typed ADR tools of ADR-0017 and ADR-0021 gone, this
is met by convention and review rather than by mechanical tooling:

- the `.claude/rules/adr-event-stream.md` rule, loaded for agents editing
  `docs/ADR-*.md` or `docs/ARCHITECTURE.md`;
- the architecture reviewer agent
  (`.claude/agents/factory-reviewer-architecture.md`) in the factory's review
  station;
- human review of `factory` class changes under `factory/policy.md`, which
  agents cannot merge themselves, so the rule and the reviewer above cannot be
  weakened without the maintainer.

Nothing blocks a direct edit to an accepted ADR body, so a violation is caught
in review, not before the edit. Documentation must not claim enforcement by a
test, hook or tool that does not exist in the tree.

## Supersedes

- docs/ADR-0021-codex-native-agent-workflow.md: the Codex surface it named was removed and the Claude Code agent delivery factory replaced it.
