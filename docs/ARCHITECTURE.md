# auto_review Architecture

This document is the current architecture projection derived from the ADR event
stream. It describes the architecture new and updated code should follow. It is
not a rationale log; historical context and supersession details live in the ADRs.

## System shape

`auto_review` is a single-tenant pull-request review bot implemented as a Rust
Cargo workspace of single-purpose crates. Forgejo and GitHub (through a GitHub
App) are the implemented repository hosts. The review pipeline depends on
provider-neutral host DTOs and a `ReviewHost` boundary rather than on either
forge's API types. It is not
a multi-tenant SaaS and does not currently target GitLab, Bitbucket, a web
dashboard, a web-based OAuth installer, or fine-tuned models.

The primary always-on runtime is a gateway service that receives Forgejo and
GitHub App traffic, handles chat commands, exposes operator endpoints, and
dispatches semantic review work. One gateway may serve Forgejo, GitHub, or both
(ADR-0022). The no-dedicated-server runtime path is
`auto-review agentcore serve`, an AWS Bedrock AgentCore-compatible HTTP surface
with `/ping` health checks and `/invocations` for CI-invoked semantic reviews.
The public operator executable is `auto-review`; the gateway service entrypoint
is `auto-review gateway`.

## Intake and dispatch

The gateway owns external request boundaries:

- Forgejo webhook requests pass through optional rate limiting, HMAC
  verification, bounded event classification, and bounded JSON failure handling.
- GitHub App webhook requests at `POST /webhooks/github` pass through the same
  rate limit, `X-Hub-Signature-256` verification with the App's webhook secret
  before parsing, and namespaced `X-GitHub-Delivery` dedup. `issue_comment` and
  `pull_request_review_comment` deliveries route `@<bot>` mentions to the chat
  handler; bot senders are ignored. A gateway without a Forgejo webhook secret
  answers `404` on `/webhooks/forgejo` rather than verifying with an empty key.
- `POST /reviews/ci` is the authenticated CI intake path. It is enabled only when
  `AR_CI_REVIEW_TOKEN` is configured and receives the owner, repo, PR number,
  head SHA, and trigger metadata for the CI-validated revision. An optional
  `provider` field (`forgejo` by default, or `github` with an optional
  `installation_id`) selects the repository host and its dispatcher.
- Forgejo chat commands are handled through webhook and polling paths because
  Forgejo does not reliably deliver every inline-thread reply event. GitHub
  delivers review-comment webhooks reliably, so the GitHub path has no poller.
- AgentCore `/invocations` accepts provider-neutral semantic-review and
  chat-command payloads. Semantic-review invocations fetch the current PR
  metadata through `ReviewHost`, reject stale head SHAs, claim an invocation
  idempotency key, and run a `ReviewJob` inline through the orchestrator before
  returning.
  Chat-command invocations parse `comment_body` with the same `@auto-review`
  parser as the gateway path and post replies through `ar-chat` over
  `ReviewHost`. Duplicate invocations with the same provider, kind, repository,
  PR number, head SHA, and comment identity return a structured duplicate
  response without re-running the review or chat command. The initial
  runtime startup wires Forgejo invocations from the existing
  `FORGEJO_BASE_URL`, `AR_FORGEJO_TOKEN`, `LLM_BASE_URL`, and
  `LLM_REASONING_MODEL` configuration names. GitHub startup wires
  `GITHUB_API_URL`, `GITHUB_APP_ID`, `GITHUB_APP_PRIVATE_KEY`, `LLM_BASE_URL`,
  and `LLM_REASONING_MODEL`; each GitHub invocation must include
  `installation_id` so the runtime can mint a repository-scoped installation
  token before repository-host operations.

Normal semantic review is gated behind repository-selected CI prerequisites.
Regular Forgejo webhooks perform low-cost intake, bookkeeping, status updates,
and chat routing; they do not normally start expensive semantic review. Explicit
chat commands such as `@auto-review re-review` may force a review.

## Review pipeline

Semantic review follows a staged hybrid pipeline rather than a single monolithic
LLM agent:

```text
clone workspace
  -> pull request diff: local merge base to head (host's diff as fallback)
  -> deterministic triage
  -> context curation
  -> review generation
  -> self-heal JSON/schema validation
  -> pre-verifier severity-floor filtering
  -> verification
  -> post-verifier severity-floor filtering + path guard
  -> optional PR metadata quality check
  -> inline review and commit-status posting
```

