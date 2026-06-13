# User Guide

Audience: developers whose PRs are reviewed by an `auto_review` bot
on a Forgejo instance. If you're operating the bot, see
[OPERATIONS.md](./OPERATIONS.md) instead.

## What the bot does

When you open or update a pull request, the bot accepts the Forgejo webhook but
waits for your project's CI workflow to trigger semantic review after its
configured prerequisites pass. Once review is triggered, the bot:

1. Clones your branch at the head SHA into an isolated workspace.
2. Gathers review context from the diff, changed-file list, repo
   guidelines, learnings, and indexed symbols. Deterministic
   linters/tests/builds are expected to run in CI before review.
3. Sends the diff (plus your repo's [`.auto_review.yaml`](#repo-config)
   guidelines and any RAG context) to a reasoning-tier language model.
4. Verifies the model's findings against the actual code, dropping
   the ones the diff doesn't corroborate.
5. Posts a single review with inline comments and an overall
   summary.

The status badge `auto_review` on the commit reflects the result:
**success** = review posted (zero or more findings), **failure** =
the bot couldn't complete the review (LLM unhealable, etc.),
**error** = transport-level failure (Forgejo / workspace).

The bot **does not auto-merge or auto-close**. Every suggestion is
advisory.

## Reading inline comments

Each finding renders with a severity icon and label:

| Icon | Meaning |
|------|---------|
| 🔴 **Error** | High-confidence problem (e.g. SQL injection, hardcoded secret). The review is posted as **Request changes** when any finding is at this severity. |
| 🟡 **Warning** | Likely issue. Worth addressing but doesn't block. |
| 💡 **Note** | Stylistic or low-confidence observation. Take or leave. |

Multi-line ranges render as `**Lines N–M:**` because Forgejo's
inline-comment schema doesn't carry an end line.

## Talking to the bot

The bot listens for `@<bot_name>` mentions on the PR conversation.
The default mention handle is `auto-review`; `auto_review` is still accepted as
a compatibility alias. Operators can rename the bot, so check the bot's account
in your Forgejo instance for the exact handle.

| Command | What it does |
|---------|--------------|
| `@<bot> help` | List every available command. |
| `@<bot> re-review` | Re-run the full review against the current head SHA, even if it was already reviewed. |
| `@<bot> remember <text>` | Save a guideline to the repo's learnings store. Future semantic retrieval requires an embedding tier (`LLM_EMBEDDING_MODEL`); otherwise the entry is stored but not retrieved by similarity search. |
| `@<bot> forget <id>` | Delete a learnings entry. The id is printed when `remember` stores the entry; operators can also audit ids with `auto-review learnings list`. |
| `@<bot> autofix` | Ask the cheap-tier model for diff-based inline patch suggestions. The bot does not push commits — you decide what to apply. |
| `@<bot> docstring` | Suggest docstrings for newly-added public APIs. Requires the cheap-tier model (`LLM_CHEAP_MODEL`). Same: comment-only, you apply. |
| `@<bot> tests` | Suggest scaffolded test cases for added code. Requires the cheap-tier model. Same. |
| `@<bot> <freeform>` | Anything else is read for intent. A question is answered by the cheap-tier model; an approval request is handled as below; a request to re-review re-runs the review. If the cheap tier is not configured, it falls back to a free-form Q&A reply. |

You don't have to phrase things a particular way. The bot reads the intent of a
comment addressed to it, so "no, that's fine for a release PR — please approve"
is understood as an approval request, and "take another look" as a re-review,
without special syntax.

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
    add it without editing the file. Semantic retrieval of remembered guidance
    in future reviews requires an embedding tier.

### Overriding the bot to force an approval

When the bot is **requesting changes** and you want it to approve anyway, tell
it so in a comment (e.g. "this is acceptable for a release PR, please approve").
For the bot to override its own outstanding findings, two conditions must hold:

1. **You must be authorized.** Your Forgejo login must be listed in the repo's
   `.auto_review.yaml` under `override_approvers`. This is opt-in — if the list
   is empty or absent, the bot declines and points you here. (A plain approval
   when the bot has *no* outstanding findings needs no authorization.)
2. **You must explain why.** The bot requires a substantive reason for the
   override; if you only say "approve it", it asks you to explain. The reason is
   recorded: the bot posts an approving review noting which findings are being
   overridden, and stamps an `[override-approved]` marker plus an
   "Approval override" section onto the PR title/body so the reason carries into
   the squash/merge commit. If a later push genuinely fixes the findings and the
   bot approves cleanly, the marker is removed automatically.

There's no "this comment is wrong" button. The right escalation is
a guideline (durable), an authorized override (per-PR, recorded), or a reply.

Forgejo currently does not expose a PAT-authenticated REST endpoint
that marks inline review conversations resolved. Even when a finding
is fixed, the bot cannot press the UI's **Resolve conversation** button
for you; that action remains manual for a signed-in Forgejo user.

## Skipping the bot

For a single PR: open it as a **draft**. Draft PRs are not reviewable. Convert to
ready-for-review when you want the PR to become eligible; your project's CI
workflow still triggers the normal semantic review after its configured
prerequisites pass.

For specific files: the repo's `.auto_review.yaml` `ignored_paths:`
list filters them out of the review entirely.

For the whole repo: the repo's `.auto_review.yaml` can set
`enabled: false` and the bot posts a `disabled by repo config`
status without reviewing.

## Repo config

`.auto_review.yaml` at the repo root configures the bot per-repo. The loader
also accepts `.auto_review.yml`; when both are present, `.auto_review.yaml`
takes precedence. Runtime loading caps the config file at 64 KiB, rejects
malformed YAML by falling back to defaults, and skips malformed ignored-path
globs. Use `--strict` locally to catch unknown keys before that permissive
runtime fallback hides a typo.

```yaml
# Top-level switch. False = the bot skips reviewing this repo.
enabled: true

# Qualitative PR metadata gate. False skips the Cheap-tier title/body check.
pr_metadata_check: true

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

# PR metadata quality gate. The legacy boolean form still works:
# `pr_metadata_check: true` or `false`.
pr_metadata_check:
  enabled: true
  checks:
    # Allow empty PR bodies without disabling title/custom metadata checks.
    body_required: true
  additional_rules:
    - "Security-sensitive changes must describe the threat model impact."
    - "Schema migrations must mention rollback risk."

# Forgejo logins allowed to force an approval over the bot's outstanding
# findings (see "Disagreeing with a finding"). Opt-in: when empty or omitted,
# nobody may override. Matching is case-insensitive.
override_approvers:
  - "your-maintainer-login"

```

Validate locally before committing:

```bash
auto-review config validate .auto_review.yaml
```

A failing validation exits non-zero, so this fits cleanly in a
pre-commit hook. Add `--strict` to also reject unknown keys — that
catches top-level typos like `enabld:` and nested metadata-control
typos like `pr_metadata_check.checks.body_requred` that the
permissive runtime loader silently ignores. Recommended for
pre-commit hooks where a typo would silently disable a setting:

```bash
auto-review config validate --strict .auto_review.yaml
```

## What the bot can't do

- **Run code.** The review runtime no longer executes bundled
  linters or repo-controlled tool configuration. Deterministic
  linters/tests/builds should run in CI before `auto_review` is
  triggered. Findings come from semantic LLM review plus verifier
  checks against the diff and read-only workspace context.
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
