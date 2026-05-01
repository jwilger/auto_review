# Changelog

All notable changes to `auto_review` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

The first non-pre-release will be `0.1.0`. Everything below is cumulative
since the start of the project.

### Added

#### End-to-end review pipeline

- **Webhook intake** (`ar-gateway`): axum HTTP server with constant-time
  HMAC-SHA256 verification (Forgejo's `X-Forgejo-Signature` and the legacy
  `X-Gitea-Signature` fallback). Filters PR actions to opened /
  synchronized / reopened / ready_for_review; drops drafts and unrelated
  events with 202 Accepted.
- **Forgejo client** (`ar-forgejo`): `Client::{get_pr_diff,
  list_changed_files, create_review, post_commit_status, create_webhook,
  get_server_version}`. Separate `InitClient` for HTTP-Basic-auth
  bootstrap operations (`create_access_token`).
- **LLM router** (`ar-llm`): `LlmProvider` trait + tier-based `Router`
  (Cheap / Reasoning / Embedding). `OpenAiProvider` implementation works
  against any OpenAI-compatible endpoint (hosted OpenAI, Ollama, vLLM,
  OpenRouter, Together, Groq) including `response_format=json_schema` for
  structured output.
- **Prompts + JSON schema** (`ar-prompts`): strict JSON Schema constraining
  the LLM to `{summary, walkthrough?, mermaid?, findings[]}`. System prompt
  anchors the JSON-only output contract. `validate_review_output` uses
  serde with `deny_unknown_fields` for schema-level validation.
- **Self-heal loop** (`ar-review::heal`): on validation failure, feeds the
  validator error back to the LLM and retries up to 3 attempts before
  reporting `ReviewError::Unhealable`.
- **Workspace clone** (`ar-review::workspace`): shallow-clones the repo at
  the PR's head SHA with `git clone --no-checkout --depth=1` + `git fetch
  --depth=1 origin <sha>` + `git checkout <sha>`. Auto-cleans via TempDir.
  `oauth2:<token>@` userinfo is scrubbed from any leaked git stderr.
- **Per-PR durable state machine** (`ar-orchestrator`): `JobDispatcher`
  trait (NoOpDispatcher for tests, SpawningDispatcher for production).
  `run_review_job` posts pending → final commit statuses around the
  triage → clone → lint → review → post sequence.

#### Linters (Milestone 1: 7 of CodeRabbit's ~45 set)

| Tool | Languages / files | Source-tool name |
|---|---|---|
| `gitleaks` | Any (secret detection across the tree) | `gitleaks` |
| `ruff` | Python | `ruff` |
| `eslint` | JS / JSX / TS / TSX / CJS / MJS | `eslint` |
| `shellcheck` | Bash / sh | `shellcheck` |
| `hadolint` | Dockerfiles | `hadolint` |
| `markdownlint` | `*.md` / `*.markdown` | `markdownlint` |
| `actionlint` | `.github/workflows/`, `.forgejo/workflows/`, `.gitea/workflows/` | `actionlint` |
| `yamllint` | `*.yml` / `*.yaml` (workflow + general) | `yamllint` |

`ar-tools::run_all` runs them in parallel; missing binaries are silently
skipped so a missing linter doesn't break the review.
`ar-review::routing::select_runners` decides which to run based on the
PR's changed file extensions.

#### Triage and cost control

- **Heuristic triage** (`ar-review::triage`): skips reviews where every
  changed file is a lockfile (Cargo.lock, package-lock.json, yarn.lock,
  pnpm-lock.yaml, poetry.lock, uv.lock, Gemfile.lock, go.sum,
  composer.lock, mix.lock, bun.lockb, Podfile.lock, flake.lock),
  generated path (`/generated/`, `*.pb.go`, `*.pb.rs`, `*.min.js`,
  `*.min.css`, `*.map`), or vendored path (`/vendor/`, `/node_modules/`,
  `/third_party/`). Posts a Success commit status with description
  "skipped (lockfile/vendored/generated only)" — no LLM call.
- **Diff cap** (`ar-review::diff::cap_diff`): bounds the unified diff at
  100 KiB before sending to the LLM. Splits at `diff --git ` file
  boundaries (line-start markers only, so hunk-content false matches are
  ignored), keeps whole files in order until the budget is exhausted, and
  appends "[auto_review: omitted N file(s)...]" so the model knows scope
  was reduced. UTF-8 char-boundary safe.

#### Repository configuration

- **`.auto_review.yaml`** (`ar-review::config`): per-repo customization
  loaded from the cloned workspace.
  - `enabled` (default `true`): master switch. When false, the bot
    posts a "disabled by repo config" success status and skips the
    review entirely.
  - `guidelines`: free-form markdown injected into the LLM prompt under
    "Repository guidelines (from .auto_review.yaml)" so the model
    treats project conventions as authoritative.
  - `ignored_paths`: gitignore-style glob patterns (via `globset`).
    Matching files are stripped from both the diff (per-file sections
    dropped) and the changed-files list before prompt rendering.
  - `disabled_tools`: linter `name()`s to skip; lets repos with their
    own CI lint pipeline avoid duplicate findings.

#### Walkthrough output

- The review JSON schema accepts optional `walkthrough` (longer markdown
  narrative) and `mermaid` (Mermaid diagram source) fields. The mapping
  layer renders them under `## Walkthrough` and inside a `\`\`\`mermaid`
  fence in the review body, falling through gracefully when omitted.