Runtime triage is deterministic only: trivial-PR and per-file classification use
path and extension heuristics, with no model call (ADR-0027).

Repository-host operations are represented by provider-neutral DTOs and the
`ReviewHost` trait in `ar-forge`. `ar-forgejo` is the current adapter and still
re-exports the common DTOs for compatibility, but new host-neutral review code
should depend on `ar-forge` types rather than Forgejo-owned DTOs.

Review-thread features are gated on probed host capabilities, not assumed
([ADR-0026](./ADR-0026-probe-host-capabilities-and-degrade-on-older-forgejo.md)). `ReviewHost::capabilities()` reports `HostCapabilities`
(multi-line comments, suggestion blocks, thread replies, resolve threads); the
trait default reports none, GitHub hosts report all four, and the Forgejo
adapter derives them from `GET /api/v1/version`, cached per client after a
successful probe: major ≥16 enables multi-line comments and ≥18 the other
three, while a failed or unparseable probe reports none (a failed probe is
retried on the next call). `ReviewHost::bot_login()` gives the pipeline
the bot's own login, cached per host (Forgejo `GET /api/v1/user`, GitHub
`<app slug>[bot]` from `GET /app`); gateway-built hosts are seeded with the
startup identity so `AR_BOT_LOGIN` still wins. Pipeline and orchestrator code
consume only these values. On Forgejo v11–v17 features degrade: suggestions
render as plain code blocks, and addressed findings are marked by editing the
original comment instead of replying and resolving.

GitHub support uses GitHub App authentication, not PATs. The `ar-github` client
foundation mints RS256 app JWTs from an app id and RSA private key, exchanges an
app JWT for scoped installation access tokens, caches fresh tokens by
installation and requested repository/permission scope, and uses installation
tokens for repository-scoped API calls. The implemented repository reads cover
GitHub pull request summaries, unified pull request diffs, and changed files for
PR triage and path guarding. GitHub refuses the whole-diff media type with
`406 too_large` for a pull request over 300 files or its line and byte limits;
the client then assembles the diff from the per-file `patch` fields of
`pulls/{n}/files`, keeping a header-only entry for files GitHub sends no patch
for (binary or too large), so callers without a workspace, such as the chat
commands, still get a diff. The first write path can create GitHub pull request
reviews, including inline review comments translated from the provider-neutral
review DTO, and post commit statuses for aggregate pass/fail reporting. GitHub
top-level pull request comment reads use the Issues comments API because GitHub
models pull request conversations as issue comments; top-level PR comment
posting uses the same endpoint. Repository config reads use the Contents API
with the raw media type so `.auto_review.yaml` can be loaded without cloning.
GitHub HTTPS clone auth material is built from the repository's `clone_url`
with installation-token credentials, because the REST API host is not the git
host. The gateway uses `AppReviewHost`, a long-lived GitHub App `ReviewHost`
that signs a fresh app JWT per app-level call, resolves (or accepts from webhook
deliveries) the installation covering each repository, and mints
repository-scoped installation tokens through the client's token cache. The
GitHub adapter translates provider-neutral reviews to GitHub's wire format:
`APPROVE`/`REQUEST_CHANGES`/`COMMENT` events, inline comments as `line` +
`side`, review states normalized to the shared vocabulary, 140-character status
descriptions, and a retry that folds inline comments into the review body when
GitHub rejects comments outside the diff.
GitHub can be wrapped as a `ReviewHost` with an installation token for the
implemented read and write operations, including PR summary fetches,
compare-diff reads for incremental review, pull-review and review-comment
listing for prior discussion context, and PR metadata updates for
override-marker maintenance. Commit-status posting and clone URL construction
are also part of the common host boundary, so the orchestrator can dispatch
GitHub AgentCore reviews through the same semantic review pipeline as Forgejo.

Deterministic linters, tests, and builds belong in CI before semantic review is
requested. The normal review runtime does not execute bundled linters, tests,
builds, generated code, or LLM-issued shell commands.

## Workspace and trust boundaries

Components receive only the workspace capability they need:

