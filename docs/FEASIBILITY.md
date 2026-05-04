# Feasibility Study: A CodeRabbit-Equivalent AI PR Reviewer for Forgejo

**Project working dir**: `/home/jwilger/projects/auto_review` (greenfield, empty)
**Target language**: Rust
**Deployment model**: Self-hosted single-tenant (one reviewer per Forgejo instance / org)
**LLM strategy**: Pluggable provider layer (local Ollama/vLLM + cloud OpenAI/Anthropic)
**Ambition**: Full functional parity with CodeRabbit (linters + RAG + learnings + sandbox + agentic verification + chat) — phased over multiple milestones

---

## 1. Context

Closed-source AI PR reviewers (CodeRabbit, Greptile, Cursor BugBot, Copilot review, Diamond) are dominant on GitHub but absent on Forgejo, which is the natural home for sovereignty-conscious / self-hosting / FOSS-aligned teams. Qodo's PR-Agent is the only credible OSS alternative; it has a Gitea provider but only the "single LLM call per tool" architecture and lacks the linter pipeline, sandboxed execution, persistent learnings, and agentic verification that make CodeRabbit feel different from a glorified `git diff | llm`.

This study scopes a Rust-implemented, self-hostable equivalent that targets Forgejo's API, supports both local and cloud LLMs, and is structured to phase up from a useful MVP to full agentic parity. The aim is a **production-quality reviewer that someone can `docker compose up` next to their Forgejo instance**, not a SaaS.

---

## 2. Reference Architecture (How CodeRabbit Actually Works)

Synthesized from CodeRabbit's own engineering blog, Google Cloud case study, LanceDB case study, OpenAI customer story, the SE-Daily interview with Harjot Gill (Jun 2025), the Kudelski RCE writeup (which inadvertently confirmed many internals), and third-party reverse-engineering. The system is a **hybrid pipeline + agentic** design — explicitly not pure ReAct loops, which CodeRabbit argues are too noisy and slow for CI.

**Pipeline (per PR)**:

1. **Webhook intake** → durable queue (Cloud Tasks). Cloud Run sandbox boots, clones the full repo, restores prior PR state if incremental.
2. **Triage**: cheap model (originally GPT-3.5-turbo, now Nemotron-class) classifies each diff hunk *trivial* vs *complex*; trivial files skip review entirely. ~50% cost saving vs single-model.
3. **Per-file summarization**: cheap model compresses each changed file's intent for downstream prompts.
4. **Static-analysis fan-out**: ~45 linters (full list in §7) run in parallel; structured findings feed into the review prompt.
5. **Context curation**: NOT pure RAG. Vector retrieval (LanceDB) over repo embeddings + ast-grep symbol queries + agent-issued shell commands (`grep`, `cat`, `curl`, etc., sandboxed) seed an exploration agent. Plus retrieval from the **Learnings** vector store (per-repo memory).
6. **Review generation**: reasoning model (o3 / o4-mini) emits line comments against a strict JSON schema.
7. **Verification agent**: separate LLM pass validates each finding (does the cited code actually do what we said?); failures fed back through a self-healing regenerate loop until JSON-schema-valid and verification-passing or N retries exhausted.
8. **Specialized parallel agents**: Walkthrough/Mermaid-diagram, Slop detection, CI-failure analyzer, Finishing-touches autofix.
9. **Incremental reviews**: subsequent commits diff against prior summaries; only new hunks re-process.
10. **Post results** as a PR review with inline comments + commit status + edited PR description.
11. **Agentic chat**: `@coderabbitai` mentions invoke a chat agent with full PR context + Learnings store + sandboxed shell tools.

**Critical infrastructure facts**:
- Vector DB: **LanceDB** (open-source, written in Rust, embedded — perfect fit for our stack).
- Sandbox: Cloud Run instances (8 vCPU / 32 GiB), 3600s timeout, ~200 instances at peak. Linters and LLM-issued bash run in this jail. *Kudelski exploited an unjailed Rubocop invocation to gain RCE + write access on ~1M repos.* **Sandboxing is non-negotiable.**
- Cost engineering: dual-model triage (~50% saving), summary-similarity caching for incremental commits (LLM-as-judge), strict per-PR token budgets.

