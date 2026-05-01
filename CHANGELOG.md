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

#### Graceful shutdown

- `ar-gateway` now installs a `with_graceful_shutdown`
  handler that listens for SIGTERM (Unix) and SIGINT
  (Ctrl-C, cross-platform). On signal: the listener stops
  accepting new connections, in-flight HTTP responses
  drain cleanly, the process exits 0.
- Behaviour matches what `systemctl stop` operators
  expect — previously SIGTERM hard-killed mid-response.
- Spawned review tasks (the dispatcher's
  `tokio::spawn` work) are best-effort: if they're still
  running at SIGTERM, the runtime drops them when `main`
  returns. The threading required for cancellation
  tokens through every spawned activity is more
  machinery than the single-tenant deploy needs.
  Documented inline in `shutdown_signal()` so future
  contributors don't accidentally promise stronger
  semantics.
- `deploy/systemd/auto_review.service` gains
  `KillSignal=SIGTERM` (explicit; default but pinned)
  and `TimeoutStopSec=30s` so systemd waits for the
  graceful drain rather than escalating to SIGKILL at
  its default 90s timeout. `systemd-analyze verify`
  clean.

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
#### Webhook delivery dedup

- New in-memory LRU of recently-seen `X-Forgejo-Delivery`
  UUIDs. When Forgejo retries a delivery (transient
  network blip, gateway restart, etc.) the same UUID
  arrives twice; the dedup layer replies 200 OK on the
  retry without re-dispatching. Default capacity 256 IDs
  (covers thousands of seconds of typical traffic);
  configurable via `AR_DEDUP_CAPACITY` (set to 0 to
  disable).
- Placement: **after** HMAC verify, **before** event
  dispatch. Unsigned junk doesn't pollute the LRU; only
  authenticated retries land there.
- New counter `auto_review_webhook_duplicates_total`
  surfaces Forgejo's at-least-once-delivery behaviour.
- Falls through cleanly when the delivery header is
  absent (old Forgejo / custom webhook posters); the
  request proceeds as if dedup weren't configured.
- Implementation: `RecentDeliveries` in
  `ar_gateway::dedup` — `Mutex<(HashSet, VecDeque)>`
  keyed by string UUID. Insertion is O(1); eviction
  pops the front of the queue and removes from the
  set. 5 unit tests cover first-sight, duplicate,
  capacity eviction (with a 4-step LRU-correctness
  trace), zero-capacity defensive clamping, and
  multi-id no-clash.
- 2 webhook integration tests verify retry-no-redispatch
  and missing-header pass-through.

#### Webhook rate limiter (T7 mitigation)

- Optional global token-bucket throttle on the
  `/webhooks/forgejo` route. Off by default so existing
  deployments don't suddenly start shedding traffic;
  operators opt in by setting both `AR_WEBHOOK_RATE_PER_SEC`
  and `AR_WEBHOOK_BURST` env vars.
- The throttle runs **before** HMAC verification so a flood
  of unsigned junk can't burn CPU on signature math.
  Rejected requests get a `429 Too Many Requests` and
  increment `auto_review_webhook_rate_limited_total`.
- Pure-Rust token-bucket implementation in
  `ar_gateway::ratelimit::TokenBucket` (no external rate-
  limiter crate); test-only `try_take_at(now)` injection
  point keeps timing deterministic.
- New Prometheus alert `AutoReviewWebhookRateLimited` fires
  on `rate > 0.05/s` over 10m — annotation cross-references
  the signature-failures and pull-request counters so
  operators can distinguish legitimate-traffic-too-tight
  from active probing.
- THREAT-MODEL.md T7 updated: previously noted as "operator
  concern, out of scope for v1"; now documented as the
  default mitigation path with the residual risk being
  operators who choose not to opt in.
