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

#### Linters (44 of CodeRabbit's ~45 set)

| Tool | Languages / files | Source-tool name |
|---|---|---|
| `gitleaks` | Any (secret detection across the tree) | `gitleaks` |
| `semgrep` | Multi-language SAST via `--config=auto` | `semgrep` |
| `trivy` | Vulnerabilities, misconfigs, secrets | `trivy` |
| `checkov` | Terraform / HCL infrastructure-as-code misconfigs | `checkov` |
| `tflint` | Terraform-specific lint + provider-plugin checks | `tflint` |
| `osv-scanner` | Dependency CVEs against Google's OSV DB | `osv-scanner` |
| `ast-grep` | Custom AST-pattern rules (any tree-sitter language) | `ast-grep` |
| `ruff` | Python | `ruff` |
| `mypy` | Python type checker (complements ruff's lint surface) | `mypy` |
| `bandit` | Python security scanner (dynamic-code, weak crypto, …) | `bandit` |
| `pylint` | Python lint (design checks, deeper semantics than ruff) | `pylint` |
| `eslint` | JS / JSX / TS / TSX / CJS / MJS | `eslint` |
| `biome` | JS / JSX / TS / TSX / CJS / MJS (rule-set distinct from eslint) | `biome` |
| `oxlint` | JS / JSX / TS / TSX / CJS / MJS (Rust rewrite of eslint) | `oxlint` |
| `stylelint` | CSS / SCSS / Sass / Less | `stylelint` |
| `htmlhint` | HTML (.html / .htm / .xhtml) | `htmlhint` |
| `prettier` | Format-drift across JS/TS/CSS/HTML/JSON/YAML/Markdown/GraphQL | `prettier` |
| `golangci-lint` | Go (errcheck, govet, staticcheck, …) | `golangci-lint` |
| `gosec` | Go security scanner (subprocess injection, weak crypto, …) | `gosec` |
| `staticcheck` | Go static analysis (deprecation, simplification, …) | `staticcheck` |
| `nilaway` | Go nil-pointer flow analysis (Uber) | `nilaway` |
| `rubocop` | Ruby (.rb / .rake / Gemfile / Rakefile) | `rubocop` |
| `phpstan` | PHP (.php / .phtml / .php3-7 / .phps) | `phpstan` |
| `swiftlint` | Swift (.swift) | `swiftlint` |
| `buf` | Protocol Buffers (.proto) | `buf` |
| `cppcheck` | C / C++ static analysis (.c/.cpp/.cc/.cxx/.h/.hpp/…) | `cppcheck` |
| `pmd` | Java static analysis (.java) | `pmd` |
| `ktlint` | Kotlin (.kt / .kts) | `ktlint` |
| `shellcheck` | Bash / sh | `shellcheck` |
| `shfmt` | Shell-script formatter (drift detection alongside shellcheck) | `shfmt` |
| `hadolint` | Dockerfiles | `hadolint` |
| `markdownlint` | `*.md` / `*.markdown` | `markdownlint` |
| `vale` | Prose linter (grammar/voice/spelling) for `.md`/`.markdown` | `vale` |
| `vint` | Vim script (.vim / vimrc / .vimrc / gvimrc / .gvimrc) | `vint` |
| `typos` | Source-tree typo finder (identifiers, comments, strings) | `typos` |
| `sqlfluff` | SQL (.sql / .dml / .ddl, multi-dialect) | `sqlfluff` |
| `taplo` | TOML (Cargo.toml, pyproject.toml, …) | `taplo` |
| `jsonlint` | JSON / JSONC syntax validation | `jsonlint` |
| `dotenv-linter` | `.env` / `.env.*` files | `dotenv-linter` |
| `actionlint` | `.github/workflows/`, `.forgejo/workflows/`, `.gitea/workflows/` | `actionlint` |
| `yamllint` | `*.yml` / `*.yaml` (workflow + general) | `yamllint` |
| `kubeconform` | Kubernetes manifests (validates against k8s JSON schema) | `kubeconform` |
| `ansible-lint` | Ansible playbook / role / task linting | `ansible-lint` |
| `helm` | `helm lint` against any chart with a touched Chart.yaml | `helm` |

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

- `GET /healthz` — cheap liveness check (process up).
- `GET /readyz` — readiness check that probes Forgejo
  reachability through the same client used by the chat
  handler and review pipeline. Returns `200 ok` (with the
  reported Forgejo version) when reachable, `503 Service
  Unavailable` otherwise. Backed by an async-Mutex-guarded
  TTL cache (default 10s, tuneable via `AR_READINESS_TTL_SECS`)
  so high-frequency k8s probes don't hammer Forgejo. Lets
  k8s deployments wire `livenessProbe: /healthz` separately
  from `readinessProbe: /readyz` — the Helm chart is updated
  to do that. When no probe is wired (single-pod systemd
  deploy), `/readyz` degrades safely to `/healthz` semantics.
- `GET /version` — JSON `{"name", "version"}` from `CARGO_PKG_VERSION`.
- `GET /metrics` — Prometheus-format counters spanning the
  webhook layer AND the review pipeline.
  - **Webhook layer:** webhooks bucketed by event
    (`pull_request`, `issue_comment`, `ping`, `other`),
    HMAC signature failures, malformed-payload failures,
    jobs dispatched, chat commands received, and chat
    commands dropped because `ChatDeps` was not wired in.
    Sustained increases in
    `auto_review_webhook_signature_failures_total` are the
    primary alerting signal for secret-rotation drift or
    active probing;
    `auto_review_webhook_payload_failures_total` is the
    signal for Forgejo version mismatch.
  - **Review pipeline:** `reviews_started_total`,
    `reviews_succeeded_total`, four
    `reviews_failed_<class>_total` counters (`forgejo`,
    `workspace`, `llm`, `unhealable`), three
    `reviews_skipped_<reason>_total` counters (`same_sha`,
    `trivial_files`, `disabled_by_config`),
    `review_duration_ms_sum` paired with
    `reviews_completed_count` for a rolling-average latency,
    `review_findings_sum` for charting bot output volume,
    and `review_duration_seconds` as a proper Prometheus
    histogram (8 cumulative buckets: 1s / 5s / 15s / 30s /
    60s / 120s / 300s / 600s plus `+Inf`) so SREs can compute
    `histogram_quantile(0.95, ...)` directly.
  - **Background poller:** `poll_cycles_total` ticks once
    per completed pass, paired with
    `poll_history_failures_total` (full-pass failures at
    the history-list step) and `poll_pr_failures_total`
    (per-PR failures within a pass; one PR's failure
    doesn't abort the pass). Mentions dispatched from the
    poller are tracked separately as
    `poll_mentions_dispatched_total` (disjoint from
    `chat_commands_received_total`, which is webhook-path
    only) and `poll_chat_failures_total` (chat-handler errors
    on poll dispatch). Until now the poller's progress was
    invisible to Prometheus — operators couldn't see whether
    inline-thread mentions were being picked up at all.

    Wired through a new `ReviewObserver` trait on
    `SpawningDispatcher`, so the dispatcher remains
    independent of the metrics format and the dependency
    arrow stays gateway → orchestrator.
  - No external metrics crate dependency — counters are
    `AtomicU64`s rendered to the text exposition format on
    scrape.
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
- Remaining ~1 linter from CodeRabbit's originally-cited set
  (languagetool — Java HTTP server, complex setup; deferred).
  At 44 bundled the project has covered nearly every concrete
  linter the feasibility study itemized.
- A larger labelled-corpus benchmark — the harness now scores
  precision/recall against `expected` labels, but the corpus
  itself is minimum-viable (one labelled fixture). Growing it
  past the ~50-PR threshold where precision/recall numbers
  become statistically meaningful is curation work, not
  autonomous-doable.
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
  piping into a regression dashboard. Three starter fixtures
  ship under `bench/fixtures/` (one labelled).
- **Labelled-corpus scoring**: fixtures with an `expected`
  array of `{path, line, note?}` entries get precision/recall
  scored against the model's findings, matched by
  `(path, line)`. The aggregate report adds a "Label scoring"
  section with matched/missed/spurious counts and
  precision/recall figures. Each expected entry is matched at
  most once (duplicate model findings at the same coordinate
  count as spurious), so the precision number penalises
  noise correctly.

#### Configurable bot identity in webhook chat path

- The webhook handler's chat-mention parsing and self-loop
  detection previously hardcoded `auto_review`, while the
  background poller already honoured `AR_BOT_LOGIN` /
  `AR_BOT_NAME`. Operators running the bot under a different
  Forgejo account (which is the recommended pattern, since
  `auto_review` is a project name not a username) got a
  silently-broken chat surface: `@<their-bot>` mentions
  weren't parsed, and the bot might re-act on its own
  comments. Both code paths now read the same env-vars and
  thread the values through `AppState::with_bot_identity`,
  so behaviour is consistent. Self-detection moves from a
  prefix match (`starts_with("auto_review")`) to an exact
  case-insensitive match against the configured login,
  which is correct (a user named `auto_review_helper` was
  previously silently ignored).

#### Grafana dashboard (M5 deploy)

- `deploy/grafana/auto_review.dashboard.json`: drop-in
  Grafana dashboard mapping every shipped counter and
  recording rule to a panel. Five rows: Pipeline funnel
  (success rate, p95 latency, throughput, cumulative
  findings), Review outcomes (stacked rate by class,
  p50/p95/p99 latency), Skipped reviews (informational),
  Webhook intake (event types, signature/payload
  rejections), Chat surface (webhook vs poller intake,
  poller cycle health). Includes a Prometheus data-source
  variable so it imports cleanly.
- `deploy/grafana/README.md`: import steps, layout
  reference, and a note that pairing it with the Prometheus
  rules pack is more efficient than letting the dashboard
  evaluate the same expressions inline.
- A new contract test
  (`shipped_grafana_dashboard_only_references_real_metrics`)
  parses the dashboard JSON, collects every
  `auto_review_*` / `auto_review:*` token, and asserts each
  one is either exposed by `/metrics` or defined as a
  recording rule in the Prometheus rules file. Drift between
  metric source and dashboard fails CI.

#### Prometheus rules pack (M5 deploy)

- `deploy/prometheus/auto_review.rules.yaml`: drop-in
  recording + alerting rules for the metrics surface.
  Four recording rules pre-compute review-completion rate,
  success ratio, combined chat-command rate, and review
  latency p95. Six alerting rules cover signature failures,
  payload-decode failures, success rate below SLO, poller
  stalled, review latency high, and per-class failure spikes
  (Forgejo-class and LLM-class). Each alert carries
  `service: auto_review` + `severity` labels for direct
  Alertmanager routing.
- `deploy/prometheus/README.md`: install snippet, tuning
  notes (which thresholds and `for:` durations to adjust for
  your traffic), and example Alertmanager routes.
- A new contract test in `ar_gateway::metrics` parses the
  rules YAML and asserts every `auto_review_*` metric the
  rules reference actually exists in `/metrics` output, so
  renaming a counter without updating the rules file fails
  CI.

#### Operations runbook (M5 docs)

- `docs/OPERATIONS.md`: day-2 operations runbook for the
  on-call engineer. Quick-reference symptom table mapped to
  diagnosis sections, daily/weekly health checks, webhook
  anomaly playbooks (signature + payload failures), review
  failure triage by error class, bot identity gotchas,
  resource pressure tuning, full rotation procedures
  (PAT / API key / webhook secret), repo-level operations
  (`.auto_review.yaml` patterns), learnings store backup /
  inspect / restore, upgrade procedure with rollback note,
  and an issue-filing checklist that lists exactly which
  artefacts to capture (and which secrets to redact).
  README and QUICKSTART link it; complements the threat model
  (which covers *what* the bot defends against) by covering
  *how* operators keep it healthy.

#### Threat model (M5 docs)

- `docs/THREAT-MODEL.md`: living document enumerating
  attacker profiles (drive-by PR, authenticated collaborator,
  compromised LLM provider, network attacker), trust
  boundaries, an asset inventory, and a threat catalogue
  (T1 sandbox-escape through T9 confused-deputy) with
  per-threat mitigation and residual-risk notes. Linked from
  the README so operators read it before exposing the bot to
  drive-by PRs. Includes guidance for keeping the document
  in sync as new components are added.

#### doctor subcommand (M5)

- `auto_review doctor` probes outbound dependencies and
  sanity-checks the webhook secret. Per-check pass / warn /
  fail / skip output; exit 0 only when every non-skipped
  check passes. Designed to drop into a deploy script
  before `register-webhook`.
- Checks:
  - **forgejo**: `GET /api/v1/version` for reachability,
    `GET /api/v1/user` to validate the bot PAT.
    Skipped without `--forgejo-url`.
  - **llm**: `GET <base>/v1/models` against any
    OpenAI-compatible endpoint (Ollama / vLLM / cloud).
    Reports model count from the response.
    Skipped without `--llm-base-url`.
  - **llm-reasoning-model / -cheap-model / -embedding-model**:
    when configured, `doctor` checks that each model name
    appears in the `/v1/models` response. Catches the
    common deploy failure where the env var is set to
    `qwen2.5-coder:32b` but only `qwen2.5-coder:7b` is
    pulled into Ollama. Reads the same env vars the
    gateway and `review-once` use
    (`LLM_REASONING_MODEL`, `LLM_CHEAP_MODEL`,
    `LLM_EMBEDDING_MODEL`).
  - **webhook-secret**: heuristic entropy check (length
    >= 16 + not all-digits + >= 8 distinct chars). Warns
    on weak secrets without failing — they're functional,
    just unrotatable since Forgejo doesn't expose the
    secret on read.
- Reads env vars (`FORGEJO_BASE_URL`, `FORGEJO_TOKEN`,
  `LLM_BASE_URL`, `LLM_API_KEY`, `WEBHOOK_SECRET`) so a
  configured deploy can run `auto_review doctor` with no
  args.

#### test-webhook subcommand (M5)

- `auto_review test-webhook --gateway-url <URL> --webhook-secret <S>`
  posts an HMAC-signed `ping` event to a running gateway and
  prints the response. Smoke-tests the webhook intake path
  (network reachability + signature secret + header forwarding
  through any reverse-proxy) without firing a real review.
  Exit 0 on 2xx; non-zero with a hint about secret + header
  stripping otherwise. `--event pull_request` substitutes a
  stub PR event for round-tripping the dispatcher path.
  Useful immediately after `register-webhook` to confirm the
  deploy works before waiting for an actual PR. Four
  end-to-end tests bring up an in-process gateway with
  `NoOpDispatcher` and verify success, wrong-secret failure,
  PR-event round-trip, and unsupported-event rejection.

#### list-linters subcommand (M5)

- `auto_review list-linters` prints the bundled linter
  catalogue with each entry's canonical name (the string
  to use under `disabled_tools:` in `.auto_review.yaml`),
  description, language tags, and homepage. `--language=<tag>`
  filters by language; `--json` emits one JSON object per
  line for piping into `jq`. Backed by a new
  `ar_tools::catalog::linter_catalogue()` returning a
  `&'static [LinterInfo]` slice. A contract test in
  `ar_review::routing` instantiates every routed runner
  through a synthetic comprehensive file set and asserts
  every name appears in the catalogue, so adding a new
  runner without updating the catalogue fails CI.

#### validate-config subcommand (M5)

- `auto_review validate-config <paths>...` parses one or more
  `.auto_review.yaml` files through the same code path the
  gateway uses (`ar_review::parse_repo_config`) and reports
  per-file results. A directory argument is scanned for the
  standard config filenames (`.auto_review.yaml` /
  `.auto_review.yml`); a file argument is taken as-is. Output
  is one `✓` line per valid file with key counts and one `✗`
  line per failure with line/column when serde_yaml provides
  it. Exits non-zero on any failure or when no files are
  found, so the subcommand fits cleanly in a pre-commit hook
  or CI step. Repo authors can iterate on `ignored_paths`,
  `disabled_tools`, and free-form `guidelines` locally
  without firing a real PR through the bot.
