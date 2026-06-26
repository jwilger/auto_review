# ADR-0021: Codex-Native Agent Workflow

## Status

Accepted

## Date

2026-06-25

## Context

The project previously kept its local agent workflow guardrails in another agent
runtime's project configuration. The team now wants Codex to be the only
maintained agent surface for repository work, including durable instructions,
specialist review agents, reusable skills, Forgejo workflow guidance, and
mechanical policy checks.

ADR-0017 established tool-governed ADR and architecture projection changes, but
its implementation details are tied to the retired local agent runtime. The
governance requirement still applies: ADR/projection mutations should go through
typed workflow helpers and policy tests instead of free-form direct edits.

## Decision

Use Codex-native repository configuration for local agent workflow support.

The maintained surface is:

- `AGENTS.md` for always-loaded repository guidance.
- `.codex/config.toml` for project Codex settings, Forgejo MCP, hooks, and
  subagent limits.
- `.codex/hooks.json` plus `scripts/codex/pre_tool_use.py` for lifecycle policy
  checks around unsafe commands and protected edit paths.
- `.codex/agents/*.toml` for specialist subagents.
- `.agents/skills/*/SKILL.md` for reusable procedures.
- `scripts/codex/rgr.py`, `scripts/codex/adr.py`, and
  `scripts/codex/forgejo.py` for deterministic project helper workflows.
- `tests/codex/` and `just codex-test` for the policy/config/helper regression
  suite.

The previous local agent configuration is removed rather than kept as a legacy
or compatibility surface.

## Consequences

Codex becomes the authoritative local agent integration for this repository.
Contributors no longer need to maintain duplicate agent instructions, plugins,
or CI gates.

The mechanical guardrails are not hidden inside agent-runtime-specific
TypeScript plugin hooks. They are ordinary repository scripts covered by tests
and invoked by Codex hooks where possible, so CI and code review can validate
the same policy surface that local Codex sessions use.

Codex hook behavior still depends on Codex trusting the project-local hook
definition. Contributors must review and trust changed hooks with `/hooks` in
Codex before relying on them locally.

## Supersedes

- docs/ADR-0017-tool-governed-adr-and-architecture-projection-workflow.md: keeps the ADR/projection governance requirement but replaces the retired runtime-specific implementation.