- 7 new tests: 5 in `ratelimit.rs` covering burst capacity,
  refill rate at 100ms granularity, capacity cap (refill
  doesn't overflow), zero-arg defensive clamping, and
  saturating-duration backwards-clock safety; 2 webhook
  integration tests verifying the 429 path AND that the
  throttle runs before HMAC verify (the second case fires
  an unsigned request twice — first goes 401, second goes
  429 because the token's spent).

- `GET /info` — runtime-config snapshot in JSON. Captured
  once at startup; surfaces which sandbox is active
  (`direct` vs `podman`), which learnings store is wired
  (`in-memory` vs `sqlite`), which LLM tiers have a
  provider configured (`reasoning` always; `cheap` and
  `embedding` opt-in), the configured reasoning model name,
  and whether the background poller and `/readyz` probe are
  enabled. Operators can confirm at runtime what the
  gateway thinks it's running without parsing logs;
  attached to issue reports it tells maintainers exactly
  which deployment shape generated the bug.
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

#### Persistent review history (Milestone 5)

- New SQLite-backed `SqliteReviewHistory` in
  `ar-orchestrator`. Set `AR_HISTORY_DB` to a filesystem
  path and the orchestrator's per-PR "last reviewed SHA"
  tracking survives `systemctl restart`. Without this
  (the previous default), every restart triggers a fresh
  full review on the next webhook for any open PR — wasted
  tokens + duplicated inline comments on lines that
  haven't changed.
- Schema is one row per PR keyed by
  `(owner, repo, pr_number)`. `record` is an UPSERT so
  retries don't duplicate rows. Same sqlx feature set
  as the existing `SqliteLearningsStore` (no new deps).
- `GatewayInfo` and `auto_review status` now expose
  `history: "sqlite"` vs `"in-memory"` so operators
  glance at status output and see whether their dedup
  state will survive restart.
- 8 unit tests cover unknown-PR returns None,
  record-then-lookup, UPSERT-replaces-without-row-leak,
  clear, clear-on-unknown-noop, distinct-PRs-isolation,
  list_known returns every recorded PR sorted, and a
  file-backed persists-across-handle-drops test that
  verifies actual disk persistence.

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

#### Bench baseline comparison (M5)

- `auto_review bench --baseline <FILE>` compares the current
  run against a previous `--json` aggregate. Prints deltas
  for success rate, precision, recall, mean and p99
  latency, and total findings — each with explicit sign so
  a regression is visually obvious. Cells where one side
  has data and the other doesn't (e.g. baseline had no
  labelled fixtures) render as `— (one side unlabelled)`
  rather than misleading 0-deltas.
- `--fail-on-regression` (requires `--baseline`) makes the
  command exit non-zero on a regression. Heuristic:
  precision or recall drop > 5 percentage points, OR p99
  latency jumps > 5 seconds. Designed to drop into CI on
  prompt-change PRs.
- `Aggregate` and `LabelScore` now derive `Deserialize` so
  the same JSON shape that `--json` emits round-trips
  through the loader.
- 8 unit tests cover the comparison logic: no-change,
  improvement, precision drop above and below the 5pp
  threshold, recall drop, p99 jump, unlabelled-baseline
  graceful degrade, and the formatters (signed-pp, signed-
  ms).

#### Expanded labelled bench corpus (M5)

- Four new labelled fixtures ship under `bench/fixtures/`:
  `labelled-command-injection` (Python `subprocess.run` with
  `shell=True` on user input), `labelled-hardcoded-secret`
  (committed Stripe live key with a "swap before deploy"
  comment), `labelled-path-traversal` (Flask filesystem read
  by request param), and `labelled-xss` (URL-controlled
  value flowing into a dynamic-HTML DOM sink). Together with
  the existing `labelled-sql-injection`, the labelled corpus
  now covers the five most common web-app vulnerability
  classes — enough breadth to exercise precision/recall
  scoring meaningfully across model + prompt revisions.
- A new contract test
  (`shipped_labelled_fixtures_parse_with_expected_findings`)
  parses every `bench/fixtures/labelled-*.json` and asserts:
  the file is valid `Fixture` JSON, the `expected` array is
  non-empty, and every expected `path` appears in the
  fixture's `changed_files` list. Catches schema drift and
  malformed fixtures at CI time.
- `bench/README.md` updated to enumerate the labelled set
  and document the contract test.

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

#### Severity floor: now runs BEFORE the verifier (M3 cost lever)

- The severity-floor filter introduced in an earlier
  iteration ran AFTER the verifier — meaning operators
  with `AR_SEVERITY_FLOOR=warning` were burning cheap-tier
  tokens verifying Note-level findings the post-filter
  would drop anyway. Reorder fixes that: filter runs after
  the reasoning model emits findings but before the
  verifier sees them. New regression test
  (`severity_floor_runs_before_verifier_to_save_cheap_tier_calls`)
  inspects the verifier's user-prompt and asserts dropped
  findings never reach it.
- Helper extraction: `apply_severity_floor` is a single
  function called both before the verifier (Full mode)
  and after the LLM/verifier path (LinterOnly mode, where
  the floor is the only filter). Idempotent in the Full
  case — second invocation post-verifier is a no-op since
  findings are already at-or-above the floor.

#### Severity floor (M3 signal-to-noise lever)

- New `AR_SEVERITY_FLOOR` gateway env var drops findings
  below an operator-configured severity threshold before
  posting. Values: `note` (default, post everything),
  `warning` (suppress note-only nits), `error` (only post
  Error-severity findings). Bot still generates and
  verifies the dropped findings, so metric counters and
  the latency histogram are unaffected — only the
  posted-comment volume changes.
- Plumbed through `ReviewArgs.min_severity`. Pipeline
  filters via `severity_rank` (Note < Warning < Error)
  after the verifier and before mapping. Logs `kept`
  and `dropped` counts so operators can confirm the
  floor is engaging on a per-review basis.
- Unrecognised env-var values fall through to `note`
  with a tracing warn — a typo doesn't accidentally
  suppress real findings. Catch typos at config-load
  time with `auto_review validate-config --strict`.
- 3 new tests: severity ordering, Warning-floor drops
  Note-only findings (verified via the pipeline's
  return value), Error-floor drops both Note and
  Warning.
- Documented in `OPERATIONS.md` §7.2.5 and the systemd
  `auto_review.env.example`.

#### Linter-only review mode (M3 cost lever)

- New `.auto_review.yaml` field `mode:` (default `full`,
  alternative `linter_only`). When set to `linter_only` the
  pipeline:
  - Still clones the workspace, runs the full bundled linter
    pipeline through the configured sandbox, and applies
    `disabled_tools` / `ignored_paths` filters.
  - **Skips** the LLM call (`generate_with_self_heal` and
    both verifier paths) entirely. Linter findings are
    mapped straight to inline review comments via the new
    `ar_review::linter_only::build_linter_only_output`.
  - **Skips** the verifier — there's no LLM output to drop;
    linter findings are trusted as-is. Repos that want noisy
    linters silenced should use `disabled_tools:`.
- Each comment is prefixed with `[<tool>]` or
  `[<tool>/<rule>]` so PR authors can see exactly which
  linter raised what. Severity (Note/Warning/Error) maps
  one-to-one between `ar_tools::Severity` and
  `ar_prompts::ReviewSeverity`.
- Useful for: repos that want centralized linter aggregation
  without LLM cost; teams trialing `auto_review` who want to
  start deterministic before opting into LLM review;
  monorepos where the LLM context budget is too tight for
  meaningful semantic review.
- 8 unit tests in `linter_only.rs` cover the mapping
  (severity, line ranges, prefix format, summary
  pluralisation). 3 config-parsing tests cover the new
  `mode:` field including invalid-value fallback.

#### Pre-merge checks (M4 finishing-touches)

- Three deterministic gates run alongside the LLM review and
  appear as a markdown checklist appended to the review body:
  - **CHANGELOG updated**: workspace has CHANGELOG.md AND
    non-trivial source changed AND CHANGELOG isn't in the diff
    → fail. Doc-only or lockfile-only diffs skip silently.
  - **Tests touched**: any source file changed but no test
    file is in the diff → fail. Recognises `*_test.*`,
    `test_*.*`, `*.test.*`, `*.spec.*`, and `tests/` /
    `__tests__/` / `spec/` directory conventions.
  - **No new TODO/FIXME comments**: scans added lines for
    `TODO`, `FIXME`, `XXX`, `HACK` markers (whole-word, so
    `todoist.com` doesn't trip it).
- Each check's status renders as a markdown checkbox
  (`[x]` pass, `[ ]` fail, `[~]` skip). Failing a check is
  **advisory** — it does not change the review event from
  `COMMENT` to `RequestChanges`. Repos with their own
  merge-gating CI keep using that; auto_review's pre-merge
  checks are nudges.
- `ar_review::pre_merge::evaluate` is the public entry
  point; pipeline wires it after the LLM verifier.
- Repo-author free-form checks (the second half of the M4
  spec) are now wired in too: list them under
  `pre_merge_checks:` in `.auto_review.yaml` and the cheap
  LLM tier evaluates each against the diff, returning
  `pass` / `fail` / `skip` with a one-sentence rationale.
  Schema-validated output (per
  `crates/ar-prompts/schemas/pre_merge_custom.json`) — any
  malformed response or length-mismatch degrades to
  empty-result rather than mis-aligning the rendered
  checklist. Skipped silently when the cheap tier is
  unconfigured (custom checks are advisory; the review
  still posts).
- Custom checks render under a `**Custom checks
  (`.auto_review.yaml`)`** sub-heading inside the same
  Pre-merge checks section so the built-in vs author-defined
  source is visually distinct.
- 18 new tests cover each check's pass/fail/skip paths plus
  the markdown renderer.

#### Supply-chain checks (M5 CI)

- `deny.toml` at the repo root configures `cargo deny check`:
  - **Advisories**: `yanked = "deny"`, `unmaintained = "workspace"`,
    `unsound = "warn"`, `notice = "warn"`. Every push is checked
    against the RUSTSEC advisory database.
  - **Licenses**: explicit allowlist tuned for an
    AGPL-3.0-or-later project (permissive: MIT / Apache-2.0 /
    BSD-* / ISC / 0BSD / Unlicense / CC0-1.0 / Zlib / Unicode /
    BSL-1.0 / OpenSSL; weak-copyleft: MPL-2.0; own:
    AGPL-3.0-or-later). Strong-copyleft GPL/LGPL deliberately
    not allowed even though they're AGPL-compatible — keeps
    relicensing options open.
  - **Bans**: `wildcards = "deny"` to prevent `*` version specs;
    `multiple-versions = "warn"` so dep duplication surfaces
    without blocking.
  - **Sources**: `unknown-registry = "deny"`, `unknown-git =
    "deny"`. Only crates.io is allow-listed; typo-squat
    registries can't sneak through.
- `.forgejo/workflows/ci.yml` gains a `supply-chain` job that
  installs `cargo-deny` and runs `cargo deny check` on every
  push. Drift here blocks the merge — a Forgejo bot with a
  write-scoped PAT can't accept dep tree drift.
- `CONTRIBUTING.md` updated with the local-run command for
  reproducing CI's check before bumping a dep.

#### systemd unit (M5 deploy)

- `deploy/systemd/auto_review.service`: hardened systemd
  unit for self-hosters who don't run k8s or docker. Pairs
  with the existing `helm/` and `docker-compose.yml` as a
  third deploy option. Includes the conservative-defaults
  hardening profile (`NoNewPrivileges`, `ProtectSystem=strict`,
  `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`,
  `CapabilityBoundingSet=`, `SystemCallFilter=@system-service`),
  burst-guard `StartLimit*`, and `RuntimeDirectory` /
  `StateDirectory` for the workspace tmpfs and learnings DB.
  `systemd-analyze verify` clean.
- `deploy/systemd/auto_review.env.example`: documented
  EnvironmentFile template covering every env var the
  gateway reads (FORGEJO_*, LLM_*, AR_*, WEBHOOK_SECRET).
  Mode 0600 on install since it carries credentials.
- `deploy/systemd/README.md`: install / hardening /
  drop-in customisation / upgrade / uninstall walkthrough.

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

#### Security policy (M5 docs)

- `SECURITY.md`: vulnerability disclosure policy at the
  repo root. Documents the reporting channel
  (`john@johnwilger.com`), pre-1.0 disclosure timeline norms
  (5-day ack, 14-day triage, 90-day fix-or-coordinate), and
  scope (in: this repo's crates and deploy artefacts; out:
  Forgejo / LLM providers / bundled linter upstreams /
  operator-controlled configuration). Cross-referenced from
  the README.

#### Project-tooling polish (M5)

- `.dockerignore`: keeps `target/`, `.git/`, editor state,
  and ephemeral test directories out of the docker build
  context. Without this, every `docker build` re-tars the
  whole workspace into the daemon — slow on cold builds,
  painful on hot rebuild loops. Filters `*.md` but
  whitelists `README.md` so the runtime image still ships
  with its top-level doc.
- `.forgejo/pull_request_template.md`: default PR
  description with sections for summary, type-of-change,
  verification (cargo test / clippy / fmt / deny),
  pre-merge checklist (CHANGELOG / rustdoc / red-team
  tests / metrics-rules contract), and Related links.
- `renovate.json`: dependency-update config for operators
  who run mend/renovate-runner alongside their Forgejo
  deploy. Groups tokio / serde / tree-sitter ecosystems
  so they bump in lockstep; holds `tower-http` and `sqlx`
  back from automatic majors (middleware-behaviour-
  sensitive); auto-merges dev-dependency patches once CI
  is green; tags vulnerability alerts with the
  `security` label.

#### QUICKSTART.md refresh (M5 docs)

- New §5a "Verify the deploy (recommended)" introduces the
  `doctor` + `test-webhook` + `status` diagnostic triad
  shipped over the M5 iterations. Operators following
  QUICKSTART now land on the verification commands as part
  of the linear walkthrough rather than discovering them
  by accident.
- Forgejo Action note updated: was "Not yet packaged"; now
  references `deploy/forgejo-action/` which actually ships.
- New "systemd" deployment-options subsection points at
  `deploy/systemd/`.
- Troubleshooting section restructured: doctor /
  test-webhook / explain-routing are the first stop, with
  the original "common failure modes" list as fall-back.
  Adds a closing pointer to the Prometheus rules pack +
  Grafana dashboard.
- Configuration reference expanded from 8 to 17 env vars
  covering everything shipped over the M5 iterations
  (AR_BOT_*, AR_HISTORY_DB, AR_LEARNINGS_DB,
  AR_SEVERITY_FLOOR, AR_WEBHOOK_RATE_PER_SEC,
  AR_DEDUP_CAPACITY, AR_READINESS_TTL_SECS,
  AR_POLL_INTERVAL_SECS, AR_SANDBOX_IMAGE,
  LLM_CHEAP_MODEL, LLM_EMBEDDING_MODEL).

#### CONTRIBUTING.md cookbook sections (M5 docs)

- Two new "How to add a..." cookbook sections complement
  the existing "Adding a new linter" recipe:
  - **Adding a new CLI subcommand** — five-step recipe
    (args struct → enum variant → handler →
    main.rs match arm → tests in both files), with the
    `AR_*` env-var convention spelled out and pointers to
    `wiremock` / `tempfile` for behavioural-test scaffolds.
  - **Adding a new chat command** — five-step recipe
    (enum variant → parser → handler branch → help-text
    update → parser + handler tests).
- Crate summary table updated for current truth: linter
  count is 44, chat-command count is 8, CLI is "init /
  register-webhook / review-once / bench / doctor / status
  / 16 more — see crate README" since enumerating every
  subcommand here would duplicate `ar-cli/README.md`.

#### Per-crate READMEs (M5 docs)

- Every workspace crate now has its own `README.md`
  (`crates/<name>/README.md`) documenting public surface,
  module breakdown, key tests, and dependencies.
  Complements ADR-0001's single-table summary with focused
  per-crate navigation; CONTRIBUTING.md cross-references
  the per-crate files.
- 11 READMEs total: `ar-gateway`, `ar-orchestrator`,
  `ar-review`, `ar-tools`, `ar-llm`, `ar-forgejo`,
  `ar-prompts`, `ar-sandbox`, `ar-chat`, `ar-index`,
  `ar-cli`. Each ~50-80 lines covering the same shape:
  what the crate does, public surface table, where the
  tests live, dependency notes.
- Cross-links to the relevant ADRs and threat-model entries
  when the crate implements a documented mitigation
  (e.g. `ar-sandbox` → ADR-0002 + T1, `ar-prompts` →
  T3 schema-allowlist tests, `ar-gateway` → ADR-0003).

#### ADR-0003 observability (M5 docs)

- `docs/ADR-0003-observability.md`: 8KB ADR documenting
  the design choices behind the runtime-introspection
  surface — why five distinct HTTP endpoints
  (`/healthz`/`/readyz`/`/version`/`/info`/`/metrics`)
  rather than one mega-status route, why
  `ReviewObserver` is a trait in `ar-orchestrator` (the
  dependency arrow stays gateway → orchestrator), why
  hand-rolled `AtomicU64` counters and not a metrics
  crate (small set + compile-time + minimal deps), why
  cumulative-bucket histogram bounds are tuned at
  1/5/15/30/60/120/300/600s for review work, and how the
  shipped Grafana / Prometheus artefacts stay in sync via
  CI contract tests. Covers consequences (positive AND
  negative — the metric set is hand-maintained; no
  Prometheus labels yet) and alternatives considered
  (OpenTelemetry, single status route, JSON metrics).
  Cross-references THREAT-MODEL, OPERATIONS, and the
  deploy artefacts. README and CONTRIBUTING link it.

#### User guide (M5 docs)

- `docs/USER-GUIDE.md`: documentation for PR authors whose
  changes are reviewed by an `auto_review` deployment. The
  audience the project's other docs didn't cover (README is
  for everyone, OPERATIONS is for ops, CONTRIBUTING is for
  contributors). Covers what the bot does on PR open, how
  to read inline comments and the pre-merge checklist, the
  full `@<bot>` chat-command surface (help / re-review /
  remember / forget / autofix / docstring / tests /
  free-form), how to disagree with a finding, how to skip
  the bot (draft, ignored_paths, enabled: false), and a
  worked `.auto_review.yaml` example. Linked from README.

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

#### Red-team integration tests (M3 verification)

- New `crates/ar-review/tests/red_team_pipeline.rs` makes the
  threat-model mitigations CI-enforced. Each test has a
  docstring naming the T# it covers, so the file doubles as
  a security-audit lens:
  - **T7** (oversized diff): 50 × 200 KiB diff capped at file
    boundaries, `omitted N` marker present.
  - **T8** (single-file giant diff): falls back to flat
    truncation rather than overflowing the LLM context.
  - **T9** (confused-deputy via Forgejo API): three tests —
    review JSON with unknown top-level fields rejected;
    review finding with unknown severity rejected; review
    event derived from finding severity, not LLM input.
  - **T3** (prompt injection): two schema-pinning tests — the
    review-output schema's top-level keys are an exact
    allow-list (`summary`, `walkthrough`, `mermaid`,
    `findings`) with `additionalProperties: false`; same for
    the verifier output (`verdicts` only).
- THREAT-MODEL.md gains a "Test coverage of these threats"
  section cross-referencing each T# to the test that pins it
  (this file plus the existing `red_team_workspace_tools.rs`
  for T4 and the HMAC unit tests for T2). Closes the §14 #3
  red-team-suite gap from the feasibility plan.

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

#### list-learnings + forget-learning subcommands (M5)

- `auto_review list-learnings --learnings-db <PATH>` (or
  reads `AR_LEARNINGS_DB`) prints every entry in the
  persistent learnings store. Output table shows id,
  source (`chat` / `guideline` / `inferred`), and a
  truncated text preview. `--json` emits NDJSON for
  piping into `jq`; the embedding vector is reduced to
  `embedding_dim` so the output stays human-scrollable.
- `auto_review forget-learning --id <N>` deletes one
  learning. Same effect as `@<bot> forget <id>` from a
  PR thread, but operates directly on the SQLite store
  so operators can script bulk wipes / migrations
  without going through Forgejo. Errors clearly when
  the id doesn't exist (no silent no-op so a typo is
  obvious).
- Operations runbook §8 (Learnings store) updated to use
  these commands as the primary admin surface, with the
  `sqlite3 …` recipe demoted to a fallback for
  custom queries.
- 7 new tests: 2 CLI parses, 3 list-learnings
  behavioural (table + ndjson + empty), 2
  forget-learning behavioural (drop existing record,
  unknown-id errors clearly).

#### Optional review concurrency cap

- New `AR_REVIEW_CONCURRENCY=N` env var puts a global ceiling
  on in-flight reviews. Without it (the default), a burst of
  N PRs spawns N tmpdirs + N concurrent LLM calls. On
  high-traffic instances or expensive cloud LLMs this can
  blow through cost limits or exhaust workspace disk.
- Implementation: `tokio::sync::Semaphore` inside the
  `SpawningDispatcher`. Acquired at the top of the spawned
  task, released when the review completes. Webhook handler
  still acks 202 immediately — excess tasks queue on the
  semaphore rather than getting dropped or failing.
- New `SpawningDispatcher::with_concurrency_limit(max)`
  builder. Defensive: clamps `max=0` to 1 so a typo doesn't
  permanently lock out reviews.
- 2 new tests pin behaviour: zero-clamp guard and the
  permit-count-drops-as-expected for `cap=2` (uses
  `available_permits()` rather than time-based assertions
  to avoid flakiness on shared CI).
- OPERATIONS.md §5.1.5 documents the tuning rule of thumb;
  `deploy/systemd/auto_review.env.example` documents the
  env var alongside other AR_* knobs.

#### Verifier-dropped findings counter

- New `auto_review_verifier_findings_dropped_total` counter
  exposed at `/metrics`. Tracks how many findings the cheap-
  tier verifier corrected away per review. Reasoning model
  emitted N → verifier kept (N - dropped). Sustained high
  drop ratios indicate the reasoning model is hallucinating;
  operators chart it as a quality signal and react with
  higher-quality models or prompt-hardening.
- `ReviewObservation::Succeeded` gains a `verifier_dropped:
  usize` field. Pipeline computes it as
  `pre_verify_count - output.findings.len()` after the
  verifier runs (which itself runs after the severity-floor,
  so the count reflects findings the verifier rejected — not
  findings filtered for being below severity threshold).
- LinterOnly mode reports `verifier_dropped: 0` since no
  verifier runs. The existing `verifier_dropped` field on
  `ReviewOutcome` mirrors the observation field for
  cross-crate visibility.
- New test
  (`verifier_dropped_counter_sums_across_reviews`)
  exercises two Succeeded observations + one Failed
  observation and asserts the counter sums correctly across
  successes (Failed doesn't carry the field).
- OPERATIONS.md daily-checks section documents the
  hallucination-rate alert formula:
  `rate(dropped[5m]) / (rate(sum[5m]) + rate(dropped[5m]))`
  above ~30% as the action threshold.

#### Severity breakdown in commit-status descriptions

- The bot's commit-status description used to read
  `auto_review: 5 findings` regardless of severity mix.
  Operators viewing GitHub-style PR-list pages saw only the
  flat count and had to click through to know whether they
  were five errors (block-the-merge) or five notes (style
  nits). Now reads:
  - `auto_review: no findings` (zero case unchanged)
  - `auto_review: 1 error`
  - `auto_review: 1 error, 2 warnings`
  - `auto_review: 1 error, 2 warnings, 3 notes`
- Order is error → warning → note (most-to-least operator-
  relevant). Zero buckets are omitted (a 1-error review
  doesn't render `1 error, 0 warnings, 0 notes`).
- Singular vs plural is correct (`1 error` not `1 errors`).
- `ReviewOutcome` gains `errors` / `warnings` / `notes`
  counters that sum to `findings_count`. Pipeline computes
  them after the verifier runs (so the post-verifier set is
  what's reported).
- 5 unit tests pin the format: zero, single-severity (×3),
  pluralisation, all-three combined, zero-bucket omission.

#### purge-history subcommand (M5)

- `auto_review purge-history --older-than-days N` drops
  review-history rows older than N days. Long-running
  deployments accumulate one row per PR ever reviewed;
  closed PRs don't need their `last_reviewed_sha` kept
  forever. Wire into a systemd timer / cron for periodic
  cleanup.
- `--dry-run` reports the current total row count + the
  cutoff timestamp without deleting, so operators can
  gauge volume on first run.
- New `SqliteReviewHistory::purge_older_than(cutoff)` —
  returns the deleted row count so the CLI prints exact
  numbers. Schema gains a
  `review_history_updated_at_idx` index on `updated_at`
  to keep purges fast.
- New `SqliteReviewHistory::record_at` — test helper that
  takes an explicit timestamp so tests can deterministically
  position rows before/after a cutoff. Doc comment marks it
  test-only; not gated under `#[cfg(test)]` so downstream
  crate tests can use it.
- 9 new tests: 4 on the SQLite store
  (cutoff drops older row, strict `<` keeps row at exact
  cutoff, keeps recent rows, empty table no-op), 2 CLI
  parse, 3 CLI behaviour (drops old, keeps recent, dry-run
  preserves rows).

#### reset-pr subcommand (M5)

- `auto_review reset-pr --history-db <PATH> --owner X
  --repo Y --pr N` clears the persistent review-history
  record for one PR. The next webhook on that PR triggers a
  fresh full review instead of a `compare` diff against a
  stale baseline SHA. Use cases:
  - After a guideline change in `.auto_review.yaml`
  - After swapping `LLM_REASONING_MODEL`
  - To recover from a botched review (operator wants the
    bot to start over)
- `--history-db` reads `AR_HISTORY_DB` by default — same env
  var the gateway uses, so operators sharing the env can run
  with no flags except the PR coordinates.
- Safe to run while the gateway is up — SQLite handles
  concurrent access. The gateway sees the cleared row on
  its next read; the orchestrator's next dispatch for that
  PR proceeds as if the SHA was never recorded.
- Idempotent: clearing an unknown PR succeeds silently
  (operators can script around it without `|| true`).
- 4 new tests: 1 CLI parse, 3 behavioural (clears existing
  record, succeeds on unknown PR, create-if-missing on
  fresh DB path).
- Documented in OPERATIONS.md §7.1.5.

#### status subcommand (M5)

- `auto_review status --gateway-url <URL>` pulls `/version`,
  `/info`, and `/metrics` from a running gateway and renders
  a one-screen operational summary: version, runtime config
  (sandbox kind / learnings store / poller / readiness),
  pipeline counters (jobs dispatched, succeeded, failed,
  skipped, success rate), webhook rejection counters
  (signature / payload / rate-limited), and poller cycles.
- `--json` emits the parsed result as a structured object
  for piping into a regression tracker or trend-line
  dashboard.
- The summary marks the `direct` sandbox as
  "NO ISOLATION — Kudelski-class RCE risk" so an operator
  glances at the status output and sees the production
  hardening gap immediately.
- Lightweight Prometheus-text-format parser
  (`parse_metric_counters`) handles the labelless counters
  the gateway emits and skips `# HELP` / `# TYPE` and
  histogram `{le="..."}` lines.
- Operations runbook §0 (pre-deploy validation) now lists
  `status` alongside `doctor` and `test-webhook` —
  doctor for outbound deps, test-webhook for intake,
  status for live state.

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

#### Forgejo client: paginate every list endpoint

- Continuation of the `list_changed_files` pagination fix.
  Two more list methods had the same single-page bug:
  - **`list_pr_review_comments`** — used by `ChatPoller` to
    scan inline review threads for `@<bot>` mentions. The
    docstring even acknowledged the cap (50 comments,
    "operators with very chatty PR threads can paginate
    later") but never paginated. A mention on comment #51
    of a chatty thread was silently invisible to the
    poller.
  - **`list_webhooks`** — used by `auto_review
    list-webhooks` and `unregister-webhook --match-url`.
    A repo with 50+ hooks audited only the first page.
- Both now loop through pages until a short response
  signals the last page, with a 100-page upper bound.
- Constants `PAGINATION_PAGE_SIZE` (50) and
  `PAGINATION_MAX_PAGES` (100) lifted from
  `list_changed_files` to module scope so all three
  methods share them.
- 2 new tests pin the pagination semantics on each newly-
  paginated method (53-row total via 50+3 pages for
  webhooks; 51-row total with the @<bot> mention on page
  2 for review comments — exactly the scenario the
  poller bug would hit).

#### Forgejo client: list_changed_files now paginates

- **Bug**: `Client::list_changed_files` issued a single
  unparametered GET against `/pulls/{n}/files`. Forgejo
  paginates this endpoint at 50 files/page by default, so
  any PR with more than 50 changed files silently returned
  only the first 50. Downstream the bot's routing and LLM
  context would see a partial file set and miss the rest;
  reviews would post comments only on the first 50 files.
- **Fix**: loop fetching `?page=N&limit=50` until a page
  returns fewer than `limit` rows. `MAX_PAGES = 100` caps
  the loop at a 5,000-file PR (an accidental commit
  would otherwise OOM on serialised JSON).
- 2 new tests pin the behaviour:
  - `list_changed_files_paginates_through_full_result_set`
    builds a 50-row page 1 + 7-row page 2 and asserts the
    final vec is 57 rows, with each page hit exactly once
    (`expect(1)` on both mocks).
  - `list_changed_files_short_first_page_terminates_loop`
    builds a 3-row page 1, mounts a page-2 mock with
    `expect(0)`, asserts the loop short-circuits on the
    short response.

#### list-webhooks + unregister-webhook subcommands (M5)

- `auto_review list-webhooks --owner <O> --repo <R>` audits
  every webhook installed on a repo. Output table shows id,
  active/inactive, type, events, and `config.url` (the
  secret is intentionally omitted — Forgejo returns it as
  `""` on read anyway). `--json` emits NDJSON for piping.
- `auto_review unregister-webhook --owner <O> --repo <R>` deletes
  a webhook by either `--id N` (single, exact) or
  `--match-url <substr>` (every webhook whose URL contains
  the substring; useful in deploy scripts that don't know
  ids ahead of time, e.g. `--match-url reviewer.example.com`
  to delete the bot's hook without touching unrelated ones).
  The two flags are mutually exclusive at the clap layer.
- Backed by new `Client::list_webhooks` and
  `Client::delete_webhook` in `ar-forgejo`. Forgejo's
  list-webhooks wire shape uses a nested `config` map; a new
  `WebhookSummary` flattens `config.url` up so callers don't
  need to reach through. 4 client tests + 2 CLI parse tests +
  5 end-to-end command tests cover the new surface.
- OPERATIONS.md §6.3 (rotate webhook secret) now uses
  `list-webhooks` / `unregister-webhook` / `register-webhook`
  as the canonical rotation flow instead of "find them
  yourself with curl".

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

#### explain-routing subcommand (M5)

- `auto_review explain-routing --file PATH...` shows which
  bundled linters would run on a given set of changed files.
  Pure routing — doesn't read the files or invoke any linter
  binary. Output is alphabetised so two invocations are
  diffable. `--json` emits `{"runners": [...]}` for piping.
- Use case: an operator on a Python+shell repo wants to
  know whether disabling `pylint` is enough to silence
  Python checks, or whether `ruff` / `bandit` / `mypy` also
  fire. Running `auto_review explain-routing --file src/x.py`
  returns the full routed set including all of them. Same
  technique works for tuning `disabled_tools:` pre-emptively
  before a PR ever fires.
- Two failure-mode tests cover empty file list and clap's
  `required = true` rejection. Three behaviour tests cover
  Python file routing, empty list, JSON output.
- USER-GUIDE.md and OPERATIONS.md "Disable a noisy linter"
  sections now point at `explain-routing` alongside
  `list-linters`.

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

#### validate-config --strict (M5)

- `auto_review validate-config --strict <paths>...` now
  rejects unknown top-level keys in `.auto_review.yaml`.
  The runtime loader stays permissive (forward-compat:
  a config written for a newer auto_review version
  shouldn't break older deploys), but the validator
  command is opt-in strict so pre-commit hooks catch
  silent typos.
- Concrete win: `enabld: true` (missing `e` in `enabled`)
  parses as the default value under the runtime loader,
  silently disabling the setting the operator thought
  they configured. `--strict` errors out with the typo'd
  key named and the valid-key list shown:
  ```
  ✗ .auto_review.yaml: unknown top-level key(s): enabld;
    valid keys are: enabled, guidelines, ignored_paths,
    disabled_tools, mode, pre_merge_checks
  ```
- New `parse_repo_config_strict` + `RepoConfigStrictError`
  in `ar-review`. A contract test
  (`strict_allowlist_matches_struct_fields`) round-trips a
  default `RepoConfig` through serde and asserts the
  serialised key set equals the strict allow-list, so
  adding a field without updating the allow-list fails CI.
- USER-GUIDE.md repo-config section recommends `--strict`
  for pre-commit hooks; CHANGELOG entry under M5.

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
