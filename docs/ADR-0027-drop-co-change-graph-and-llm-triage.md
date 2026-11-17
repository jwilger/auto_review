# ADR-0027: Drop the Co-Change Graph and LLM Triage

## Status

Accepted

## Date

2026-11-17

## Context

ADR-0001 describes a hybrid review pipeline whose repository context includes
co-change data, and whose triage could use a cheap-tier model. Neither part ever
reached a review path.

- The co-change graph in `ar-index` could not produce useful data. The review
  workspace is a `--depth=1` clone (`crates/ar-review/src/workspace.rs`), so
  `git log` sees one commit and the graph has no edges.
- `compute_co_change` ran `git` directly, outside the hermetic git helpers that
  ADR-0011 requires for every git invocation.
- `format_repo_context` accepted co-change neighbours, but its only caller
  always passed an empty list.
- The LLM triage module in `ar-review`, with its prompt and JSON schema in
  `ar-prompts`, was exported but never called. Runtime triage has always been
  the deterministic path and extension heuristics.
- No review vendor publishes a measured quality gain from co-change signals.

Keeping the code cost maintenance and misled readers: the crate docs, README,
and architecture notes described both as if the pipeline used them.

## Decision

Drop co-change data and cheap-tier LLM triage from the hybrid pipeline described
in ADR-0001. Delete the `ar-index` co-change module, the `ar-review` LLM triage
module, and the `ar-prompts` triage prompt and schema. Runtime triage is
deterministic. Repository context draws on the diff, changed paths, tree-sitter
symbols, embeddings, and learnings, and does not use git history.

The cheap model tier stays; finding verification uses it.

## Consequences

- No runtime behaviour changes. The rendered repository context is identical
  because the co-change section was always empty.
- `ar-index` no longer exports `CoChangeGraph`, `compute_co_change`, or
  `parse_git_log_co_change`. `ar-review` no longer exports
  `triage_files_with_llm` or `filter_reviewable`. `ar-prompts` no longer exports
  the triage schema, prompt, or types.
- One fewer unconfined `git` call exists in the codebase.
- If history signals are wanted later, the path is a host-API "recent commits
  touching path" tool, which works with shallow clones and keeps git access on
  the host side. That change needs its own ADR.
- Design record: `docs/research/auto-review-feature-designs-2026.md`, theme
  CLEAN, Decision 17.