- Gateway webhook, chat, and CI-intake paths do not read the checkout.
- Workspace preparation uses hermetic Git subprocesses that ignore ambient host
  Git configuration, prompt helpers, templates, hooks, object/index overrides,
  and SSH command overrides. Host adapters supply an already-authenticated
  clone URL through `ReviewHost`; Forgejo keeps its existing token helper while
  GitHub uses installation-token auth material. Given the pull request's base
  SHA, workspace preparation also fetches a bounded amount of base and head
  history (in commits and in time) to find the merge base, so the
  pull-request diff and changed-file list can be computed locally instead of
  through a host's size-limited diff endpoint. That local diff reads
  `.gitattributes` from the merge base, never the head checkout, through
  git's `--attr-source` option, which git older than 2.42 rejects (the review
  then uses the host's diff; the gateway's startup git probe and
  `auto-review ops doctor` warn about such a git). `run_review_job` takes the
  base SHA from the `ReviewJob` when the intake supplied it, otherwise from
  the pull request it fetched, and full reviews use the local diff and
  changed-file list for both RAG context and review generation, so neither
  the host's whole-diff endpoint nor its changed-file list is read. Ignored
  paths and the diff byte cap still apply before the model sees it. Without a
  merge base the review logs why, with the repository and pull request,
  counts it in `auto_review_local_diff_fallbacks_total{reason}`, and uses the
  host's diff as before. Incremental reviews also fetch the previously
  reviewed commit into the workspace and diff only what was pushed since,
  locally first, then through the host's compare endpoint, then as a full
  review when the previous commit is gone.
- Context building and agentic verification use read-only, path-confined access
  under the clone root with symlink escape rejection and output/result caps.
- Agentic verifier tools are file read and search capabilities, not shell access.
- Repository-controlled instruction files are read from the PR's base branch,
  never from the head checkout (ADR-0024). `ar_review::trusted_files` is the
  single runtime loader: `read_trusted_repo_file` fetches a file through
  `ReviewHost::get_file_content` at the base branch tip by name, and
  `load_repo_config_from_base` tries `.auto_review.yaml`, then
  `.auto_review.yml`, returning the config with its provenance (path and ref)
  and an optional warning. A missing file, an oversized body, malformed YAML, or
  a non-404 fetch error falls back to default config and the review proceeds.
  `run_review_job` fetches the pull request once and passes a `BaseRef` (name
  and SHA) into workspace preparation and the review; the chat handler uses the
  same loader. Later ingestion of `AGENTS.md`, `CLAUDE.md`, `REVIEW.md`, or
  path instructions must use this loader or the base tree.
- Base-branch repository config overrides operator defaults such as
  `AR_SEVERITY_FLOOR` once a repository key exists for the same behaviour.
  Operator environment variables are defaults only and never cap a repository
  key.

Any future feature that executes repo-controlled code must introduce a
feature-specific sandbox design, fail-closed configuration, threat-model update,
and red-team tests before it is enabled.

## Persistence and repository context

The default single-tenant persistence model is embedded SQLite. SQLite-backed
stores persist review history, learnings, vector embeddings, and webhook delivery
deduplication state across restarts. In-memory stores remain appropriate for
tests and development paths where persistence is not the behavior under test.
AgentCore has an invocation idempotency trait seam with in-memory and DynamoDB
implementations. The DynamoDB implementation claims keys with a conditional
write and stores an `expires_at` epoch-seconds attribute suitable for DynamoDB
Time To Live. Review history also has a DynamoDB implementation for
cold-start-safe last-reviewed-SHA tracking, and learnings have a DynamoDB
implementation for remembered guidance across AgentCore cold starts.
`auto-review agentcore serve` defaults to in-memory idempotency and
review-history stores, uses the DynamoDB idempotency implementation when
`AGENTCORE_IDEMPOTENCY_DYNAMODB_TABLE` or `--idempotency-dynamodb-table` is
configured, and uses DynamoDB review history when
`AGENTCORE_HISTORY_DYNAMODB_TABLE` or `--history-dynamodb-table` is configured.
AgentCore uses DynamoDB learnings when `AGENTCORE_LEARNINGS_DYNAMODB_TABLE` or
`--learnings-dynamodb-table` is configured.

