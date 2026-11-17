# Crate map

The workspace is split into small crates with narrow responsibilities. This file
is the central crate-level navigation aid; operator installation and deployment
instructions live in [Quickstart](./QUICKSTART.md) and [Deployment](./DEPLOYMENT.md).

| Crate | Responsibility |
|---|---|
| `ar-agentcore` | AWS Bedrock AgentCore-compatible HTTP runtime surface, including `/ping` health handling, provider-neutral `/invocations` payload parsing, handler-backed serving, in-memory and DynamoDB invocation idempotency, semantic-review dispatch, and stale-head rejection. |
| `ar-gateway` | HTTP server, Forgejo HMAC and GitHub `X-Hub-Signature-256` verification, Forgejo and GitHub App webhook intake, provider-routed CI-triggered dispatch, Forgejo chat poller, health/readiness/info/metrics routes. |
| `ar-orchestrator` | `JobDispatcher`, production `SpawningDispatcher`, in-memory/SQLite/DynamoDB review history, one pull-request fetch per job to resolve the base branch that repo config is read from, provider-neutral context diff, incremental compare-diff, changed-file triage, and status calls, and per-PR lifecycle observations for metrics. |
| `ar-forge` | Provider-neutral repository-host DTOs, host error type, `HostCapabilities`, and `ReviewHost` trait for clone URLs, PR summaries, diffs, reviews, issue comments, repo config reads, PR metadata, statuses, host capabilities, and the bot login, shared by Forgejo and GitHub adapters. See [Host capabilities](#host-capabilities) for the per-version matrix. Comment editing goes through `edit_issue_comment` and `edit_review_comment`, whose default implementations return a `HostError`. |
| `ar-forgejo` | Forgejo REST client, webhook DTOs, review/comment/status/webhook APIs, Basic-auth bootstrap client, server-version capability probe and bot-login lookup (see [Host capabilities](#host-capabilities)). `edit_issue_comment` and `edit_review_comment` both `PATCH issues/comments/{id}` (Forgejo v11+ edits inline code comments there too); a `204 No Content` reply means the comment was not edited and is an error. |
| `ar-github` | GitHub App REST client foundation, including app JWT minting, installation-token exchange/cache, `ReviewHost` wrapper, clone URL auth material, PR summary reads, PR and compare-diff reads, changed-file reads, top-level PR comment reads/posts, pull-review and review-comment reads, PR metadata updates, repo config file reads, pull request review posting, commit-status posting, `X-Hub-Signature-256` webhook signature verification, typed issue-comment and review-comment webhook payloads, and the long-lived `AppReviewHost` that signs fresh app JWTs, resolves per-repository installations, and mints repository-scoped installation tokens for the gateway. Both hosts report every capability and resolve the `<app slug>[bot]` login (see [Host capabilities](#host-capabilities)). Both `ReviewHost` implementations edit comments with `edit_issue_comment` (`PATCH issues/comments/{id}`) and `edit_review_comment` (`PATCH pulls/comments/{id}`). |
| `ar-llm` | Provider trait, OpenAI-compatible provider, and tier router for `Reasoning`, `Cheap`, and `Embedding`. |
| `ar-index` | Tree-sitter symbols, embeddings, SQLite/in-memory vector stores, and in-memory/SQLite/DynamoDB learnings store. `RepoScope` (lowercased `owner/repo`) scopes learnings: `LearningsStore::add` takes the scope and origin PR, `list` takes a filter (all, one repository, or unscoped), `remove` takes an optional scope (`None` for operator CLI removal), and `query_nearest` only returns learnings in the given scope. A stored repository value that does not parse as `owner/repo` is read as unscoped. |
| `ar-prompts` | Prompt templates, JSON schemas, DTOs, and validators for review and verification LLM calls; `fence_untrusted` and `fence_nonce` are the reusable helpers that wrap untrusted text in nonce-tagged `UNTRUSTED_DATA` markers (with marker neutralization, invisible-character stripping, CRLF normalization and a byte cap) for the review and metadata-check prompts today and for verifiers, chat and later features. |
| `ar-review` | Review activities: provider-supplied clone URL workspace prep, repo config, the trusted base-branch loader for repository-controlled instruction files (`trusted_files`), RAG context, prompt rendering, self-heal, verification, severity filtering, review mapping. |
| `ar-chat` | `@<bot>` parser and provider-neutral `ReviewHost` handlers for `help`, `remember`, `forget`, `re-review`, `autofix`, `tests`, and free-form Q&A. |
| `ar-cli` | The `auto-review` binary: gateway and AgentCore entrypoints plus auth, webhook, config, review, bench, ops, history, and learnings commands. |

## Runtime pipeline

```text
Forgejo or GitHub App webhook / CI trigger
  -> ar-gateway
  -> ar-orchestrator job dispatch (one dispatcher per provider)
  -> ar-review workspace/context/review/self-heal/filter/verify pipeline
  -> ReviewHost adapter (ar-forgejo or ar-github) review + commit status
```

LLM calls flow through `ar-llm::Router`. Review prompts and schemas come from
`ar-prompts`. RAG context, persistent learnings, and vector storage come from
`ar-index`. Provider-neutral host DTOs and the `ReviewHost` trait come from
`ar-forge`; Forgejo keeps compatibility DTO re-exports for existing callers
while host-neutral code migrates to the common crate.

## `ar-prompts` review contract

The review LLM call is constrained by `crates/ar-prompts/schemas/review.json`
(OpenAI strict mode: every property is listed in `required`) and parsed into
`ReviewOutput` by `validate_review_output`. Each `ReviewFinding` carries
`path`, `line_start`, `line_end`, `severity`, `message`, plus:

- `title`: a short headline, 8 words or fewer. Inline comments show it in bold
  after the severity label; `ar-review` trims it, collapses whitespace and
  control characters to single spaces, and caps it at 120 bytes before
  rendering.
- `category`: exactly one `FindingCategory` from a closed taxonomy, the single
  category set for metrics, noise controls, feedback suppression, fan-out and
  the bench. There is no catch-all value; an unknown or wrong-case category
  fails validation with `ValidationError::SchemaMismatch`, so
  `FindingCategory::as_str` is the only source of category labels.

| Category | Meaning |
|---|---|
| `correctness` | Wrong behaviour, logic errors, broken invariants. |
| `security` | Any security issue, even when it is also a correctness bug. |
| `reliability` | Error handling, panics, retries, resource leaks, races. |
| `performance` | Needless work, allocations, slow queries or algorithms. |
| `compatibility` | Breaking API, schema, config or platform changes. |
| `ci_coverage` | Missing CI linter or check (the CI coverage advisory). |
| `tests` | Missing, weak or incorrect tests. |
| `documentation` | Missing, stale or wrong docs and comments. |
| `refactor` | Structural suggestions such as extracting or merging code. |
| `naming` | Unclear or misleading names. |
| `style` | Formatting and stylistic nits. |

JSON that omits `title` or `category` still deserializes, with `title = ""` and
`category = correctness`, so older fixtures parse and nothing is accidentally
treated as a low-value category. Serialized findings always include both
fields. The review event and severity floor are derived from `severity` only;
`category` does not change them.

## Host capabilities

`ReviewHost::capabilities()` (in `ar-forge`) reports which optional
review-thread features the connected host supports, and
`ReviewHost::bot_login()` returns the bot's own login, cached per host
(ADR-0026). The trait defaults report no capabilities and an error for the
login. `ar-forgejo` probes `GET /api/v1/version` and caches a successful
probe per client (`ar_forgejo::capabilities::parse_major_version` reads the
leading major component after an optional `v`); a failed probe is retried on the
next call. It resolves the login through `GET /api/v1/user`. Both requests are
bounded by a 10-second timeout. `ar-github` reports every capability and
resolves `<app slug>[bot]` through `GET /app`, bounded by the same 10-second
timeout (`ar_github::IDENTITY_TIMEOUT`, overridable with
`with_identity_timeout`); a timed-out lookup is not cached. Each host type offers
`with_bot_login` to seed a known login, and hosts built by the gateway are
seeded with the startup bot identity.

| Host | multi-line comments | suggestion blocks | thread replies | resolve threads |
|---|---|---|---|---|
| Forgejo ≤15 (including legacy `1.x` numbering) | no | no | no | no |
| Forgejo 16, 17 | yes | no | no | no |
| Forgejo ≥18 | yes | yes | yes | yes |
| Forgejo, failed or unparseable version probe | no | no | no | no |
| GitHub | yes | yes | yes | yes |

## Test conventions

- Pure helpers get adjacent `#[cfg(test)] mod tests`.
- Forgejo HTTP paths use `wiremock`.
- LLM paths use canned/scripted providers.
- Security boundary claims are pinned by red-team tests referenced from
  [Threat Model](./THREAT-MODEL.md).
- Do not turn prose-only documentation changes into deterministic wording
  tests. Tests may read docs only when the document is generated, consumed by
  tooling, or acts as a public contract such as `docs/CLI.md`; explain that
  contract next to the test.
