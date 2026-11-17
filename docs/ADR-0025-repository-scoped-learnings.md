# ADR-0025: Repository-Scoped Learnings

## Status

Accepted

## Date

2026-11-17

## Context

The learnings store had no repository column. One gateway or AgentCore runtime
serves many repositories, so a `remember` on a PR in one repository, or a review
correction recorded there, was retrieved into every other repository's review
prompt. Chat `forget <id>` looked up the id globally, so a commenter on a PR in
one repository could delete another repository's learning. The vector store has
the same cross-repository sharing for reviewed source snippets.

Review corrections worked around the missing column by prefixing their text with
"Repository `<owner>/<repo>` only.". That prefix only asked the model to ignore
the learning elsewhere; it did not keep the learning out of other prompts.

Comparable tools scope this state: CodeRabbit scopes learnings as local, global
or auto, and Cursor Bugbot's learned rules are per repository.

ADR-0022 gave each provider its own dispatcher over shared history, learnings,
vector and metrics stores, and noted that review history keys do not include
the provider.

## Decision

Scope learnings and vector-store rows by repository.

- `ar_index::RepoScope { owner, repo }` is the scope key. Both parts are
  lowercased and it is displayed as `owner/repo`. It has no provider component,
  so mirrored repositories with the same lowercased `owner/repo` on different
  forges share learnings and vector rows.
- Every new learning stores `repo` (the scope) and `origin_pr` (the PR it came
  from) in the in-memory, SQLite and DynamoDB stores. Review corrections no
  longer embed the "Repository X only." prefix.
- `LearningsStore::query_nearest` takes a scope and returns only learnings in
  it. Context building passes the reviewed repository's scope, and the
  dispatcher passes the job's owner and repo.
- Chat `remember`, review corrections and `forget` use the scope of the PR's
  repository. `forget` of an id in another scope replies "No learning with id N
  in this repository" and leaves the learning in place. `remove` without a
  scope is reserved for the operator CLI, and `list` takes a filter for all
  rows, one repository, or unscoped rows.
- Legacy rows: opening an existing SQLite store adds the `repo` and `origin_pr`
  columns idempotently and backfills `repo` for rows whose text starts with
  "Repository `<owner>/<repo>` only.". DynamoDB items without a `repo`
  attribute get their scope from the same prefix at read time. Rows with no
  recoverable scope stay unscoped and are excluded from retrieval; the gateway
  logs their count once at startup so an operator can review or remove them.
- Vector-store rows are keyed by the same `RepoScope`. That change is
  implemented separately (issue #40) and drops and rebuilds the cache table.
- PR-scoped learnings that apply only to their PR until it merges are deferred.
  `origin_pr` is stored now so that rule can be added without another
  migration.

## Consequences

- One repository's learnings and corrections no longer reach another
  repository's review prompts, and chat `forget` cannot delete another
  repository's learnings. Red-team tests in `ar-chat` and scope tests in
  `ar-index` and `ar-review` pin this (see THREAT-MODEL T6).
- Mirrored repositories share learnings by design. Two unrelated repositories
  with the same `owner/repo` name on different forges served by one gateway
  would also share them; operators should avoid that layout.
- Legacy learnings without the correction prefix stop influencing reviews until
  an operator handles them. This trades some lost guidance for removing the
  cross-repository leak.
- The scope key differs from findings and feedback vectors, which carry the
  provider. Review history keys stay without a provider, as ADR-0022 notes.
- Any commenter on a PR can still add learnings to that repository; gating
  `remember` and `forget` to approvers is a separate decision.