---

## 3. Proposed Architecture (Rust, Self-Hosted)

Four loosely-coupled services, all in one Cargo workspace, deployable as one container or separated:

```
┌──────────────────────────────────────────────────────────────┐
│  Forgejo (customer's instance, with bot user + PAT)          │
└──────────┬─────────────────────────────────▲─────────────────┘
           │ webhooks (PR open/sync, mention) │ Reviews API,
           ▼                                   │ commit status
┌──────────────────────────────────────────────┴───────────────┐
│  GATEWAY  (axum HTTPS, HMAC verify, enqueue job)             │
└──────────┬───────────────────────────────────────────────────┘
           │
           ▼
┌──────────────────────────────────────────────────────────────┐
│  ORCHESTRATOR  (durable workflow per PR; fan-out/fan-in)     │
│   Triage → Summarize → Static-analysis fanout → Context curat│
│   → Review → Verify → Self-heal loop → Post                  │
└──┬───────────┬───────────────┬───────────────┬──────────────┘
   │           │               │               │
   ▼           ▼               ▼               ▼
┌──────┐ ┌──────────┐ ┌──────────────┐ ┌──────────────┐
│LLM   │ │INDEXER   │ │SANDBOX EXEC  │ │LEARNINGS     │
│Router│ │tree-sitter│ │OCI/youki +  │ │store         │
│      │ │+ LanceDB  │ │seccomp+     │ │(LanceDB tbl) │
│      │ │embeddings │ │netns         │ │              │
└──────┘ └──────────┘ └──────────────┘ └──────────────┘
```

### 3.1 Crate layout

```
auto_review/
├── Cargo.toml                  # workspace
├── crates/
│   ├── ar-gateway/             # axum HTTP, webhook intake, HMAC, queue producer
│   ├── ar-orchestrator/        # state machine; spawns workers; durable via SQLite/Postgres
│   ├── ar-forgejo/             # Forgejo API client (reqwest); diff/PR/review/comment/status
│   ├── ar-llm/                 # provider trait + impls (openai, anthropic, ollama, vllm)
│   ├── ar-index/               # tree-sitter parsers + LanceDB embeddings + co-change graph
│   ├── ar-tools/               # static-analysis tool runners + result parsers
│   ├── ar-sandbox/             # OCI sandbox launcher (youki) with seccomp, netns
│   ├── ar-prompts/             # prompt templates + JSON schemas (serde + jsonschema)
│   ├── ar-review/              # review-pipeline activities (triage, summarize, review, verify)
│   ├── ar-chat/                # agentic chat handler (@-mention webhook path)
│   └── ar-cli/                 # ops CLI: index, replay, debug, run-once-on-PR
├── deploy/
│   ├── docker-compose.yml      # gateway + orchestrator + Postgres + LanceDB volume + sandbox
│   ├── Dockerfile              # multi-stage; bundles ~45 linter binaries
│   └── forgejo-action/         # alternative packaging (action.yml + entrypoint)
└── docs/
```

### 3.2 Key Rust crate choices