Learnings are scoped by repository (ADR-0025). Each learning stores a
`repo` scope, the lowercased `owner/repo` with no provider component, and the
`origin_pr` it came from. Context building retrieves only learnings in the
reviewed repository's scope; chat `remember`, review corrections and `forget`
use the scope of the PR's repository. Mirrored repositories with the same
lowercased `owner/repo` on different forges share learnings. Opening an older
SQLite store adds the columns and backfills the scope from the legacy
"Repository `<owner>/<repo>` only." text prefix; DynamoDB items without a `repo`
attribute get the same treatment at read time. Rows with no recoverable scope
are excluded from retrieval, and the gateway logs their count at startup.
Vector-store rows are not scoped yet: `SqliteVectorStore` is still shared
across repositories, and keying its rows by the same `RepoScope` is planned
under ADR-0025 in issue #40.

Repository context draws on the diff, changed paths, tree-sitter symbols,
embeddings when an embedding tier is configured, and repository-scoped
persistent learnings. Git-history signals are not used (ADR-0027). The review pipeline depends on the `VectorStore`
abstraction; `SqliteVectorStore` is the current persistent default. LanceDB or
another dedicated vector database should be revisited only when measured scale,
latency, or filtering requirements justify it.

## LLM routing

`ar-llm::Router` maps `ModelTier::{Reasoning, Cheap, Embedding}` to provider
implementations. The shipped provider surface is OpenAI-compatible and supports
hosted OpenAI-compatible APIs plus local or gateway-style backends such as
Ollama, vLLM, OpenRouter, Together, Groq, and similar endpoints.

## Observability

The gateway exposes separate operator routes with distinct semantics:

| Endpoint | Purpose |
| --- | --- |
| `GET /healthz` | Process liveness without downstream I/O. |
| `GET /readyz` | Readiness through configured runtime dependencies. |
| `GET /version` | Static deployment identity. |
| `GET /info` | Startup-time runtime configuration and posture snapshot. |
| `GET /metrics` | Prometheus text exposition. |

Review lifecycle metrics cross the crate boundary through `ReviewObserver` so the
orchestrator does not depend on the gateway's Prometheus implementation. Metric
names, dashboards, alert rules, and operator docs are a coupled contract.

## Development and CI tooling

`just` is the canonical command interface for routine development and CI checks.
Recipes such as formatting, clippy, tests, dependency policy checks, build checks,
and aggregate CI checks should call the underlying tools directly rather than
requiring `nix develop --command ...` inside the recipe.

Nix is an optional developer setup path and a supported CI provisioning mechanism.
Developers may use any environment that provides the required tools on `PATH`,
while `nix develop` provides the pinned tool environment used by the project. Nix
also remains responsible for production package assembly, embedded OCI/rootfs
runtime packaging, and NixOS module/service support.

Forgejo PR CI should run focused jobs built around the `just` recipes. CI may use
Nix to provision tools, but `nix flake check` is not the primary orchestration
interface for routine checks.

## Distribution and runtime isolation

The official out-of-the-box Linux distribution artifact is the signed
`auto-review` binary archive published through Forgejo releases. Release assets
must include checksums, signatures, signing material, SBOM/provenance metadata,
and verification instructions. Direct binary releases are temporarily Linux
`x86_64` only until a trusted Linux `aarch64` build and provenance path exists.

The project does not currently publish an official Docker/OCI image as a
first-class release artifact. Operators who want Docker or Podman images may
build their own image from the source or released binary.

For gateway execution from the official binary artifact, the architecture
requires embedded or linked OCI isolation by default. Bare-process gateway mode is
an explicit opt-out and must be reported clearly in startup logs, `/info`, and
diagnostic commands. If embedded OCI isolation cannot be established and the
operator has not opted out, startup fails closed.

Existing implementation in this area may be staged; new work should move toward
the binary-first, fail-closed embedded-OCI posture rather than expanding
bare-process gateway operation or reintroducing project-published image release
obligations.

## ADR event stream

Architecture decisions are recorded as ADR events under `docs/ADR-*.md`.
ADRs are append-only: accepted or rejected ADRs are immutable except for brief
supersession metadata. A changed decision is recorded as a new ADR that
supersedes the old one, the older record gets only a `Superseded` status and a
short `Superseded By` section, and this document is updated in the same change.
Per ADR-0023, enforcement comes from the `.claude/rules/adr-event-stream.md`
rule, the architecture reviewer agent and human review, not from mechanical
tooling.
