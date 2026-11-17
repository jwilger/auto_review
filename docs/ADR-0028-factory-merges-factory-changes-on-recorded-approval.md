# ADR-0028: Factory Merges Factory Changes on Recorded Maintainer Approval

## Status

Accepted

## Date

2026-11-17

## Context

ADR-0023 lists three things that hold the ADR-0016 event-stream rule in place
without mechanical tooling. The third is "human review of `factory` class
changes under `factory/policy.md`, which agents cannot merge themselves".

On 2026-11-17 the factory opened twenty pull requests that all waited for the
maintainer. Each was built on an older `main`, and landing them by hand took
hours of merging `main`, resolving conflicts and rerunning CI. To stop that
recurring, the factory now merges a pull request that waited for a person once
that person has approved it, including `factory`-class pull requests. Agents
therefore do run `gh pr merge` on `factory` pull requests, so ADR-0023's "agents
cannot merge themselves" no longer describes the system.

Agents act as the maintainer's own GitHub account. GitHub cannot tell an agent's
merge, label or review from the maintainer's, and it does not let an author
approve their own pull request. The guard has to tell the two apart instead.

## Decision

A `factory`-class change still needs the maintainer's approval, and the factory
may merge it only on an approval the maintainer recorded on the pull request:

- The merge is pinned to the pull request's current head with
  `gh pr merge <n> --squash --match-head-commit <sha>`, and is never scheduled
  with `--auto`, because a scheduled auto-merge outlives later pushes.
- The approval is either an approving review from a person under `## Roles` in
  `factory/policy.md` on a commit followed only by clean merges of `main`, or
  the `factory:approved` label applied by a Roles person while the branch's
  newest own commit, or a clean merge of `main` after it, was the pushed head.
  Roles are read from `main`, never from the pull request's branch.
- `.claude/hooks/factory-merge-guard.sh` checks all of this through `gh api` and
  `git` before the merge runs, and denies whenever it cannot read something.
- Agents may not add the `factory:approved` label or submit an approving review.
  The same hook blocks both, so an agent cannot approve its own change.

The `.claude/rules/adr-event-stream.md` rule and the architecture reviewer
agent stay as ADR-0023 describes them.

## Consequences

- The maintainer still decides every `factory`-class change, so the event-stream
  rule and its reviewer still cannot be weakened without the maintainer. The
  maintainer approves; the factory merges.
- Approval covers one head. A new own commit, a hand-resolved merge of `main`,
  or a label applied before the head was pushed does not count, so the maintainer
  approves again after any rework.
- The hook guards against mistakes and prompt injection. It is not a security
  boundary: anything running as the maintainer's account outside Claude Code can
  still merge. `FACTORY_MERGE_APPROVED=<pr>` stays reserved for the maintainer's
  explicit instruction within a session.

## Supersedes

- docs/ADR-0023-claude-code-agent-delivery-factory.md, in part: its third
  enforcement point, that agents cannot merge `factory` class changes
  themselves. The rest of ADR-0023 stands.