| Concern | Crate | Why |
|---|---|---|
| HTTP server | `axum` + `tower` | Async, mature, great middleware story for HMAC verify |
| Forgejo client | hand-rolled on `reqwest` (the `forgejo-api` crate is incomplete on the Reviews endpoint) | Need exact control over the `comments[]` payload shape |
| Workflow durability | `sqlx` + Postgres + state-machine in code | Temporal Rust SDK is alpha; restate.dev is overkill for v1. Keep it simple: one `pr_run` row per PR with a `state` column; activities update rows transactionally |
| Diff parsing | `similar` (already used by `cargo` itself) | Robust hunk parsing, line numbering matches what we need to send to Forgejo |
| Tree-sitter | `tree-sitter` + per-language grammar crates | First-class Rust support; same lib CodeRabbit's stack uses |
| Vector store | **LanceDB** (`lancedb` crate) | Native Rust, embedded (no extra service), what CodeRabbit themselves use. Postgres+`pgvector` is the obvious alternative if we want one DB |
| LLM router | new internal trait + async-openai for OpenAI-shaped APIs (OpenAI, Ollama, vLLM, OpenRouter), `anthropic-rs` (or hand-rolled) for Claude | No Rust LiteLLM exists; the abstraction is small (~300 LoC). Ship a `LlmProvider` trait with `complete`, `complete_streaming`, `embed`, `tool_call` |
| Sandbox | `youki` (OCI runtime in Rust) + `caps`, `seccompiler`, network namespace via `nix` | Pure-Rust path. Fallback: shell out to `podman run --runtime=crun --network=none --read-only` if youki integration is heavy |
| Prompt templating | `minijinja` | Jinja-compatible; same templates as PR-Agent so we can borrow patterns |
| JSON schema validation | `jsonschema` + `serde_json` | Required for self-heal loop |
| Webhook HMAC | `hmac` + `sha2` | Forgejo signs with `X-Forgejo-Signature` (HMAC-SHA256) |
| Markdown render | `pulldown-cmark` | For walkthrough + comment formatting |
| Mermaid diagrams | emit text only (Forgejo renders Mermaid in markdown natively) | No render dep needed |

---

## 4. Forgejo Integration Layer (concrete API choices)

Verified by research agents against the Gitea/Forgejo OpenAPI spec:

- **Webhook intake**: `pull_request` event covers `opened`, `synchronized`, `reopened`. HMAC-SHA256 signature in `X-Forgejo-Signature`. Event in `X-Forgejo-Event`. *Known gap*: `pull_request_review_comment` events do **not** fire (gitea#26023) — agentic-chat threading must use polling fallback or `issue_comment` mention triggers.
- **Diff fetch**: `GET /repos/{owner}/{repo}/pulls/{n}.diff` returns the unified diff; `GET /pulls/{n}/files` returns the changed-files list with patch hunks; `GET /raw/{path}?ref={sha}` fetches whole files at any ref.
- **Posting reviews**: `POST /repos/{owner}/{repo}/pulls/{n}/reviews` with body `{ body, commit_id, event: "COMMENT"|"REQUEST_CHANGES"|"APPROVED", comments: [{path, body, old_position, new_position}] }`. *Note*: positions are line offsets, not GitHub's `line`+`side` schema. Multi-line range comments are partially supported (gitea#36231) — single-line is safe; multi-line we'll attempt and fall back.
- **Commit status**: `POST /repos/{owner}/{repo}/statuses/{sha}` for the aggregate pass/fail badge. **No Checks API** — all per-line findings flow through review comments.
- **Bot identity**: dedicated bot user + scoped PAT (`POST /users/{name}/tokens` with explicit scopes). No GitHub-App equivalent — onboarding is "create bot user → mint PAT → paste into config → register webhook." This is fine for self-hosted single-tenant.
- **Packaging alternative**: ship a `forgejo-action/` template using the auto-injected `FORGEJO_TOKEN` for users who want a no-server install. Same core binary; entrypoint just runs once and exits.
- **Rate limits**: undocumented in core; pagination via `Link` headers and `X-Total-Count`. Be defensive: respect 429s with exponential backoff, cache ETags where available.

---

## 5. Review Pipeline (state machine, per PR)

```
[INTAKE] -> [CLONE_REPO] -> [TRIAGE] -> [SUMMARIZE] -> [STATIC_ANALYSIS]
   -> [INDEX_DELTA] -> [CONTEXT_CURATE] -> [REVIEW_FANOUT]
   -> [VERIFY] -> [SELF_HEAL? loop]
   -> [WALKTHROUGH] -> [PRE_MERGE_CHECKS]
   -> [POST_REVIEW] -> [POST_STATUS] -> [DONE | FAILED]
```

Each transition is one DB-row update; activities are idempotent and re-runnable. On failure, the orchestrator records a structured error and posts a degraded review (so the PR author still sees *something*, even if only linter output).

**Triage rule (cheap-model prompt)**: classify each file as `{trivial, formatting, doc, simple, complex}`. Skip LLM review entirely for `trivial`/`formatting`; lighter prompt for `doc`/`simple`; full agentic review for `complex`.

**Self-heal loop**: validate review JSON against `schema/Review.json`. On failure: feed validation errors back into a regenerate prompt up to N=3 iterations, then degrade to the last syntactically-valid candidate.

**Verification activity**: for each line finding, ask a separate LLM to answer "Looking at the actual code at this line, is this finding correct? Provide evidence." Drop findings that fail. This is the single biggest quality lever vs PR-Agent.

---

## 6. RAG / Code Index

- **Tree-sitter parsing** on first index of a repo: extract symbols (defs, refs) per file, store in SQLite as a graph (file→symbol, symbol→{def_loc, ref_locs}).
- **Embeddings**: chunk by symbol (function/class/module), embed with a small local model (`bge-small-en-v1.5` via `fastembed-rs`, or remote API), store in **LanceDB**.
- **Co-change graph**: parse `git log --name-only` to compute file pairs that change together; surface as supplementary context ("you edited X; Y has co-changed with X 12 of last 20 times").
- **Incremental updates** on each new commit: only re-parse changed files.
- **Learnings store**: separate LanceDB table; each entry is `{repo_id, embedding, text, source: chat|guideline|inferred, created_at}`. Retrieved by semantic match against current diff + file path; top-K injected into the review prompt.
- **Configuration**: `.coderabbit.yaml`-style file (call ours `.auto_review.yaml`) for repo-level overrides — language preferences, custom guidelines, ignored paths, ast-grep rules.

---

## 7. Static-Analysis Tool Pipeline

Bundled binaries in the container image, run inside the sandbox. Initial set covers ~80% of repos:

- **Multi-language / security**: ast-grep, semgrep (or OpenGrep), trivy, gitleaks (or trufflehog), checkov, osv-scanner.
- **Per-language**: ruff (Python), eslint+oxlint+biome (JS/TS), golangci-lint (Go), clippy (Rust — already in repo CI usually), rubocop (Ruby), phpstan (PHP), shellcheck (bash), hadolint (Docker), actionlint (CI), markdownlint, yamllint, sqlfluff, languagetool (prose).

Each runner emits a normalized `Finding` struct (path, line range, severity, message, rule_id, source_tool). Findings are deduplicated against the LLM's own findings before posting.

**Sandbox is mandatory** for these (Kudelski lesson): no network, read-only repo mount except a writable workdir, seccomp-restricted syscalls, CPU/memory/wall-clock limits, no host paths. youki + a custom OCI bundle is the cleanest Rust-native path; podman+gVisor is the pragmatic fallback.

---

## 8. LLM Router

A small `LlmProvider` trait:

```rust
#[async_trait]
trait LlmProvider {
    async fn complete(&self, req: CompleteRequest) -> Result<CompleteResponse>;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn tier(&self) -> ModelTier; // Cheap | Reasoning | Embedding
}
```

Implementations: `OpenAi`, `Anthropic`, `Ollama` (OpenAI-compatible endpoint), `VLlm`, `OpenRouter`. Config selects which provider serves which tier. Defaults: cheap=`qwen2.5-coder:7b` via Ollama, reasoning=`qwen2.5-coder:32b` via Ollama. Cloud override profile available.

Token budgeting / chunking lives one layer up in `ar-review`: hunk-by-hunk for huge PRs, whole-file when it fits, with PR-Agent–style hunk-importance ranking when it doesn't.

---

## 9. Agentic Chat & Verification

The hardest layer. Two execution modes share the same `ar-sandbox` substrate:

- **Verification agent** (CI path): given a candidate finding, runs `grep`/`cat`/`ast-grep` in the sandbox to confirm or refute. Bounded turn budget (~5 tool calls).
- **Chat agent** (`@auto_review` mention): full conversation state stored per-thread; same sandboxed tools; can run tests/build commands if customer opts in via repo config.

Tool-calling is via the LLM provider's native function-calling API where available, with a normalized fallback (parsed JSON in fenced blocks) for providers that don't.

*Forgejo gotcha*: review-comment-reply webhooks don't fire reliably. We trigger on `issue_comment` events and on a periodic poll of open PRs the bot is mentioned in.

---

## 10. Major Risks & Open Questions

| Risk | Severity | Mitigation |
|---|---|---|
| Sandbox escape via linter or LLM-issued shell (Kudelski-class) | **Critical** | youki + seccomp + netns + read-only mounts + wall-clock kill; integration test with known-malicious inputs; document threat model |
| Local-LLM quality gap on reasoning step | High | Default reasoning tier to Sonnet/o3 in cloud profile; ship a "quality benchmark" CLI that runs a fixed PR corpus against the configured model and emits a score |
| Token cost on big PRs (cloud profile) | High | Triage skip + summary caching + per-PR token cap with degraded fallback |
| LanceDB embedded vs Postgres/pgvector | Medium | Start LanceDB (zero ops, what CodeRabbit uses); abstract behind a `VectorStore` trait so we can swap |
| No Forgejo App identity → no marketplace one-click install | Low (self-hosted only) | Ship an `auto_review init` CLI that creates the bot user, mints the PAT, and registers the webhook in one command |
| Multi-line review comments partially supported by Forgejo (gitea#36231) | Low | Detect via API capability probe; degrade to single-line citing range in body |
| Forgejo Reviews API quirks across versions | Low | Pin tested versions; capability matrix in docs |
| License compatibility of bundled linters | Medium | Most are MIT/Apache; semgrep is LGPL-2.1 (OK to ship as binary). Audit + document per-tool licensing in `THIRD_PARTY.md` |

---

## 11. Phased Build Plan

The user has greenlit the full-clone scope; phasing keeps each milestone shippable on its own.

### Milestone 0 — Workspace bootstrap (1 week)
- Cargo workspace, crate skeletons, CI (cargo check/clippy/test/fmt), Dockerfile multi-stage skeleton, README + ADR-0001 (architecture).

### Milestone 1 — MVP reviewer (3–5 weeks)
- `ar-gateway` accepts Forgejo `pull_request` webhook with HMAC verify.
- `ar-forgejo` fetches diff, posts review with inline comments via Reviews API.
- `ar-llm` with OpenAI + Ollama providers; single-pass review prompt with JSON-schema output and self-heal loop.
- 5 bundled linters running outside the sandbox (just for MVP): ruff, eslint, shellcheck, hadolint, markdownlint.
- `auto_review init` CLI for bot setup.
- **Verification**: stand up a dev Forgejo, bot reviews real PRs end-to-end on 3 sample repos (Rust, TS, Python).

### Milestone 2 — RAG + learnings + triage (4–6 weeks)
- `ar-index`: tree-sitter parsing for Rust/TS/Python/Go, LanceDB embeddings, co-change graph.
- Two-tier model routing (triage/cheap + reasoning).
- Learnings store with `@auto_review remember/forget` chat commands.
- Incremental commit handling with summary cache.
- Walkthrough generation + Mermaid diagrams in PR description.
- **Verification**: replay a corpus of ≥50 historical PRs; compare findings to what a human reviewer flagged; track precision/recall.

### Milestone 3 — Sandbox + full linter suite (4–6 weeks)
- `ar-sandbox` on youki: OCI bundle, seccomp profile, no-net namespace, resource limits.
- Bundle remaining ~40 linters in the container image; route all linter exec through the sandbox.
- Verification agent (LLM double-check) runs in the sandbox with `grep`/`cat`/`ast-grep` tools.
- **Verification**: red-team test suite — malicious lint configs, fork-bombs, network-egress attempts; all must be contained.

### Milestone 4 — Agentic chat + finishing-touches (4–6 weeks)
- `ar-chat`: `@auto_review` mention handler, per-thread conversation state, sandboxed tool use.
- Polling fallback for missing review-comment webhook events.
- Finishing-touches: docstring gen, autofix patches, unit-test scaffolding (all behind explicit user opt-in commands).
- Optional CI/static-analysis integrations for project-specific merge checks.
- **Verification**: dogfood on `auto_review`'s own PRs.

### Milestone 5 — Polish, docs, release (2–4 weeks)
- Forgejo Action packaging.
- `docker compose` deploy template, Helm chart.
- Quality benchmark suite + leaderboard against PR-Agent on the same corpus.
- Public release + announcement on Codeberg + r/selfhosted + lobste.rs.

**Total: 18–28 weeks (~5–7 months)** for one focused FTE-equivalent.

---

## 12. Cost Model (per-PR, cloud profile)

Borrowing community benchmarks: PR-Agent runs $0.02–$0.10/PR on GPT-4-class models; CodeRabbit-style multi-pass with verification is 2–3× that. Expect **$0.05–$0.30/PR** on a Haiku-triage + Sonnet-reasoning profile, **$0** for fully-local Ollama profile (CPU/GPU cost only). Per-PR latency target: **<3 minutes** for typical PRs (≤500 LoC), <10 minutes for huge ones.

---

## 13. Critical Files to Create First

These bootstrap the workspace and lock the architecture in place. Other crates can stub.

- `Cargo.toml` (workspace root with member list and shared deps)
- `crates/ar-gateway/src/main.rs` (axum server, `/webhooks/forgejo` route, HMAC middleware)
- `crates/ar-forgejo/src/lib.rs` (`Client`, `get_diff`, `get_files`, `post_review`, `post_status`)
- `crates/ar-llm/src/lib.rs` (`LlmProvider` trait + `OpenAi` impl)
- `crates/ar-orchestrator/src/state.rs` (state machine enum + transition fn + sqlx persistence)
- `crates/ar-prompts/templates/review.jinja` + `crates/ar-prompts/schema/review.json`
- `deploy/Dockerfile` (multi-stage; final stage installs ruff/eslint/shellcheck/hadolint/markdownlint)
- `deploy/docker-compose.yml` (gateway + Postgres + LanceDB volume)
- `docs/ADR-0001-architecture.md`

Existing patterns to lift verbatim (with attribution):
- **PR-Agent** prompts (Apache-2.0): `pr_reviewer_prompts.toml`, the `__new hunk__`/`__old hunk__` diff format, the Pydantic-ish JSON output schema. Source: github.com/qodo-ai/pr-agent.
- **CodeRabbit's** triage→summarize→review→verify pipeline shape (no code, just architecture per their public blog).
- **AuditLM** (Forgejo-native Rust reviewer, 31 stars) for any usable Forgejo-client patterns; license check needed before lifting code.

---

## 14. End-to-End Verification

1. **Local dev loop**: `docker compose up forgejo + auto_review`; create a test repo + PR; verify webhook fires, review posts, status updates. Repeat with a synced commit; verify incremental review.
2. **Quality benchmark**: corpus of 50+ historical PRs from real repos with known-good review feedback (from human reviewers or CodeRabbit's own past comments where public). Score precision/recall. Track per-milestone.
3. **Security**: red-team suite (malicious linter configs, fork-bombs, egress attempts, prompt-injection in PR body that tries to escape the sandbox). All must be neutralized.
4. **Forgejo compatibility matrix**: test against Forgejo `7.x`, `8.x`, `9.x`; Gitea `1.22+` if we want broader reach. Document known-good versions.
5. **Local-LLM-only profile**: full review pipeline must complete on a workstation with Ollama + a 32B-class model and no internet. This is the existence proof for sovereignty users.

---

## 15. What This Plan Does Not Cover

- **Multi-tenant SaaS**: explicitly out of scope per the deployment-mode decision. If/when it's revisited, the auth model needs an "App-style" identity layer (likely faked via a dedicated bot user per customer + a delegated-OAuth dance).
- **GitLab / Bitbucket support**: orthogonal to Forgejo focus; the `ar-forgejo` crate could be paralleled later by `ar-gitlab` etc. with the same orchestrator core.
- **GUI / dashboard**: not for v1. CLI-and-config-file only. A web UI for browsing past reviews / tuning learnings is a candidate for milestone 5+.
- **Fine-tuned models**: sticking with off-the-shelf. Custom training is deferred.
