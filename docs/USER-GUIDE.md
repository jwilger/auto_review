# User Guide

Audience: developers whose PRs are reviewed by an `auto_review` bot
on a Forgejo instance. If you're operating the bot, see
[OPERATIONS.md](./OPERATIONS.md) instead.

## What the bot does

When you open or update a pull request, the bot:

1. Clones your branch at the head SHA into an isolated workspace.
2. Runs ~44 bundled linters across every changed file, scoped by
   language.
3. Sends the diff (plus linter findings, your repo's
   [`.auto_review.yaml`](#repo-config) guidelines, and any RAG
   context) to a reasoning-tier language model.
4. Verifies the model's findings against the actual code, dropping
   the ones the diff doesn't corroborate.
5. Posts a single review with inline comments and an overall
   summary, plus a [pre-merge checklist](#pre-merge-checks).

The status badge `auto_review` on the commit reflects the result:
**success** = review posted (zero or more findings), **failure** =
the bot couldn't complete the review (LLM unhealable, etc.),
**error** = transport-level failure (Forgejo / workspace).

The bot **does not auto-merge, auto-approve, or auto-close**. Every
suggestion is advisory.

## Reading inline comments

Each finding renders with a severity icon and label:

| Icon | Meaning |
|------|---------|
| 🔴 **Error** | High-confidence problem (e.g. SQL injection, hardcoded secret). The review is posted as **Request changes** when any finding is at this severity. |
| 🟡 **Warning** | Likely issue. Worth addressing but doesn't block. |
| 💡 **Note** | Stylistic or low-confidence observation. Take or leave. |

Multi-line ranges render as `**Lines N–M:**` because Forgejo's
inline-comment schema doesn't carry an end line.

## Pre-merge checks

The review body ends with a `## Pre-merge checks` checklist:

- `[x]` — passed
- `[ ]` — failed (advisory; doesn't block merging)
- `[~]` — skipped (didn't apply to this diff)

Three deterministic built-ins always run: **CHANGELOG updated**,
**Tests touched**, **No new TODO/FIXME comments**. Repos can add
custom natural-language checks via
[`pre_merge_checks:`](#repo-config) and the bot evaluates each
against the diff.

Failing a check asks for changes in the posted review, but it is still
a **nudge, not a merge gate**. auto_review does not enforce branch
protection; your Forgejo repository settings decide whether a
Request-changes review blocks merging.

## Talking to the bot

The bot listens for `@<bot_name>` mentions on the PR conversation.
The default name is `auto_review` but operators can rename it; see
the bot's account in your Forgejo instance for the exact handle.

| Command | What it does |
|---------|--------------|
| `@<bot> help` | List every available command. |
| `@<bot> re-review` | Re-run the full review against the current head SHA, even if it was already reviewed. |
| `@<bot> remember <text>` | Save a guideline to the repo's learnings store. Future reviews retrieve relevant entries via RAG. |
| `@<bot> forget <id>` | Delete a learnings entry. The bot lists ids in `help` output. |
| `@<bot> autofix` | Generate suggested patches for the bot's own findings as a comment. The bot does not push commits — you decide what to apply. |
| `@<bot> docstring` | Suggest docstrings for newly-added public APIs. Same: comment-only, you apply. |
| `@<bot> tests` | Suggest scaffolded test cases for added code. Same. |
| `@<bot> <freeform>` | Anything else gets routed to the cheap-tier model as a free-form Q&A about the PR. |

Mentions inside an inline review thread (replies to a finding) are
picked up by the bot's polling loop, typically within a minute.

## Disagreeing with a finding

The bot's findings are advisory. If you think a finding is wrong:

1. Reply to the inline thread explaining why. Other reviewers see
   your reasoning.
2. If the same false positive recurs across PRs, ask a maintainer
   to add a guideline to `.auto_review.yaml`:
   ```yaml
   guidelines: |
     `unsafe` blocks in src/foo/ are vetted; don't flag.
   ```
   The next review will see the rule. Or use
   `@<bot> remember the unsafe blocks in src/foo/ are vetted` to
   add it without editing the file.

There's no "this comment is wrong" button. The right escalation is
a guideline (durable) or a reply (per-PR).

Forgejo currently does not expose a PAT-authenticated REST endpoint
that marks inline review conversations resolved. Even when a finding
is fixed, the bot cannot press the UI's **Resolve conversation** button
for you; that action remains manual for a signed-in Forgejo user.

## Skipping the bot

For a single PR: open it as a **draft**. The bot's webhook handler
filters drafts. Convert to ready-for-review when you want the
review to fire.

For specific files: the repo's `.auto_review.yaml` `ignored_paths:`
list filters them out of the review entirely.

For the whole repo: the repo's `.auto_review.yaml` can set
`enabled: false` and the bot posts a `disabled by repo config`
status without reviewing.

## Repo config

`.auto_review.yaml` at the repo root configures the bot per-repo:

```yaml
# Top-level switch. False = the bot skips reviewing this repo.
enabled: true

# Free-form text injected into the LLM system prompt as
# "repository conventions". Use for project-specific rules.
guidelines: |
  We never use raw SQL — every query goes through QueryBuilder.
  Prefer immutable types; mutating helpers must be marked with
  `// MUTATES` for the reviewer.

# Path globs to skip reviewing entirely. Gitignore-flavored.
ignored_paths:
  - "vendor/**"
  - "src/generated/**"

# Linter names to disable (run `auto_review list-linters` to
# see the canonical names).
disabled_tools:
  - markdownlint

# Review behaviour. `full` (default) runs the LLM review with
# linter context. `linter_only` skips the LLM entirely and posts
# linter findings as inline comments — zero token cost.
mode: full

# Free-form English checks evaluated by the cheap LLM tier. Each
# one renders as a checklist item under the "Pre-merge checks"
# section of the review body. Skipped when no cheap-tier model
# is configured.
pre_merge_checks:
  - "All new public APIs have rustdoc comments"
  - "No raw SQL queries; everything goes through QueryBuilder"
```

Validate locally before committing:

```bash
auto_review validate-config .auto_review.yaml
```

A failing validation exits non-zero, so this fits cleanly in a
pre-commit hook. Add `--strict` to also reject unknown top-level
keys — that catches typos like `enabld:` (missing `e`) that the
permissive runtime loader silently ignores. Recommended for
pre-commit hooks where a typo would silently disable a setting:

```bash
auto_review validate-config --strict .auto_review.yaml
```

## What the bot can't do

- **Run code.** The sandbox prevents linter binaries from making
  network calls or writing outside the workspace, and the LLM
  doesn't have a shell tool. Findings are static-analysis +
  LLM-reasoning only.
- **Merge or approve.** No write capability beyond posting reviews
  and inline comments.
- **See secrets in your environment.** The bot reads only what's
  in your PR's diff and your repo's tracked files.

If you want the bot to run a *test suite* against your PR, the
operator needs to wire CI separately — auto_review is a code
reviewer, not a test runner.

## See also

- [README.md](../README.md) — project overview and architecture.
- [OPERATIONS.md](./OPERATIONS.md) — for the engineer running the
  bot.
- [THREAT-MODEL.md](./THREAT-MODEL.md) — what attacks the bot
  defends against (relevant if your repo accepts drive-by PRs).
