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

#### Linters (26 of CodeRabbit's ~45 set)

| Tool | Languages / files | Source-tool name |
|---|---|---|
| `gitleaks` | Any (secret detection across the tree) | `gitleaks` |
| `semgrep` | Multi-language SAST via `--config=auto` | `semgrep` |
| `trivy` | Vulnerabilities, misconfigs, secrets | `trivy` |
| `checkov` | Terraform / HCL infrastructure-as-code misconfigs | `checkov` |
| `osv-scanner` | Dependency CVEs against Google's OSV DB | `osv-scanner` |
| `ast-grep` | Custom AST-pattern rules (any tree-sitter language) | `ast-grep` |
| `ruff` | Python | `ruff` |
| `mypy` | Python type checker (complements ruff's lint surface) | `mypy` |
| `bandit` | Python security scanner (dynamic-code, weak crypto, …) | `bandit` |
| `eslint` | JS / JSX / TS / TSX / CJS / MJS | `eslint` |
| `biome` | JS / JSX / TS / TSX / CJS / MJS (rule-set distinct from eslint) | `biome` |
| `oxlint` | JS / JSX / TS / TSX / CJS / MJS (Rust rewrite of eslint) | `oxlint` |
| `golangci-lint` | Go (errcheck, govet, staticcheck, …) | `golangci-lint` |
| `rubocop` | Ruby (.rb / .rake / Gemfile / Rakefile) | `rubocop` |
| `phpstan` | PHP (.php / .phtml / .php3-7 / .phps) | `phpstan` |
| `swiftlint` | Swift (.swift) | `swiftlint` |
| `shellcheck` | Bash / sh | `shellcheck` |
| `hadolint` | Dockerfiles | `hadolint` |
| `markdownlint` | `*.md` / `*.markdown` | `markdownlint` |
| `vale` | Prose linter (grammar/voice/spelling) for `.md`/`.markdown` | `vale` |
| `sqlfluff` | SQL (.sql / .dml / .ddl, multi-dialect) | `sqlfluff` |
| `taplo` | TOML (Cargo.toml, pyproject.toml, …) | `taplo` |
| `dotenv-linter` | `.env` / `.env.*` files | `dotenv-linter` |
| `actionlint` | `.github/workflows/`, `.forgejo/workflows/`, `.gitea/workflows/` | `actionlint` |
| `yamllint` | `*.yml` / `*.yaml` (workflow + general) | `yamllint` |
| `kubeconform` | Kubernetes manifests (validates against k8s JSON schema) | `kubeconform` |

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

All RAG pieces are now wired end-to-end through
`build_review_context` → review prompt: the orchestrator's
`prepare_and_lint` step calls `build_review_context` against the
cloned workspace, embeds the diff, and injects top-K retrieved
symbols + co-changed files + matching learnings as a "Repository
context" section in the LLM prompt. The Embedding tier is
optional; absent it, RAG is skipped silently.

#### Verifier (Milestone 3 cost lever)

`ar-review::verify::verify_findings` calls the cheap-tier model
with the diff + candidate findings and asks for per-finding
keep/drop verdicts. Drops findings the verifier doesn't
corroborate. Falls open (returns input unchanged) when the cheap
tier isn't configured or the response is malformed — verifier
failures shouldn't silently drop real findings.

#### Sandbox (Milestone 3)

`ar-sandbox` ships a `Sandbox` trait + two implementations:

- `DirectSandbox` — spawns linters on the host. No isolation;
  used by tests and the local-dev gateway.
- `PodmanSandbox` — wraps every linter spawn in
  `podman run --rm --network=none --read-only --tmpfs /tmp:size=64m
  --security-opt=no-new-privileges --cap-drop=ALL --memory=… --cpus=…
  --pids-limit=… --user 65534:65534 -v <repo>:/work:ro -w /work …`,
  plus a tokio-based wall-clock timeout. Production gateways flip
  to this by setting `AR_SANDBOX_IMAGE` to a pre-pulled linter
  image (`deploy/Dockerfile.sandbox`).

All 12 linter runners go through the trait — none spawn
`tokio::process::Command` directly anymore.

#### Agentic chat (Milestone 4)

`ar-chat::ChatHandler` answers `@auto_review` mentions on PRs.
Five shapes:

- `help` — brief usage reply.
- `remember <text>` — persists a learning into the shared
  `LearningsStore` (visible to RAG retrieval on next review).
- `forget <id>` — removes a learning by id.
- `re-review` — dispatches a fresh `ReviewJob` with `force=true`
  so the per-PR history dedup is bypassed.
- `autofix` — posts inline `\`\`\`suggestion` patches for safe
  mechanical fixes (typos, dead code, off-by-ones); capped at 5.
- `docstring` — generates docstrings for newly-added items in the
  diff that lack them; same posting flow as `autofix`, capped at 5.
- `tests` — scaffolds unit tests for newly-added items that lack
  coverage; posted as a single markdown comment with copy-pasteable
  test cases (tests live in separate files, so no inline suggestion).
- Anything else — free-form question answered by the cheap-tier
  model with the PR diff (capped at 40 KiB) as context.

`issue_comment` webhook events are HMAC-verified, parsed for the
`@auto_review` prefix, and dispatched through the chat handler
on the same axum server as PR webhooks.

#### Persistent learnings (Milestone 5)

`SqliteLearningsStore` provides a drop-in `LearningsStore`
implementation backed by SQLite (sqlx with `runtime-tokio` +
`sqlite` features). Embeddings are stored as little-endian f32
byte slices; cosine similarity runs in-process over a full table
scan (fine for the 10s-1000s of rows a typical repo accumulates;
LanceDB ANN backing is queued for higher scale). The gateway
wires it via `AR_LEARNINGS_DB`; setting it switches from the
default in-memory store to the SQLite-backed one.

#### Deploy artefacts (Milestone 5)

- `deploy/Dockerfile`        — gateway image.
- `deploy/Dockerfile.sandbox` — minimal image with the 12 linter
  binaries baked in, intended target for `AR_SANDBOX_IMAGE`.
- `deploy/docker-compose.yml` — gateway + Postgres template,
  surfaces `AR_LEARNINGS_DB` and `AR_SANDBOX_IMAGE`.
- `deploy/helm/`             — Helm chart with the same env-var
  knobs (`config.learningsDb`, `config.sandboxImage`).
- `deploy/forgejo-action/`   — composite action wrapping
  `auto_review review-once` for in-CI mode.

### Pending (roadmap, per the feasibility study)

- LanceDB-backed `VectorStore` impl (the in-memory + SQLite
  paths cover correctness; LanceDB is the scale lever).
- youki-based `Sandbox` impl as a lighter alternative to the
  podman shell-out.
- Remaining ~19 linters from CodeRabbit's set
  (languagetool, terragrunt, detekt, prettier-check, buf, …).
- Real-world end-to-end verification on a live Forgejo + LLM;
  everything to date has been unit/integration-tested with
  wiremock + canned LLM providers.
- Red-team test suite for the sandbox: unit-level coverage now
  ships (`crates/ar-review/tests/red_team_workspace_tools.rs`,
  `crates/ar-sandbox/tests/red_team_argv.rs` — symlink escape,
  chained symlinks, regex-DoS, binary-file walk safety, shell-
  metachar argv passthrough, hardening-flag invariants).
  Live-podman container-escape harness with adversarial linter
  binaries is still pending; ADR-0002 calls it out as
  out-of-scope-for-unit-tests.

#### osv-scanner runner (13th bundled linter)

- `OsvScannerRunner` adds a second always-run dependency-CVE
  scanner alongside trivy. Trivy and OSV draw from different
  vulnerability feeds; running both surfaces CVEs that either
  DB has indexed first. Findings are surfaced at line 1 of
  the manifest with the OSV/GHSA/CVE id in the rule_id and
  presence-of-CVSS-as-severity heuristic.

#### Polling fallback for inline review-thread mentions (M4)

- Forgejo doesn't fire `pull_request_review_comment` webhooks
  reliably for thread replies (gitea#26023). The webhook path
  catches top-level PR comments via `issue_comment`; this poller
  fills the inline-thread gap.
- `ar_gateway::poller::ChatPoller` runs a background tokio task
  that, every `AR_POLL_INTERVAL_SECS` (default 60, set to 0 to
  disable), enumerates every PR in `ReviewHistory::list_known`,
  fetches its review comments via the new
  `Client::list_pr_review_comments`, and dispatches any new
  `@auto_review` mentions through the chat handler.
- Cursors are per-(repo, pr) highest-seen comment id (Forgejo's
  ids are monotonic). First poll per PR seeds the cursor at the
  current max id without dispatching, so backfill never replays
  history. Bot-authored comments are filtered by login to prevent
  self-reply loops. Per-PR errors don't abort the pass.
- Knobs: `AR_POLL_INTERVAL_SECS`, `AR_BOT_LOGIN`, `AR_BOT_NAME`.
- 5 wiremock-backed tests cover the seed/dispatch/skip-bot/error
  branches end-to-end.

#### Test-scaffolding chat command (M4 finishing-touches)

- `@auto_review tests` (or `test` / `unit-tests` / `scaffold-tests`)
  finds newly-added or substantially-modified items in the diff
  that lack test coverage and proposes one focused test case per
  item using the language's idiomatic framework (`#[test]` for
  Rust, `pytest`, `vitest`, `RSpec`, etc.). Capped at 5 scaffolds
  per command.
- Output shape diverges from autofix/docstring: tests usually
  live in a separate file, so we post a single issue comment with
  one fenced markdown section per scaffold rather than inline
  review-comment suggestions. The strict JSON-schema constraint is
  `{scaffolds: [{item_name, item_path, framework, source}]}`.
- Closes the third (and final) "finishing-touches" item from the
  plan's M4 surface — autofix, docstring, and unit-test
  scaffolding are now all wired.

#### Docstring-generation chat command (M4 finishing-touches)

- `@auto_review docstring` (or `docstrings` / `docs`) finds
  newly-added or modified functions, methods, classes, structs,
  and enums in the diff that lack a docstring and proposes them
  as inline `\`\`\`suggestion` patches via the same posting
  flow as `autofix`. Aliased to `docstrings` and `docs`.
- The replacement format prepends the docstring to the original
  signature line so Forgejo's "Apply suggestion" button inserts
  the docstring above the item.
- Refactored `handle_autofix` into a generic `handle_suggest`
  parameterised by a `SuggestionKind` (Autofix | Docstrings) so
  the two commands share the prompt-render → LLM → JSON-validate
  → review-comment-post flow and only differ in the system
  prompt + banner copy.

#### Autofix chat command (M4 finishing-touches)

- `@auto_review autofix` asks the cheap-tier model for at most 5
  high-confidence inline patches against the PR diff and posts
  each as a Forgejo review comment with a `\`\`\`suggestion`
  block (Forgejo renders these as one-click apply buttons).
- Strict JSON-schema constraint over `{patches: [{path, line,
  replacement, reason}]}` — malformed output gets a graceful
  "didn't return well-formed suggestions" reply rather than
  silently failing.
- Aliased as `auto-fix` and `fix` for ergonomics.
- Skips drafts; gives a placeholder reply when no Cheap tier is
  configured; replies "nothing safe to suggest" when the model
  returns an empty patch list. 5 wiremock-backed integration
  tests cover the keep/skip/error branches end-to-end.

#### Agentic verifier with workspace tools (M3/M4)

- `ar_review::workspace_tools` ships `read_file` and `search` as
  standalone functions: read-only file inspection bounded to the
  workspace root. Both reject `..` traversal, absolute paths, and
  symlinks pointing outside the workspace; `search` skips
  `.git`/`target`/`node_modules`/`vendor`/`__pycache__`/`.venv`/
  `dist`/`build`/`.next`/`.cache` during recursive walks.
- `ar_review::agentic_verify::verify_findings_agentic` runs a
  per-finding ReAct loop: cheap-tier LLM emits JSON tool calls
  (`read_file` / `search` / `verdict`) constrained by JSON schema;
  the loop executes the tool, feeds the result back as the next
  user message, and continues until the model emits a `verdict`
  or the turn budget (5) is exhausted.
- Fail-open at every error path: LLM failure, malformed JSON,
  unknown tool, turn-budget exhausted, tool execution error all
  keep the finding rather than dropping it. The orchestrator
  routes to the agentic verifier when `AR_AGENTIC_VERIFIER=1` is
  set in the gateway env; otherwise the single-pass
  `verify_findings` continues to run. The dispatcher keeps the
  cloned `PreparedWorkspace` alive past `review_pull_request` so
  the agentic loop can `read_file` and `search` against it.

#### bench subcommand (M5)

- `auto_review bench` replays one or more PR fixtures through
  the LLM-review path (prompt rendering → reasoning model →
  self-heal → optional verifier) and reports per-fixture
  findings counts and latency, plus an aggregate
  (successes/failures, totals, mean/median/p99 latency).
  `--json` emits the aggregate as a single line of JSON for
  piping into a regression dashboard. Two starter fixtures
  ship under `bench/fixtures/`.