#### CLI

- **`auto_review init`** mints the bot user's first PAT via Basic auth
  (rpassword prompts if `--password` is omitted) and prints the
  one-time secret + suggested env-var line.
- **`auto_review register-webhook`** registers a `pull_request` +
  `issue_comment` webhook at `<gateway-url>/webhooks/forgejo` with the
  configured `WEBHOOK_SECRET`.

#### Operations endpoints

- `GET /healthz` — cheap liveness check.
- `GET /version` — JSON `{"name", "version"}` from `CARGO_PKG_VERSION`.
- `POST /webhooks/forgejo` — HMAC-verified PR/event intake.

#### Documentation

- `README.md`: project status, architecture summary, crate listing,
  license note (AGPL-3.0-or-later), acknowledgements (PR-Agent
  attribution).
- `QUICKSTART.md`: step-by-step deployment walkthrough, env-var
  reference, troubleshooting matrix.
- `docs/FEASIBILITY.md`: full feasibility study based on CodeRabbit
  reverse-engineering and Forgejo API capability mapping.
- `docs/ADR-0001-architecture.md`: architecture decision record.

#### Milestone 2 RAG groundwork

- **Tree-sitter symbol extraction** for Rust, Python, TypeScript, and
  TSX (with .js/.jsx/.cjs/.mjs routing to the TypeScript grammar
  since TS is a JS superset). `extract_symbols_for_path` dispatches
  by file extension and returns `Vec<Symbol>` records carrying kind,
  name, and 1-based inclusive line range.
- **Workspace walker** (`index_workspace`): walks a cloned repo,
  filters out `.git`, `target`, `node_modules`, `vendor`,
  `third_party`, `__pycache__`, `.venv`, `dist`, `build`, `.next`,
  `.cache`, files >1 MiB, and non-UTF-8 files; emits one
  `IndexedSymbol { path, symbol }` per definition.
- **Embedding pass** (`embed_symbols`): slices each symbol's source
  range, batches all snippets into a single `router.embed(...)` call,
  emits `EmbeddedSymbol` records ready for a vector store.
- **Vector store**: `VectorStore` trait + `InMemoryVectorStore`
  (cosine-similarity search, upsert-on-key). LanceDB swap-in is a
  follow-up.
- **Co-change graph** (`compute_co_change`): parses
  `git log --name-only` to build a `Map<path, Map<path, count>>` of
  files that change together. `co_changed_with(path, top_n)` returns
  the most-correlated files.
- **Learnings store**: `LearningsStore` trait +
  `InMemoryLearningsStore`. Records carry text, source (Chat /
  Guideline / Inferred), and an embedding; `query_nearest` does
  similarity search.
- **LLM triage**: `triage_files_with_llm` calls the cheap-tier model
  with a strict-JSON-schema prompt, returns per-file Trivial /
  Formatting / Doc / Simple / Complex classifications.
  `filter_reviewable` keeps only Simple/Complex (fail-open on
  unclassified).
- **Per-PR review history** (`ReviewHistory` trait +
  `InMemoryReviewHistory`): tracks the last SHA at which each PR
  was reviewed, enabling incremental review on subsequent commits.
- **Compare-diff API** (`Client::get_compare_diff`): fetches the
  unified diff between two SHAs/branches, the substrate the
  orchestrator will use to ask "what changed since the last review?"

These pieces are individually tested but not yet wired into
`review_pull_request` end-to-end. That integration is the next
substantial step.

### Pending (roadmap, per the feasibility study)

- Wire the RAG building blocks (index + vector store + learnings)
  into `review_pull_request` so the LLM prompt actually carries
  retrieved context and remembered guidance.
- Persistent backings for the in-memory stores: SQLite for review
  history, LanceDB for vectors and learnings.
- Incremental review wiring: read review_history before the diff
  fetch, switch to compare_diff when the previous SHA is known.
- OCI sandbox via youki for linter + LLM-issued shell execution
  (Milestone 3).
- Remaining linters from CodeRabbit's set (Milestone 3) — currently
  shipping 8: gitleaks, ruff, eslint, shellcheck, hadolint,
  markdownlint, actionlint, yamllint.
- Verification agent that double-checks each LLM finding against the
  cited code (Milestone 3).
- Agentic `@auto_review` chat handler with sandboxed tool use
  (Milestone 4).
- Finishing-touches autofix / docstring generation / unit-test
  scaffolding (Milestone 4).
- Forgejo Action packaging, Helm chart, quality benchmark suite, public
  release (Milestone 5).
- Real-world end-to-end verification on a live Forgejo + LLM is also
  pending; everything to date has been unit/integration-tested with
  wiremock + canned LLM providers.
