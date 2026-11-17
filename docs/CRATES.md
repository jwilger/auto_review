# Crate map

The workspace is split into small crates with narrow responsibilities. This file
is the central crate-level navigation aid; operator installation and deployment
instructions live in [Quickstart](./QUICKSTART.md) and [Deployment](./DEPLOYMENT.md).

| Crate | Responsibility |
|---|---|
| `ar-agentcore` | AWS Bedrock AgentCore-compatible HTTP runtime surface, including `/ping` health handling, provider-neutral `/invocations` payload parsing, handler-backed serving, in-memory and DynamoDB invocation idempotency, semantic-review dispatch, and stale-head rejection. |
| `ar-gateway` | HTTP server, Forgejo HMAC and GitHub `X-Hub-Signature-256` verification, Forgejo and GitHub App webhook intake, provider-routed CI-triggered dispatch, Forgejo chat poller, health/readiness/info/metrics routes. |
| `ar-orchestrator` | `JobDispatcher`, production `SpawningDispatcher`, in-memory/SQLite/DynamoDB review history, provider-neutral context diff, incremental compare-diff, changed-file triage, and status calls, and per-PR lifecycle observations for metrics. |
| `ar-forge` | Provider-neutral repository-host DTOs, host error type, and `ReviewHost` trait for clone URLs, PR summaries, diffs, reviews, issue comments, repo config reads, PR metadata, and statuses shared by Forgejo and GitHub adapters. |
| `ar-forgejo` | Forgejo REST client, webhook DTOs, review/comment/status/webhook APIs, Basic-auth bootstrap client. |
| `ar-github` | GitHub App REST client foundation, including app JWT minting, installation-token exchange/cache, `ReviewHost` wrapper, clone URL auth material, PR summary reads, PR and compare-diff reads, changed-file reads, top-level PR comment reads/posts, pull-review and review-comment reads, PR metadata updates, repo config file reads, pull request review posting, commit-status posting, `X-Hub-Signature-256` webhook signature verification, typed issue-comment and review-comment webhook payloads, and the long-lived `AppReviewHost` that signs fresh app JWTs, resolves per-repository installations, and mints repository-scoped installation tokens for the gateway. |
| `ar-llm` | Provider trait, OpenAI-compatible provider, and tier router for `Reasoning`, `Cheap`, and `Embedding`. |
| `ar-index` | Tree-sitter symbols, embeddings, SQLite/in-memory vector stores, co-change graph utilities, and in-memory/SQLite/DynamoDB learnings store. |
| `ar-prompts` | Prompt templates, JSON schemas, DTOs, and validators for review, triage, and verification LLM calls. |
| `ar-review` | Review activities: provider-supplied clone URL workspace prep, repo config, RAG context, prompt rendering, self-heal, verification, severity filtering, review mapping. |
| `ar-chat` | `@<bot>` parser and provider-neutral `ReviewHost` handlers for `help`, `remember`, `forget`, `re-review`, `autofix`, `docstring`, `tests`, and free-form Q&A. |
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
