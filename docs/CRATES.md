# Crate map

The workspace is split into small crates with narrow responsibilities. This file
is the central crate-level navigation aid; operator installation and deployment
instructions live in [Quickstart](./QUICKSTART.md) and [Deployment](./DEPLOYMENT.md).

| Crate | Responsibility |
|---|---|
| `ar-gateway` | HTTP server, HMAC verification, webhook intake, CI-triggered dispatch, chat poller, health/readiness/info/metrics routes. |
| `ar-orchestrator` | `JobDispatcher`, production `SpawningDispatcher`, review history, per-PR lifecycle observations for metrics. |
| `ar-forgejo` | Forgejo REST client, webhook DTOs, review/comment/status/webhook APIs, Basic-auth bootstrap client. |
| `ar-llm` | Provider trait, OpenAI-compatible provider, and tier router for `Reasoning`, `Cheap`, and `Embedding`. |
| `ar-index` | Tree-sitter symbols, embeddings, co-change graph, and in-memory/SQLite learnings store. |
| `ar-prompts` | Prompt templates, JSON schemas, DTOs, and validators for review, triage, and verification LLM calls. |
| `ar-review` | Review activities: workspace prep, repo config, RAG context, prompt rendering, self-heal, verification, severity filtering, Forgejo review mapping. |
| `ar-chat` | `@<bot>` parser and handlers for `help`, `remember`, `forget`, `re-review`, `autofix`, `docstring`, `tests`, and free-form Q&A. |
| `ar-cli` | The `auto-review` binary: gateway entrypoint plus auth, webhook, config, review, bench, ops, history, and learnings commands. |

## Runtime pipeline

```text
Forgejo webhook / CI trigger
  -> ar-gateway
  -> ar-orchestrator job dispatch
  -> ar-review workspace/context/review/verify pipeline
  -> ar-forgejo review + commit status
```

LLM calls flow through `ar-llm::Router`. Review prompts and schemas come from
`ar-prompts`. RAG context and persistent learnings come from `ar-index`.

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
